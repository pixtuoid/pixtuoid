//! A watch root's attach state, reconciled against the filesystem so the root
//! is watched exactly while it exists. No notify backend can watch an absent
//! path, and inotify and Windows drop a watch whose directory is deleted, so a
//! second, non-recursive watch sits on the root's nearest existing ancestor —
//! notify's advice for a watched path that may be removed (`Watcher::watch`'s
//! docs). Its events only wake [`RootWatch::reconcile`], which re-reads the
//! filesystem, so a lost or coalesced event can't leave the watch stale.

use std::io;
use std::path::{Path, PathBuf};

use notify::event::{EventKind, ModifyKind};
use notify::{Config, EventHandler, PollWatcher, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tracing::{info, warn};

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
    /// (events may have been lost). An open, a content or a metadata write
    /// can't, and an anchor such as `$HOME` sees a stream of them — inotify
    /// reports the reconcile's own opens, so nudging on those would loop.
    pub(crate) fn on_event(&self, res: &notify::Result<notify::Event>) {
        let structural = match res {
            Ok(event) => !matches!(
                event.kind,
                EventKind::Access(_)
                    | EventKind::Modify(ModifyKind::Data(_) | ModifyKind::Metadata(_))
                    // Windows' FILE_ACTION_MODIFIED: a write, never a new entry.
                    | EventKind::Modify(ModifyKind::Any)
            ),
            Err(_) => true,
        };
        if structural {
            self.send();
        }
    }
}

/// What woke a reconcile. A nudge leaves an attach that failed on the same
/// directory to the poll, so a persistent failure (inotify's watch limit on a
/// big tree) isn't re-walked on every event of a busy anchor.
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
    Unchanged,
}

/// Which directory a path named. Equal ids are the same directory, so a root
/// deleted and recreated between two reconciles reads as replaced. On Unix
/// the open handle pins the inode, so the recreation can't reuse its number;
/// NTFS ids carry a reuse sequence number instead, and an open Windows handle
/// would keep a deleted directory's name from being recreated.
#[derive(Debug)]
struct DirId {
    key: (u64, u64),
    #[cfg(unix)]
    _pin: std::fs::File,
}

impl PartialEq for DirId {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}

/// `None` when nothing is at `path`.
fn dir_id(path: &Path) -> io::Result<Option<DirId>> {
    match open_dir_id(path) {
        Ok(id) => Ok(Some(id)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(unix)]
fn open_dir_id(path: &Path) -> io::Result<DirId> {
    use std::os::unix::fs::MetadataExt;
    let pin = open_for_events(path)?;
    let meta = pin.metadata()?;
    Ok(DirId {
        key: (meta.dev(), meta.ino()),
        _pin: pin,
    })
}

#[cfg(windows)]
fn open_dir_id(path: &Path) -> io::Result<DirId> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, GetFileInformationByHandle,
    };
    // A directory opens only with backup semantics; reading its id needs no
    // access right.
    let dir = std::fs::OpenOptions::new()
        .access_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    let mut info = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
    // SAFETY: `dir` is an open handle for the whole call, and `info` is
    // writable storage of the struct the call fills.
    if unsafe { GetFileInformationByHandle(dir.as_raw_handle(), info.as_mut_ptr()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a nonzero return filled `info`.
    let info = unsafe { info.assume_init() };
    Ok(DirId {
        key: (
            u64::from(info.dwVolumeSerialNumber),
            (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        ),
    })
}

/// Opens a directory only to identify or watch it: `O_EVTONLY` on macOS, so
/// the handle never blocks an unmount of its volume.
#[cfg(unix)]
fn open_for_events(path: &Path) -> io::Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(target_os = "macos")]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut opts, libc::O_EVTONLY);
    opts.open(path)
}

async fn identify(path: &Path) -> io::Result<Option<DirId>> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || dir_id(&path))
        .await
        .map_err(io::Error::other)?
}

/// The deepest existing proper ancestor of `root`, short of a filesystem root:
/// anchored at `/`, every change at the top of the disk would wake it, so a
/// root with no other existing ancestor is left to the poll.
async fn find_anchor(root: &Path) -> Option<(PathBuf, DirId)> {
    let root = root.to_path_buf();
    tokio::task::spawn_blocking(move || {
        root.ancestors()
            .skip(1)
            .take_while(|a| a.parent().is_some())
            .find_map(|a| Some((a.to_path_buf(), dir_id(a).ok()??)))
    })
    .await
    .ok()
    .flatten()
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

/// Builds the watcher a [`RootWatch`] puts on one directory.
pub(super) type BuildWatcher = Box<dyn Fn() -> notify::Result<Box<dyn Watcher + Send>> + Send>;

/// A watcher on one directory, alive exactly as long as that watch: notify's
/// backends key their watches by path, so re-watching a recreated directory
/// on a kept watcher lets the dead one's late removal (inotify's
/// `DELETE_SELF`, Windows' silent unwatch) take the new watch with it.
struct Watch {
    _watcher: Box<dyn Watcher + Send>,
    path: PathBuf,
    id: DirId,
}

impl Watch {
    fn start(
        build: &BuildWatcher,
        path: &Path,
        mode: RecursiveMode,
    ) -> notify::Result<Box<dyn Watcher + Send>> {
        let mut watcher = build()?;
        watcher.watch(path, mode)?;
        Ok(watcher)
    }

    fn is_on(&self, path: &Path, id: &DirId) -> bool {
        self.path == path && self.id == *id
    }
}

/// One root's watch plus the anchor that reports the root's appearance and
/// removal. Nothing is watched until the first [`Self::reconcile`].
pub(crate) struct RootWatch {
    root: PathBuf,
    mode: RecursiveMode,
    build_root: BuildWatcher,
    build_anchor: BuildWatcher,
    nudge: Nudge,
    attached: Option<Watch>,
    /// The directory whose attach last failed, left to [`Wake::Poll`].
    failed: Option<DirId>,
    anchor: Option<Watch>,
    root_health: FailureLatch,
    anchor_health: FailureLatch,
}

impl RootWatch {
    /// `root_events` receives the root watch's events; the anchor's only wake
    /// `nudge`'s receiver. Two watchers rather than one, so an ancestor's own
    /// files (CC's `history.jsonl` beside its projects root) never reach
    /// `root_events`.
    pub(crate) fn new<H: EventHandler + Clone>(
        root: PathBuf,
        mode: RecursiveMode,
        root_events: H,
        nudge: Nudge,
    ) -> Self {
        let build_root: BuildWatcher = Box::new(move || match mode {
            RecursiveMode::Recursive => tree_watcher(root_events.clone()),
            RecursiveMode::NonRecursive => dir_watcher(root_events.clone()),
        });
        let anchor_nudge = nudge.clone();
        let build_anchor: BuildWatcher = Box::new(move || {
            let nudge = anchor_nudge.clone();
            dir_watcher(move |res: notify::Result<notify::Event>| nudge.on_event(&res))
        });
        Self::with_builders(root, mode, build_root, build_anchor, nudge)
    }

    pub(super) fn with_builders(
        root: PathBuf,
        mode: RecursiveMode,
        build_root: BuildWatcher,
        build_anchor: BuildWatcher,
        nudge: Nudge,
    ) -> Self {
        Self {
            root,
            mode,
            build_root,
            build_anchor,
            nudge,
            attached: None,
            failed: None,
            anchor: None,
            root_health: FailureLatch::default(),
            anchor_health: FailureLatch::default(),
        }
    }

    pub(crate) fn is_attached(&self) -> bool {
        self.attached.is_some()
    }

    /// Brings both watches in line with the filesystem: the anchor on the
    /// root's deepest existing ancestor, the root watch on the directory the
    /// root names now.
    ///
    /// # Errors
    ///
    /// The root could not be stat'ed or watched. An anchor failure only warns:
    /// the poll still finds the root.
    pub(crate) async fn reconcile(&mut self, wake: Wake) -> anyhow::Result<Reconciled> {
        if wake == Wake::Poll {
            self.failed = None;
        }
        self.reanchor().await;
        // Read only once the anchor is watched, so a root created or replaced
        // after this read wakes the next reconcile.
        let Some(id) = identify(&self.root).await? else {
            self.attached = None;
            self.failed = None;
            return Ok(Reconciled::Unchanged);
        };
        if self
            .attached
            .as_ref()
            .is_some_and(|w| w.is_on(&self.root, &id))
            || self.failed.as_ref() == Some(&id)
        {
            return Ok(Reconciled::Unchanged);
        }
        self.attached = None;
        match Watch::start(&self.build_root, &self.root, self.mode) {
            Ok(watcher) => {
                self.attached = Some(Watch {
                    _watcher: watcher,
                    path: self.root.clone(),
                    id,
                });
                Ok(Reconciled::Attached)
            }
            // Removed mid-attach: the anchor reports it.
            Err(_) if identify(&self.root).await?.is_none() => Ok(Reconciled::Unchanged),
            Err(e) => {
                self.failed = Some(id);
                Err(e.into())
            }
        }
    }

    /// [`Self::reconcile`] after boot, where a failure must not end the source:
    /// warned once while it persists.
    pub(crate) async fn reconcile_logged(&mut self, wake: Wake) -> Reconciled {
        match self.reconcile(wake).await {
            Ok(reconciled) => {
                if self.failed.is_none() && self.root_health.on_success() {
                    info!(root = ?self.root, "watch root attached after a failure");
                }
                reconciled
            }
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

    async fn reanchor(&mut self) {
        let want = find_anchor(&self.root).await;
        let unchanged = match (&want, &self.anchor) {
            (Some((path, id)), Some(anchor)) => anchor.is_on(path, id),
            (None, None) => true,
            _ => false,
        };
        if unchanged {
            return;
        }
        self.anchor = None;
        let Some((path, id)) = want else {
            return;
        };
        match Watch::start(&self.build_anchor, &path, RecursiveMode::NonRecursive) {
            Ok(watcher) => {
                if self.anchor_health.on_success() {
                    info!(root = ?self.root, anchor = ?path, "watch anchor attached after a failure");
                }
                self.anchor = Some(Watch {
                    _watcher: watcher,
                    path,
                    id,
                });
            }
            // Removed since the walk, and nothing else would wake a re-walk.
            Err(_) if matches!(identify(&path).await, Ok(None)) => self.nudge.send(),
            Err(e) => {
                if self.anchor_health.on_failure() {
                    warn!(
                        root = ?self.root,
                        anchor = ?path,
                        error = ?e,
                        "watch anchor could not be attached; the root's appearance waits for the poll"
                    );
                }
            }
        }
    }
}
