//! A watch root's attach state, reconciled against the filesystem so the root
//! is watched exactly while it exists. No notify backend can watch an absent
//! path, and inotify and Windows drop a watch whose directory is deleted, so a
//! non-recursive watch also sits on the root's nearest existing ancestor —
//! notify's advice for a watched path that may be removed (`Watcher::watch`'s
//! docs). Its events only wake [`RootWatch::reconcile`], which re-reads the
//! filesystem, so a lost or coalesced event can't leave the watch stale.

use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use notify::event::{EventKind, ModifyKind};
use notify::{Config, EventHandler, PollWatcher, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::health::FailureLatch;

#[cfg(target_os = "macos")]
mod kqueue;

/// Wakes a [`RootWatch`]'s owner. One slot: a full channel already holds a
/// pending wake, and a reconcile reads the present filesystem, so a dropped
/// nudge loses nothing.
#[derive(Clone, Debug)]
pub(crate) struct Nudge(mpsc::Sender<()>);

impl Nudge {
    pub(crate) fn channel() -> (Self, mpsc::Receiver<()>) {
        let (tx, rx) = mpsc::channel(1);
        (Self(tx), rx)
    }

    pub(crate) fn send(&self) {
        let _ = self.0.try_send(());
    }

    /// Nudges on an event that can change which directories exist, or an error
    /// (events may have been lost).
    pub(crate) fn on_event(&self, res: &notify::Result<notify::Event>) {
        match res {
            Ok(event) => self.on_kind(&event.kind),
            Err(_) => self.send(),
        }
    }

    /// An open, a content or a metadata write can't change which directories
    /// exist, and an anchor such as `$HOME` sees a stream of them — inotify
    /// reports the reconcile's own opens, so nudging on those would loop.
    fn on_kind(&self, kind: &EventKind) {
        let structural = !matches!(
            kind,
            EventKind::Access(_)
                | EventKind::Modify(ModifyKind::Data(_) | ModifyKind::Metadata(_))
                // Windows' FILE_ACTION_MODIFIED: a write, never a new entry.
                | EventKind::Modify(ModifyKind::Any)
        );
        if structural {
            self.send();
        }
    }
}

/// What woke a reconcile. A nudge leaves watches that failed on the same
/// directories to the poll, so a persistent failure (inotify's watch limit on
/// a big tree) isn't re-walked on every event of a busy anchor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Wake {
    Nudge,
    Poll,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reconciled {
    /// The root watch was (re)placed: files written before it existed are
    /// the caller's to scan.
    Attached,
    /// The root went away, and its watch with it.
    Detached,
    Unchanged,
}

/// Which directory a path named. The birth time joins the id because an id
/// alone can name a recreated directory: a freed inode number is reused, and
/// Windows file ids are "not guaranteed to be unique over time", nor on ReFS
/// (`BY_HANDLE_FILE_INFORMATION`'s docs).
#[derive(Clone, Debug, PartialEq)]
struct DirId {
    volume: u64,
    index: u64,
    born: Option<SystemTime>,
}

/// `None` when nothing is at `path`.
fn dir_id(path: &Path) -> io::Result<Option<DirId>> {
    match read_dir_id(path) {
        Ok(id) => Ok(Some(id)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(unix)]
fn read_dir_id(path: &Path) -> io::Result<DirId> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path)?;
    Ok(DirId {
        volume: meta.dev(),
        index: meta.ino(),
        born: meta.created().ok(),
    })
}

#[cfg(windows)]
fn read_dir_id(path: &Path) -> io::Result<DirId> {
    let dir = winapi_util::Handle::from_path_any(path)?;
    let info = winapi_util::file::information(&dir)?;
    Ok(DirId {
        volume: info.volume_serial_number(),
        index: info.file_index(),
        born: dir.as_file().metadata()?.created().ok(),
    })
}

async fn identify(path: &Path) -> io::Result<Option<DirId>> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || dir_id(&path))
        .await
        .map_err(io::Error::other)?
}

/// Where a root's watches belong: on the root's directory while it exists,
/// and on its deepest existing proper ancestor short of a filesystem root —
/// anchored at `/`, every change at the top of the disk would wake it, so a
/// root with no other existing ancestor is left to the poll.
#[derive(Clone, Debug, Default, PartialEq)]
struct Placement {
    root: Option<DirId>,
    anchor: Option<(PathBuf, DirId)>,
}

async fn read_placement(root: &Path) -> io::Result<Placement> {
    let root = root.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let anchor = root
            .ancestors()
            .skip(1)
            .take_while(|a| a.parent().is_some())
            .find_map(|a| Some((a.to_path_buf(), dir_id(a).ok()??)));
        Ok(Placement {
            root: dir_id(&root)?,
            anchor,
        })
    })
    .await
    .map_err(io::Error::other)?
}

/// The one spelling of a watch root, taken where a root enters the watching:
/// notify makes a relative watch path absolute against the cwd, and its events
/// carry that spelling (notify 8.2.0 `inotify.rs:547-551`,
/// `windows.rs:530-534`), so every other consumer must hold it too.
pub(crate) fn absolute_root(root: PathBuf) -> PathBuf {
    std::path::absolute(&root).unwrap_or(root)
}

/// Builds a watcher the watches go on.
pub(super) type BuildWatcher = Box<dyn Fn() -> notify::Result<Box<dyn Watcher + Send>> + Send>;

/// Builds a [`Backend::Shared`] watcher, routing by the root it is passed.
pub(super) type BuildShared = Box<dyn Fn(&Path) -> notify::Result<Box<dyn Watcher + Send>> + Send>;

/// How a [`Placement`]'s watches are hosted. Each placement gets fresh
/// watchers: notify's backends key watches by path, so re-watching a recreated
/// directory on a kept watcher lets the dead one's late removal (inotify's
/// `DELETE_SELF`, Windows' silent unwatch) take the new watch with it.
pub(super) enum Backend {
    /// Both watches on one watcher, its events routed by [`Routed`]: one
    /// inotify instance per root, against `inotify(7)`'s per-user
    /// `max_user_instances` (<https://man7.org/linux/man-pages/man7/inotify.7.html>).
    Shared(BuildShared),
    /// Each watch on its own watcher: a macOS anchor is a kqueue one, and a
    /// polled test root can't host a native anchor.
    Split {
        root: BuildWatcher,
        anchor: BuildWatcher,
    },
}

struct Started {
    watchers: Vec<Box<dyn Watcher + Send>>,
    root: notify::Result<()>,
    anchor: notify::Result<()>,
}

impl Backend {
    /// The root is watched before the anchor, so an anchor that can't be
    /// watched never costs the root its watch.
    fn start(
        &self,
        root_path: &Path,
        root: Option<RecursiveMode>,
        anchor: Option<&Path>,
    ) -> Started {
        let root = root.map(|mode| (root_path, mode));
        let mut watchers = Vec::new();
        let (root, anchor) = match self {
            Self::Shared(build) => match build(root_path) {
                Ok(mut watcher) => {
                    let root = root.map_or(Ok(()), |(path, mode)| watcher.watch(path, mode));
                    let anchor = anchor.map_or(Ok(()), |path| {
                        watcher.watch(path, RecursiveMode::NonRecursive)
                    });
                    watchers.push(watcher);
                    (root, anchor)
                }
                // One watcher failed both: the root's copy keeps the error.
                Err(e) => match root {
                    Some(_) => (
                        Err(e),
                        Err(notify::Error::generic("watcher creation failed")),
                    ),
                    None => (Ok(()), anchor.map_or(Ok(()), |_| Err(e))),
                },
            },
            Self::Split {
                root: build_root,
                anchor: build_anchor,
            } => {
                let mut start = |build: &BuildWatcher,
                                 path: &Path,
                                 mode: RecursiveMode|
                 -> notify::Result<()> {
                    let mut watcher = build()?;
                    watcher.watch(path, mode)?;
                    watchers.push(watcher);
                    Ok(())
                };
                let root = root.map_or(Ok(()), |(path, mode)| start(build_root, path, mode));
                let anchor = anchor.map_or(Ok(()), |path| {
                    start(build_anchor, path, RecursiveMode::NonRecursive)
                });
                (root, anchor)
            }
        };
        Started {
            watchers,
            root,
            anchor,
        }
    }
}

/// A shared watcher's events, routed by path: those strictly inside the root
/// are the root's; the rest — the root itself, the anchor and its other
/// entries, such as CC's `history.jsonl` beside its projects root — only
/// nudge. Exact on inotify and Windows: their events join the watched path,
/// spelled as [`absolute_root`] spells every root.
#[derive(Clone)]
pub(super) struct Routed<H> {
    pub(super) root: PathBuf,
    pub(super) root_events: H,
    pub(super) nudge: Nudge,
}

impl<H: EventHandler> EventHandler for Routed<H> {
    fn handle_event(&mut self, res: notify::Result<notify::Event>) {
        let mut event = match res {
            Ok(event) => event,
            Err(e) => {
                self.nudge.send();
                self.root_events.handle_event(Err(e));
                return;
            }
        };
        let (inside, outside): (Vec<_>, Vec<_>) = std::mem::take(&mut event.paths)
            .into_iter()
            .partition(|p| *p != self.root && p.starts_with(&self.root));
        // A pathless event is a rescan: events were lost.
        if !outside.is_empty() || inside.is_empty() {
            self.nudge.on_kind(&event.kind);
        }
        if !inside.is_empty() {
            event.paths = inside;
            self.root_events.handle_event(Ok(event));
        }
    }
}

fn tree_watcher<H: EventHandler>(handler: H) -> notify::Result<Box<dyn Watcher + Send>> {
    Ok(match super::TEST_POLL_OVERRIDE.get().copied() {
        // `with_compare_contents` detects changes by hashing file contents, not
        // just mtime/size, so truncate-rewrites are caught reliably.
        Some(interval) => Box::new(PollWatcher::new(
            handler,
            Config::default()
                .with_poll_interval(interval)
                .with_compare_contents(true),
        )?),
        None => Box::new(RecommendedWatcher::new(handler, Config::default())?),
    })
}

/// A watcher of one directory's entries. Not notify's on macOS, where an anchor
/// can be `$HOME`: its FSEvents stream would span the subtree, ~/Documents
/// included, and its kqueue backend (built only without FSEvents, notify's
/// `lib.rs`) recursively watches each new child.
fn dir_watcher<H: EventHandler>(handler: H) -> notify::Result<Box<dyn Watcher + Send>> {
    #[cfg(target_os = "macos")]
    let watcher = kqueue::KqueueDirWatcher::new(handler, Config::default())?;
    #[cfg(not(target_os = "macos"))]
    let watcher = RecommendedWatcher::new(handler, Config::default())?;
    Ok(Box::new(watcher))
}

/// One root's watch plus the anchor that reports the root's appearance and
/// removal. Nothing is watched until the first [`Self::reconcile`].
pub(crate) struct RootWatch {
    root: PathBuf,
    mode: RecursiveMode,
    backend: Backend,
    nudge: Nudge,
    /// What `watchers` were started for.
    placed: Placement,
    watchers: Vec<Box<dyn Watcher + Send>>,
    root_watched: bool,
    /// A watch of `placed` failed: retried only by a [`Wake::Poll`].
    broken: bool,
    root_health: FailureLatch,
    anchor_health: FailureLatch,
}

impl RootWatch {
    /// `root` arrives spelled by [`absolute_root`]. `root_events` receives the
    /// root watch's events; the anchor's only wake `nudge`'s receiver, so an
    /// ancestor's own files never reach `root_events`.
    pub(crate) fn new<H: EventHandler + Clone>(
        root: PathBuf,
        mode: RecursiveMode,
        root_events: H,
        nudge: Nudge,
    ) -> Self {
        let backend = if cfg!(target_os = "macos") || super::TEST_POLL_OVERRIDE.get().is_some() {
            let anchor_nudge = nudge.clone();
            Backend::Split {
                root: Box::new(move || match mode {
                    RecursiveMode::Recursive => tree_watcher(root_events.clone()),
                    RecursiveMode::NonRecursive => dir_watcher(root_events.clone()),
                }),
                anchor: Box::new(move || {
                    let nudge = anchor_nudge.clone();
                    dir_watcher(move |res: notify::Result<notify::Event>| nudge.on_event(&res))
                }),
            }
        } else {
            let nudge = nudge.clone();
            Backend::Shared(Box::new(move |root: &Path| {
                let routed = Routed {
                    root: root.to_path_buf(),
                    root_events: root_events.clone(),
                    nudge: nudge.clone(),
                };
                Ok(Box::new(RecommendedWatcher::new(
                    routed,
                    Config::default(),
                )?))
            }))
        };
        Self::with_backend(root, mode, backend, nudge)
    }

    pub(super) fn with_backend(
        root: PathBuf,
        mode: RecursiveMode,
        backend: Backend,
        nudge: Nudge,
    ) -> Self {
        debug_assert!(root.is_absolute(), "{root:?} skipped `absolute_root`");
        Self {
            root,
            mode,
            backend,
            nudge,
            placed: Placement::default(),
            watchers: Vec::new(),
            root_watched: false,
            broken: false,
            root_health: FailureLatch::default(),
            anchor_health: FailureLatch::default(),
        }
    }

    /// Brings the watches in line with the filesystem: the anchor on the root's
    /// deepest existing ancestor, the root watch on the directory the root
    /// names now. It re-reads until a read finds what the last one placed, so
    /// a directory created while the watches moved still wakes a reconcile.
    ///
    /// # Errors
    ///
    /// The root could not be read, or exists and could not be watched. An
    /// anchor failure only warns: the poll still finds the root.
    pub(crate) async fn reconcile(&mut self, wake: Wake) -> anyhow::Result<Reconciled> {
        let mut retry = wake == Wake::Poll && self.broken;
        let mut outcome = Reconciled::Unchanged;
        // A round per level the anchor can move; a chain still changing after
        // that is left to the next wake.
        for _ in 0..self.root.ancestors().count() {
            let now = read_placement(&self.root).await?;
            if now == self.placed && !retry {
                return Ok(outcome);
            }
            retry = false;
            let had_root = self.root_watched;
            self.watchers.clear();
            let started = self.backend.start(
                &self.root,
                now.root.as_ref().map(|_| self.mode),
                now.anchor.as_ref().map(|(path, _)| path.as_path()),
            );
            self.watchers = started.watchers;
            self.placed = now;
            self.root_watched = self.placed.root.is_some() && started.root.is_ok();
            self.broken = false;
            if let (Err(e), Some((path, _))) = (started.anchor, &self.placed.anchor) {
                // Removed since the read: the next round moves the anchor.
                if identify(path).await?.is_some() {
                    self.broken = true;
                    if self.anchor_health.on_failure() {
                        warn!(
                            root = ?self.root,
                            anchor = ?path,
                            error = ?e,
                            "watch anchor could not be attached; the root's appearance waits for the poll"
                        );
                    }
                }
            } else if self.placed.anchor.is_some() && self.anchor_health.on_success() {
                info!(root = ?self.root, "watch anchor attached after a failure");
            }
            if let Err(e) = started.root {
                // Removed since the read: the next round detaches it.
                if identify(&self.root).await?.is_some() {
                    self.broken = true;
                    return Err(e.into());
                }
            }
            if self.root_watched {
                outcome = Reconciled::Attached;
            } else if had_root {
                outcome = Reconciled::Detached;
            }
        }
        self.nudge.send();
        Ok(outcome)
    }

    /// [`Self::reconcile`] after boot, where a failure must not end the source:
    /// warned once while it persists.
    pub(crate) async fn reconcile_logged(&mut self, wake: Wake) -> Reconciled {
        match self.reconcile(wake).await {
            Ok(Reconciled::Attached) => {
                if self.root_health.on_success() {
                    info!(root = ?self.root, "watch root attached after a failure");
                }
                Reconciled::Attached
            }
            Ok(Reconciled::Detached) => {
                self.root_health.on_success();
                debug!(root = ?self.root, "watch root removed; its watch detached");
                Reconciled::Detached
            }
            Ok(Reconciled::Unchanged) => Reconciled::Unchanged,
            Err(e) => {
                if self.root_health.on_failure() {
                    warn!(
                        root = ?self.root,
                        error = ?e,
                        "watch root could not be attached; the poll retries it"
                    );
                }
                Reconciled::Unchanged
            }
        }
    }
}
