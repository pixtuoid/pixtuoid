use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Semaphore;
use tracing::warn;

use crate::source::jsonl::FailureLatch;

use super::{CONN_TIMEOUT, MAX_CONCURRENT_CONNS};

/// First retry delay after an accept() error. mio/tokio only clear readiness on
/// EWOULDBLOCK, so a persistent accept errno (the EMFILE class) returns
/// Ready(Err) on every await — an unthrottled retry is a 100% CPU spin.
const ACCEPT_BACKOFF_FIRST: Duration = Duration::from_millis(100);
/// Backoff ceiling: fd pressure can persist for minutes, but the daemon must
/// pick pending shim connections up promptly once fds free.
const ACCEPT_BACKOFF_MAX: Duration = Duration::from_secs(5);

struct AcceptBackoff {
    next: Duration,
}

impl Default for AcceptBackoff {
    fn default() -> Self {
        Self {
            next: ACCEPT_BACKOFF_FIRST,
        }
    }
}

impl AcceptBackoff {
    fn on_error(&mut self) -> Duration {
        let delay = self.next;
        self.next = (self.next * 2).min(ACCEPT_BACKOFF_MAX);
        delay
    }

    fn on_success(&mut self) {
        self.next = ACCEPT_BACKOFF_FIRST;
    }
}

/// Ensure the socket's parent is a private, us-owned `0700` directory BEFORE the
/// bind opens the lock / socket inside it — but ONLY for the `/tmp/pixtuoid-{uid}/`
/// no-XDG fallback we manage (#485). `XDG_RUNTIME_DIR` is systemd-managed 0700 and
/// an explicit `PIXTUOID_SOCKET` parent is the user's choice — neither is ours to
/// create or police, so both are left untouched.
///
/// TOCTOU-safe: `mkdir(0700)` is atomic create-if-absent, and `/tmp`'s sticky bit
/// stops another uid swapping the validated `lstat` result before the lock open.
///
/// Excluded by function in `.cargo/mutants.toml` — driving it means
/// creating/validating the REAL per-user socket dir, which would race a live
/// daemon. Keep it a pure substitution into [`ensure_owned_socket_dir_in`]: any
/// logic added HERE becomes unmeasurable.
#[cfg(unix)]
fn ensure_owned_socket_dir(path: &Path) -> Result<()> {
    ensure_owned_socket_dir_in(
        path,
        &owned_socket_dir(),
        rustix::process::getuid().as_raw(),
    )
}

/// Harden `owned` only when `path` actually lives inside it, else no-op.
/// `owned` + `uid` are parameters so the FIRES direction is reachable from a
/// test without touching the real per-user socket dir.
#[cfg(unix)]
fn ensure_owned_socket_dir_in(path: &Path, owned: &Path, uid: u32) -> Result<()> {
    if !is_owned_fallback_in(path, owned) {
        return Ok(());
    }
    ensure_private_dir(owned, uid)
}

/// The socket file name inside [`owned_socket_dir`]. Shared with
/// `ClaudeCodeSource::default_socket_path` so the endpoint and the guard below
/// are built from the same two pieces.
#[cfg(unix)]
pub(crate) const SOCKET_FILE_NAME: &str = "pixtuoid.sock";

/// THE definition of the no-XDG `/tmp` fallback directory (#485): both the
/// socket path and the ownership guard below derive from this one fn, so a
/// second hand-copied literal can't silently disarm the guard. (The SHIM keeps
/// its own copy in `pixtuoid-hook/src/paths.rs` — no dep edge is allowed between
/// the two crates — pinned to this one by `tests/socket_path_parity.rs`.)
#[cfg(unix)]
pub(crate) fn owned_socket_dir() -> std::path::PathBuf {
    let uid = rustix::process::getuid().as_raw();
    std::path::PathBuf::from(format!("/tmp/pixtuoid-{uid}"))
}

/// Whether `path` is the socket endpoint directly inside `owned`.
#[cfg(unix)]
fn is_owned_fallback_in(path: &Path, owned: &Path) -> bool {
    path.parent() == Some(owned)
}

/// Create `dir` as a `0700` directory owned by `uid`, or — if it already exists —
/// validate it IS one, else error.
#[cfg(unix)]
fn ensure_private_dir(dir: &Path, uid: u32) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // lstat (never follow): a planted symlink is caught as non-dir.
            let md = std::fs::symlink_metadata(dir)
                .with_context(|| format!("stat-ing hook socket dir {}", dir.display()))?;
            if !md.file_type().is_dir() {
                anyhow::bail!(
                    "hook socket dir {} exists but is not a directory (hostile squat) — \
                     refusing to bind",
                    dir.display()
                );
            }
            if md.uid() != uid {
                anyhow::bail!(
                    "hook socket dir {} is owned by uid {} not {} (hostile squat) — \
                     refusing to bind",
                    dir.display(),
                    md.uid(),
                    uid
                );
            }
            if md.mode() & 0o077 != 0 {
                anyhow::bail!(
                    "hook socket dir {} is group/other-accessible (mode {:o}) — \
                     refusing to bind",
                    dir.display(),
                    md.mode() & 0o7777
                );
            }
            Ok(())
        }
        Err(e) => Err(e).with_context(|| format!("creating hook socket dir {}", dir.display())),
    }
}

/// A sibling of the FINAL socket path, named `<sock>.<suffix>`. Both siblings
/// `bind` derives — the arbitration lock and the pre-rename temp endpoint — MUST
/// be built from the same base, because both bind branches have to arbitrate on
/// one lock file.
#[cfg(unix)]
fn socket_sibling(path: &Path, suffix: &str) -> std::path::PathBuf {
    path.with_file_name(format!(
        "{}.{suffix}",
        path.file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default()
    ))
}

/// `sockaddr_un`'s `sun_path` capacity, NUL included — [unix(7)] fixes the
/// field per platform, so it is read off the struct, never restated.
///
/// [unix(7)]: https://man7.org/linux/man-pages/man7/unix.7.html
const SUN_PATH_CAP: usize =
    std::mem::size_of::<libc::sockaddr_un>() - std::mem::offset_of!(libc::sockaddr_un, sun_path);

/// Arbitrate the bind on `<path>.lock`: harden the owned fallback dir, then take
/// the exclusive lock, or [`super::SocketBusy`]. Synchronous file IO, so
/// [`Listener::bind`] runs it on tokio's blocking pool.
fn acquire_bind_lock(path: &Path) -> Result<std::fs::File> {
    ensure_owned_socket_dir(path)?;
    // An EXCLUSIVE advisory lock on a sibling `<sock>.lock`, NOT connect()
    // errnos: a backlog-saturated LIVE daemon yields ECONNREFUSED on macOS and
    // EAGAIN on Linux, so an errno-guessing probe unlinks a live socket and
    // leaves it accepting on an anonymous inode forever. Never unlinked —
    // unlock-then-unlink lets a waiter on the old inode and a newcomer on a
    // fresh one both "hold" it — and derived from the FINAL path so both bind
    // branches arbitrate on the same file.
    let lock_path = socket_sibling(path, "lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        // O_NOFOLLOW: the parent dir may be a shared /tmp, and a symlink planted
        // at `<sock>.lock` would flock an arbitrary file for the daemon's life.
        .custom_flags(libc::O_NOFOLLOW)
        .open(&lock_path)
        .with_context(|| format!("opening hook socket lock at {}", lock_path.display()))?;
    match lock.try_lock() {
        Ok(()) => Ok(lock),
        Err(std::fs::TryLockError::WouldBlock) => {
            // Typed so the CC source degrades to transcript-only rather than
            // dying; of two racing starts exactly one acquires the lock, so the
            // loser leaves no anonymous listener.
            Err(anyhow::Error::new(super::SocketBusy {
                path: path.to_path_buf(),
            }))
        }
        Err(std::fs::TryLockError::Error(e)) => {
            Err(e).with_context(|| format!("locking hook socket at {}", lock_path.display()))
        }
    }
}

/// Whether `tmp` fits `sun_path` with its NUL, so the bind can go through it.
fn binds_via_temp(tmp: &Path) -> bool {
    tmp.as_os_str().len() < SUN_PATH_CAP
}

/// One accepted hook connection.
pub(super) type Conn = UnixStream;

#[derive(Debug)]
pub(super) struct Listener {
    listener: UnixListener,
    // Never unlocked: the kernel releases it however abruptly the process dies, so
    // the lock — not the socket file, which nothing unlinks — is what the next
    // bind's liveness arbitration reads.
    _lock: std::fs::File,
}

impl Listener {
    pub(super) async fn bind(path: &Path) -> Result<Self> {
        let lock = {
            let path = path.to_path_buf();
            tokio::task::spawn_blocking(move || acquire_bind_lock(&path)).await??
        };
        if tokio::fs::try_exists(path).await.unwrap_or(false) {
            // Lock acquired ⇒ the previous owner is dead ⇒ the socket is residue.
            // Probe anyway: a connect that succeeds, or backlogs (WouldBlock only
            // happens on a live listener), proves an owner predating the lock
            // protocol. Any OTHER error is not evidence of life — reclaim.
            let alive = match tokio::net::UnixStream::connect(path).await {
                Ok(_stream) => true,
                Err(e) => e.kind() == std::io::ErrorKind::WouldBlock,
            };
            if alive {
                return Err(anyhow::Error::new(super::SocketBusy {
                    path: path.to_path_buf(),
                }));
            }
            let _ = tokio::fs::remove_file(path).await;
        }
        // Bind at a temp name, chmod, then rename onto the final path (which does
        // not disturb the listening inode), so the socket is never reachable there
        // looser than 0600 — and without a process-global umask, which would race
        // every other tokio worker's file creation.
        let tmp = socket_sibling(path, &format!("{}.tmp", std::process::id()));
        // A PIXTUOID_SOCKET whose FINAL path fits `sun_path` but whose
        // `.<pid>.tmp` twin does not falls back to a direct bind + chmod,
        // re-accepting the pre-chmod micro-TOCTOU.
        if !binds_via_temp(&tmp) {
            let listener = UnixListener::bind(path)
                .with_context(|| format!("binding hook socket at {}", path.display()))?;
            tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .await
                .with_context(|| format!("restricting hook socket mode at {}", path.display()))?;
            return Ok(Self {
                listener,
                _lock: lock,
            });
        }
        // A leftover temp can only be ours-by-name from a crashed prior run
        // that had this very pid — never a live socket.
        let _ = tokio::fs::remove_file(&tmp).await;
        let listener = UnixListener::bind(&tmp)
            .with_context(|| format!("binding hook socket at {}", tmp.display()))?;
        if let Err(e) =
            tokio::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).await
        {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(e)
                .with_context(|| format!("restricting hook socket mode at {}", tmp.display()));
        }
        if let Err(e) = tokio::fs::rename(&tmp, path).await {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(e).with_context(|| {
                format!(
                    "moving hook socket into place at {} (from {})",
                    path.display(),
                    tmp.display()
                )
            });
        }
        Ok(Self {
            listener,
            _lock: lock,
        })
    }

    /// Accept connections forever, each handed to `handle` under [`CONN_TIMEOUT`].
    pub(super) async fn serve<F, Fut>(self, handle: F) -> Result<()>
    where
        F: Fn(Conn) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let sem = Arc::new(Semaphore::new(MAX_CONCURRENT_CONNS));
        let mut backoff = AcceptBackoff::default();
        let mut accept_health = FailureLatch::default();
        loop {
            let permit = match Arc::clone(&sem).acquire_owned().await {
                Ok(p) => p,
                Err(_) => {
                    anyhow::bail!("hook socket semaphore closed unexpectedly");
                }
            };
            match self.listener.accept().await {
                Ok((stream, _addr)) => {
                    if accept_health.on_success() {
                        tracing::info!("hook socket accepting connections again");
                    }
                    backoff.on_success();
                    let handled = handle(stream);
                    tokio::spawn(async move {
                        let _permit = permit;
                        let _ = tokio::time::timeout(CONN_TIMEOUT, handled).await;
                    });
                }
                Err(e) => {
                    // The listener fd stays valid, so retry — but not at CPU speed
                    // and not one warn per iteration, or a persistent errno pegs a
                    // core and rotates real diagnostics out of the log.
                    if accept_health.on_failure() {
                        warn!(error = %e, "hook socket accept error (retrying with backoff)");
                    }
                    tokio::time::sleep(backoff.on_error()).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sun_path_cap_is_the_platform_s_field() {
        let want = if cfg!(target_os = "linux") { 108 } else { 104 };
        assert_eq!(SUN_PATH_CAP, want);
    }

    #[test]
    fn a_temp_path_binds_only_with_room_for_its_nul() {
        let of = |len: usize| std::path::PathBuf::from("/".repeat(len));
        assert!(binds_via_temp(&of(SUN_PATH_CAP - 1)));
        assert!(!binds_via_temp(&of(SUN_PATH_CAP)));
    }

    #[test]
    fn accept_backoff_doubles_to_the_cap_and_resets_on_success() {
        let mut b = AcceptBackoff::default();
        assert_eq!(b.on_error(), ACCEPT_BACKOFF_FIRST);
        assert_eq!(b.on_error(), ACCEPT_BACKOFF_FIRST * 2);
        let mut last = Duration::ZERO;
        for _ in 0..16 {
            last = b.on_error();
        }
        assert_eq!(
            last, ACCEPT_BACKOFF_MAX,
            "the ladder must cap, not overflow"
        );
        b.on_success();
        assert_eq!(
            b.on_error(),
            ACCEPT_BACKOFF_FIRST,
            "a successful accept must reset the ladder"
        );
    }

    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    fn my_uid() -> u32 {
        rustix::process::getuid().as_raw()
    }

    #[test]
    fn ensure_private_dir_creates_a_fresh_0700_dir_owned_by_us() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let dir = tmp.path().join("pixtuoid-sockdir");
        ensure_private_dir(&dir, my_uid()).expect("fresh create must succeed");
        let md = std::fs::symlink_metadata(&dir).expect("stat");
        assert!(md.is_dir());
        assert_eq!(md.mode() & 0o777, 0o700, "must be created private");
        assert_eq!(md.uid(), my_uid());
    }

    #[test]
    fn ensure_private_dir_accepts_an_existing_owned_0700_dir_idempotently() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let dir = tmp.path().join("d");
        ensure_private_dir(&dir, my_uid()).expect("first create");
        ensure_private_dir(&dir, my_uid()).expect("re-validate an owned 0700 dir");
    }

    #[test]
    fn ensure_private_dir_rejects_a_symlink() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let target = tmp.path().join("real");
        std::fs::create_dir(&target).expect("mkdir target");
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        assert!(
            ensure_private_dir(&link, my_uid()).is_err(),
            "a symlink at the socket dir path is hostile (lstat catches it)"
        );
    }

    /// A symlink planted at `<sock>.lock` must not get its target opened,
    /// created or locked.
    #[test]
    fn the_bind_lock_refuses_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("pixtuoid.sock");
        let lock = socket_sibling(&sock, "lock");
        let victim = dir.path().join("victim");
        std::fs::write(&victim, "theirs").unwrap();
        std::os::unix::fs::symlink(&victim, &lock).unwrap();
        assert!(acquire_bind_lock(&sock).is_err(), "a symlinked lock binds");
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "theirs");

        let dangling = dir.path().join("absent");
        std::fs::remove_file(&lock).unwrap();
        std::os::unix::fs::symlink(&dangling, &lock).unwrap();
        assert!(acquire_bind_lock(&sock).is_err());
        assert!(
            !dangling.exists(),
            "a dangling symlink's target was created"
        );
    }

    #[test]
    fn ensure_private_dir_rejects_a_regular_file() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let file = tmp.path().join("f");
        std::fs::write(&file, b"squat").expect("write file");
        assert!(
            ensure_private_dir(&file, my_uid()).is_err(),
            "a regular file squatting the dir path is hostile"
        );
    }

    #[test]
    fn ensure_private_dir_rejects_a_group_or_other_accessible_dir() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let dir = tmp.path().join("loose");
        std::fs::create_dir(&dir).expect("mkdir");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        assert!(
            ensure_private_dir(&dir, my_uid()).is_err(),
            "a world/group-accessible dir must be rejected (mode & 0o077 != 0)"
        );
    }

    #[test]
    fn ensure_private_dir_rejects_a_dir_owned_by_another_uid() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let dir = tmp.path().join("d");
        ensure_private_dir(&dir, my_uid()).expect("create as us");
        assert!(
            ensure_private_dir(&dir, my_uid().wrapping_add(1)).is_err(),
            "a dir owned by a uid other than the expected one is hostile"
        );
    }

    #[test]
    fn ensure_private_dir_reports_a_non_eexist_create_failure_as_a_create_failure() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let dir = tmp.path().join("missing-parent").join("d");
        let err = ensure_private_dir(&dir, my_uid()).expect_err("ENOENT must fail");
        assert!(
            format!("{err:#}").contains("creating hook socket dir"),
            "a non-EEXIST create error must be reported as a CREATE failure: {err:#}"
        );
    }

    /// Asks about the REAL production endpoint, built from the same fn: the
    /// guard is a path comparison, so a second hand-copied literal disarms it
    /// with the whole suite green.
    #[test]
    fn is_owned_fallback_fires_for_the_dir_the_socket_path_is_built_from() {
        let owned = owned_socket_dir();
        assert!(is_owned_fallback_in(&owned.join(SOCKET_FILE_NAME), &owned));
        assert!(!is_owned_fallback_in(
            Path::new("/tmp/pixtuoid.sock"),
            &owned
        ));
        assert!(!is_owned_fallback_in(&owned.with_extension("sock"), &owned));
    }

    #[test]
    fn ensure_owned_socket_dir_in_is_a_noop_for_non_fallback_parents() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let owned = tmp.path().join("owned");
        let sock = tmp.path().join("elsewhere").join("pixtuoid.sock");
        ensure_owned_socket_dir_in(&sock, &owned, my_uid())
            .expect("non-fallback parent is a no-op");
        assert!(
            !sock.parent().expect("parent").exists(),
            "a non-fallback parent must not be created"
        );
        assert!(
            !owned.exists(),
            "a no-op must not create the managed dir either"
        );
    }

    #[test]
    fn ensure_owned_socket_dir_in_hardens_the_dir_the_socket_lives_in() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let owned = tmp.path().join("pixtuoid-42");
        let sock = owned.join(SOCKET_FILE_NAME);
        ensure_owned_socket_dir_in(&sock, &owned, my_uid()).expect("must harden");
        let md = std::fs::symlink_metadata(&owned).expect("the managed dir must now exist");
        assert!(md.is_dir());
        assert_eq!(md.mode() & 0o777, 0o700, "created private");
    }

    #[test]
    fn ensure_owned_socket_dir_in_refuses_a_squatted_dir_the_socket_lives_in() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let owned = tmp.path().join("squatted");
        std::fs::create_dir(&owned).expect("mkdir");
        std::fs::set_permissions(&owned, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        assert!(
            ensure_owned_socket_dir_in(&owned.join(SOCKET_FILE_NAME), &owned, my_uid()).is_err(),
            "a group/other-accessible dir at the socket's parent must refuse the bind"
        );
    }
}
