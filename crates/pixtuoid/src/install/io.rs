use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};

/// [`pixtuoid_core::platform::path_env`] that ALSO requires an ABSOLUTE path — for XDG base-dir reads
/// (`XDG_CONFIG_HOME`/`XDG_STATE_HOME`): the XDG spec says a relative value is
/// invalid and must be ignored (else config/log/pack land CWD-relative, silently
/// bypassing `~/.config`). NOT for user-chosen paths like `PIXTUOID_LOG`, which
/// may legitimately be relative.
pub(crate) fn nonempty_abs_env(name: &str) -> Option<PathBuf> {
    pixtuoid_core::platform::path_env(name).filter(|v| v.is_absolute())
}

/// Normalize a config-location env override: TRIM it, and — when `home` is
/// `Some` — expand a leading `~`, `~/`, or `~\` against `home` (OpenClaw's
/// `^~(?=$|[/\\])` anchor: `~foo` is NOT a home prefix). With `home: None` (no
/// home resolved) a `~` stays literal, for the caller to refuse as relative.
///
/// Returns a [`PathBuf`], NOT a `String`: comparisons stay STRUCTURAL
/// (component-wise), never byte-wise on a `/`-vs-`\` string.
pub(crate) fn expand_tilde(value: &Path, home: Option<&Path>) -> PathBuf {
    // Trim as TEXT only where the value is text: a path that is not UTF-8 is
    // taken verbatim rather than dropped — the bytes ARE the path.
    let v = value.to_str().map_or(value, |s| Path::new(s.trim()));
    let Some(home) = home else {
        return v.to_path_buf();
    };
    if v == Path::new("~") {
        return home.to_path_buf();
    }
    if let Ok(rest) = v.strip_prefix("~") {
        return home.join(rest);
    }
    // `~\rest` is ONE component on unix, so the component strip above misses the
    // Windows form a CLI may have written; only a UTF-8 value can carry it.
    match v.to_str().and_then(|s| s.strip_prefix(r"~\")) {
        Some(rest) => home.join(rest),
        None => v.to_path_buf(),
    }
}

/// Resolve a `$HOME`-relative path, falling back to the CWD when no home dir
/// is resolvable. Only safe for read-only PROBES: WRITE paths must use
/// [`home_relative_checked`] — installing into `./.reasonix/...` produces a
/// file the CLI's global-scope loader never reads.
pub(crate) fn home_relative(rel: &str) -> PathBuf {
    pixtuoid_core::platform::user_home_opt()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(rel)
}

pub(crate) fn home_relative_checked(rel: &str) -> Result<PathBuf> {
    checked_home_join(pixtuoid_core::platform::user_home_opt(), rel)
}

/// Apply owner-only permissions to a file this binary is about to CREATE — the
/// race-free half, because the mode binds at creation and leaves no window in
/// which a co-located user can `open()` the artifact. Inert on Windows, where
/// ACLs inherit from the directory.
pub(crate) fn owner_only_create(opts: &mut OpenOptions) -> &mut OpenOptions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts
}

fn checked_home_join(home: Option<PathBuf>, rel: &str) -> Result<PathBuf> {
    home.map(|h| h.join(rel))
        .ok_or_else(|| home_unset("the home directory", "HOME/USERPROFILE"))
}

pub(crate) const HOOK_OVERRIDE_ENV: &str = "PIXTUOID_HOOK";

/// The error for "no home resolves", shared by every home-anchored target.
/// `vars` is what that target's resolver reads, so the remedy always follows
/// the list it refers to.
pub(crate) fn home_unset(what: &str, vars: &str) -> anyhow::Error {
    anyhow!("cannot resolve {what} ({vars} unset); set one and reconnect")
}

/// The remedy sentence for "the shim isn't where we looked", shared by every site
/// that offers one. It names `PIXTUOID_HOOK` and nothing else: the `--hook-path`
/// flag these messages used to advertise no longer exists, so clap answers it with
/// `unexpected argument` — an error whose only remedy was itself an error.
pub(crate) const SHIM_LOCATE_REMEDY: &str = "install it alongside pixtuoid (`brew install \
     pixtuoid` / `cargo install pixtuoid-hook` / `npm i -g pixtuoid`) or point \
     PIXTUOID_HOOK at an absolute path to the shim";

/// The shim's binary name, which a bare-name hook command resolves on PATH.
pub(crate) const HOOK_BIN: &str = "pixtuoid-hook";

/// AUTO-locate `pixtuoid-hook`: PATH, then a sibling of the running exe — both
/// arms return absolute, verified-existing paths. The [`HOOK_OVERRIDE_ENV`]
/// override is deliberately NOT read here: [`super::resolve_hook_binary_from`] absolutizes it
/// first, since a relative value embedded into a Codex/Reasonix config would
/// silently never fire from another cwd.
pub(crate) fn default_hook_binary() -> Result<PathBuf> {
    default_hook_binary_from(
        || which::which(HOOK_BIN).ok(),
        std::env::consts::EXE_EXTENSION,
        running_exe_dir,
    )
}

/// The resolution ORDER itself, with every environment read injected so both
/// platforms' resolution is drivable on any host — a truth table for
/// [`path_hit_is_native`] pins the predicate, not its use.
///
/// `exe_dir` stays LAZY: `current_exe()` must not be consulted — nor its failure
/// propagated — when the PATH arm already answered.
fn default_hook_binary_from(
    lookup: impl FnOnce() -> Option<PathBuf>,
    exe_extension: &str,
    exe_dir: impl FnOnce() -> Result<PathBuf>,
) -> Result<PathBuf> {
    if let Some(p) = lookup().filter(|p| path_hit_is_native(p, exe_extension)) {
        return Ok(p);
    }
    let dir = exe_dir()?;
    let candidate = dir.join(hook_sibling_name());
    if candidate.exists() {
        return Ok(candidate);
    }
    Err(anyhow!(
        "could not locate pixtuoid-hook (not on PATH, and not beside the pixtuoid \
         binary at {}); {SHIM_LOCATE_REMEDY}",
        dir.display()
    ))
}

fn running_exe_dir() -> Result<PathBuf> {
    let exe = std::env::current_exe().context(
        "could not determine the running executable's path while locating pixtuoid-hook",
    )?;
    exe.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("exe has no parent"))
}

/// Whether a PATH hit is a real native executable rather than a PATHEXT shim.
///
/// `which` is PATHEXT-aware on Windows, so `npm i -g pixtuoid` (which
/// materialises `pixtuoid-hook.cmd`, `.ps1` and an extensionless sh script in
/// the global bin dir) resolves to a NON-PE before the real `pixtuoid-hook.exe`
/// that the same install placed beside `pixtuoid.exe` — and every
/// `EmbedAbsolute` target spawns the embedded path WITHOUT a shell, so the hook
/// silently never fires. Rejecting the shim lets the exe-sibling arm answer.
///
/// `exe_extension` is a parameter (fed [`std::env::consts::EXE_EXTENSION`]) so
/// both platforms' truth tables are testable on either host. It is `""` on Unix,
/// which has no PATHEXT — the filter is inert there.
fn path_hit_is_native(p: &Path, exe_extension: &str) -> bool {
    exe_extension.is_empty()
        || p.extension()
            .is_some_and(|e| e.eq_ignore_ascii_case(exe_extension))
}

/// The hook binary's filename next to the running exe: exec-form spawning needs
/// the real PE name; PATHEXT is a shell behavior we must not rely on.
fn hook_sibling_name() -> String {
    format!("{HOOK_BIN}{}", std::env::consts::EXE_SUFFIX)
}

/// APPENDS `.suffix` to the full filename — never `with_extension`, which
/// truncates at the last dot (corrupting `config.local.toml` into `config.lock`).
fn sibling(target: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}.{}", target.display(), suffix))
}

/// Make a rename onto `path` durable: [fsync(2)] on the file "does not
/// necessarily ensure that the entry in the directory containing the file has
/// also reached disk", so the parent is synced too. Windows has no directory
/// handle to flush here.
///
/// [fsync(2)]: https://man7.org/linux/man-pages/man2/fsync.2.html
fn sync_parent(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        File::open(parent)?.sync_all()?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// [`sync_parent`] after a rename that already put the new bytes in place: a
/// filesystem that refuses a directory fsync (some network and FUSE mounts)
/// must not turn a write that happened into a reported failure.
fn sync_parent_after_rename(path: &Path) {
    if let Err(e) = sync_parent(path) {
        tracing::warn!(error = %e, path = ?path, "written, but its directory could not be synced");
    }
}

/// Read raw config content, following symlinks; "" for a missing file. For a
/// locked read→merge→write round use [`ConfigLock::read`] instead, so the read
/// shares the guard's pinned resolution.
pub(crate) fn read_config(path: &Path) -> Result<String> {
    read_resolved(&resolve_symlink(path))
}

/// `target` must already be symlink-resolved (or be a plain path).
fn read_resolved(target: &Path) -> Result<String> {
    if !target.exists() {
        return Ok(String::new());
    }
    let mut s = String::new();
    File::open(target)?.read_to_string(&mut s)?;
    Ok(s)
}

/// Rename `from` onto `to`, with a Windows-only bounded retry: `fs::rename` onto
/// a file another process holds open raises ERROR_SHARING_VIOLATION (os error
/// 32), and Claude Code keeps `settings.json` open briefly, so a bare rename can
/// lose the write. The sleeps match CC's typical hold duration. On Unix the
/// rename succeeds atomically even while a reader holds the old fd, so a single
/// attempt is correct there.
fn rename_with_retry(from: &Path, to: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        const MAX_ATTEMPTS: u32 = 3;
        const RENAME_RETRY_SLEEP_MS: u64 = 50;
        for _ in 1..MAX_ATTEMPTS {
            match std::fs::rename(from, to) {
                Ok(()) => return Ok(()),
                Err(_) => {
                    std::thread::sleep(std::time::Duration::from_millis(RENAME_RETRY_SLEEP_MS))
                }
            }
        }
        std::fs::rename(from, to)
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(from, to)
    }
}

/// Create `path` for writing with the config-dir hardening every atomic write
/// under `install/` shares. On Unix, `O_NOFOLLOW | O_EXCL`: a symlink an attacker
/// with config-dir write pre-plants at the tmp name is NOT followed (that would
/// clobber the link's target with our bytes), and the create owns a fresh inode
/// rather than an adopted one. A pre-existing tmp — our own crash residue, or a
/// hostile symlink O_NOFOLLOW rejected — is reclaimed ONCE (`remove_file` unlinks
/// the entry itself, never following a symlink; the retry stays hardened). The
/// final rename onto the RESOLVED target still follows that target's own symlink
/// (invariant #4) — only the distinct, attacker-controllable tmp is guarded.
fn create_hardened_tmp(path: &Path) -> Result<File> {
    let mut opts = OpenOptions::new();
    opts.create(true).write(true).truncate(true);
    owner_only_create(&mut opts);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_EXCL);
    }
    match opts.open(path) {
        Ok(f) => Ok(f),
        #[cfg(unix)]
        Err(_) if path.symlink_metadata().is_ok() => {
            std::fs::remove_file(path)?;
            Ok(opts.open(path)?)
        }
        Err(e) => Err(e.into()),
    }
}

/// RAII guard over a config file's advisory lock, held for the guard's whole
/// lifetime so a caller can cover an entire read→merge→write round. The lock
/// FILE is deliberately never unlinked: unlock-then-unlink lets a waiter holding
/// the old inode and a newcomer creating a fresh one both "hold" the lock.
///
/// Residual: an external writer (Claude Code rewriting its own settings.json)
/// can't honor this lock — it only serializes pixtuoid against pixtuoid.
#[derive(Debug)]
pub(crate) struct ConfigLock {
    /// The symlink-resolved real target — writes go here, never the symlink.
    target: PathBuf,
    file: File,
}

/// Acquire the advisory lock for `path`'s config file, resolving symlinks first
/// (invariant #4) so the lock lives beside the REAL target. FAILs on contention
/// rather than blocking.
pub(crate) fn lock_config(path: &Path) -> Result<ConfigLock> {
    let target = resolve_symlink(path);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let lock_path = sibling(&target, "lock");
    let file = open_lock_sidecar(&lock_path)?;
    file.try_lock()
        .map_err(|e| anyhow!("could not lock {}: {e}", lock_path.display()))?;
    Ok(ConfigLock { target, file })
}

/// Open (creating) the advisory-lock sidecar: `O_NOFOLLOW` so a symlink
/// pre-planted at `<target>.lock` fails the open rather than making us flock an
/// arbitrary file, and an owner-only CREATE mode so a co-located user can't
/// open+flock it and wedge every install/uninstall AND every config save
/// (`flock(2)` grants an exclusive lock through a read-only descriptor, so a
/// umask-default 0644 sidecar would be enough for them).
fn open_lock_sidecar(lock_path: &Path) -> std::io::Result<File> {
    let mut opts = OpenOptions::new();
    opts.create(true).read(true).write(true).truncate(false);
    owner_only_create(&mut opts);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    opts.open(lock_path)
}

impl ConfigLock {
    pub(crate) fn target(&self) -> &Path {
        &self.target
    }

    /// Read the locked config (missing → "") through the guard's PINNED
    /// resolution — never re-resolving the symlink. A concurrent symlink
    /// retarget (e.g. `stow --restow` mid-install) would otherwise split a
    /// locked round across two files — merge input from the NEW target, write
    /// onto the OLD — under a lock that excludes nobody at the new path.
    pub(crate) fn read(&self) -> Result<String> {
        read_resolved(&self.target)
    }

    /// Atomic write to the locked target: temp file beside it, fsync, then
    /// rename onto it. Writing through the guard (instead of re-calling
    /// `write_config_atomic`) is what avoids the same-process flock
    /// self-deadlock — a second open description on the same lock file
    /// conflicts even within one process.
    ///
    /// The temp is created 0600 on Unix and, when the target already exists,
    /// restated to the target's exact mode BEFORE any content is written — so a
    /// user-tightened settings.json (API keys) is never widened, and a fresh
    /// file defaults tight rather than umask-default.
    pub(crate) fn write_atomic(&self, contents: &str) -> Result<()> {
        let tmp = sibling(&self.target, "tmp");
        {
            let mut f = create_hardened_tmp(&tmp)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let perms = std::fs::metadata(&self.target)
                    .map(|m| m.permissions())
                    .unwrap_or_else(|_| std::fs::Permissions::from_mode(0o600));
                f.set_permissions(perms)?;
            }
            f.write_all(contents.as_bytes())?;
            f.sync_all()?;
        }
        rename_with_retry(&tmp, &self.target)?;
        sync_parent_after_rename(&self.target);
        Ok(())
    }
}

impl Drop for ConfigLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// Atomic write that follows symlinks, advisory-locked for the duration of the
/// write only — a read→merge→write round must instead take [`lock_config`]
/// before the read and write via [`ConfigLock::write_atomic`].
pub(crate) fn write_config_atomic(path: &Path, contents: &str) -> Result<()> {
    lock_config(path)?.write_atomic(contents)
}

/// A plugin host's own CLI, through which pixtuoid registers and removes its plugin.
#[derive(Debug, Clone, Copy)]
pub(crate) struct HostCli {
    pub(crate) command: &'static str,
}

impl HostCli {
    /// `which` applies PATHEXT, so an npm install's `<command>.cmd` resolves on
    /// Windows. `None` when it is not on PATH: an uninstall then can't deregister.
    pub(crate) fn locate(self) -> Option<PathBuf> {
        which::which(self.command).ok()
    }

    /// [`Self::locate`], or the error that names what to do about it.
    ///
    /// homebrew-core contract: the formula's `test do` connects `claude-code`
    /// in a sandbox with no `claude`, and expects this error to fail it.
    pub(crate) fn require(self) -> Result<PathBuf> {
        let Self { command } = self;
        self.locate().with_context(|| {
            format!(
                "`{command}` is not on PATH; pixtuoid installs its hooks as a plugin \
                 through it — put it on PATH and reconnect"
            )
        })
    }

    /// Run it with `args` and return its stdout; a non-zero exit is the error.
    pub(crate) fn run(self, args: &[&std::ffi::OsStr]) -> Result<String> {
        let Self { command, .. } = self;
        let cli = self.require()?;
        // The real CLI would register and remove plugins in the developer's own
        // CLI — a test run once did.
        #[cfg(test)]
        assert!(
            cli.starts_with(std::env::temp_dir()),
            "a test is about to run the real {} — put a fake `{command}` first on PATH",
            cli.display()
        );
        let out = std::process::Command::new(&cli)
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .with_context(|| format!("running {}", cli.display()))?;
        if !out.status.success() {
            return Err(anyhow!(
                "`{command} {}` failed: {}",
                args.iter()
                    .map(|a| a.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(" "),
                String::from_utf8_lossy(if out.stderr.is_empty() {
                    &out.stdout
                } else {
                    &out.stderr
                })
                .trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

/// Whether the bare `pixtuoid-hook` name resolves on PATH: a bare-name target's
/// hook config stores the name for portability and the CLI spawns hooks via PATH,
/// so if this is false the installed hooks silently never fire.
pub(crate) fn hook_on_path() -> bool {
    which::which(HOOK_BIN).is_ok()
}

/// Follow symlink chain to the final target, even if that target doesn't exist
/// yet (stow creates the link before the dotfiles repo is fully set up).
/// `canonicalize` fails on a dangling symlink, so we walk `read_link` manually.
pub(crate) fn resolve_symlink(path: &Path) -> PathBuf {
    let mut cur = path.to_path_buf();
    for _ in 0..32 {
        match std::fs::symlink_metadata(&cur) {
            Ok(meta) if meta.file_type().is_symlink() => match std::fs::read_link(&cur) {
                Ok(target) => {
                    cur = if target.is_relative() {
                        cur.parent().unwrap_or(Path::new(".")).join(&target)
                    } else {
                        target
                    };
                }
                Err(_) => return cur,
            },
            _ => return cur,
        }
    }
    cur
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[cfg(unix)]
    #[test]
    fn sync_parent_opens_the_real_parent() {
        let tmp = TempDir::new().unwrap();
        sync_parent(&tmp.path().join("settings.json")).unwrap();
        assert!(
            sync_parent(&tmp.path().join("gone").join("settings.json")).is_err(),
            "a missing parent must fail the sync, not skip it"
        );
    }

    #[test]
    fn a_windows_path_hit_must_be_a_real_pe_not_a_pathext_shim() {
        let win = |p: &str| path_hit_is_native(Path::new(p), "exe");
        assert!(win(r"C:\Users\me\bin\pixtuoid-hook.exe"));
        assert!(
            win(r"C:\Users\me\bin\pixtuoid-hook.EXE"),
            "PATHEXT is case-insensitive"
        );
        for shim in [
            r"C:\Users\me\AppData\Roaming\npm\pixtuoid-hook.cmd",
            r"C:\Users\me\AppData\Roaming\npm\pixtuoid-hook.ps1",
            r"C:\Users\me\AppData\Roaming\npm\pixtuoid-hook.bat",
            r"C:\Users\me\AppData\Roaming\npm\pixtuoid-hook",
        ] {
            assert!(
                !path_hit_is_native(Path::new(shim), "exe"),
                "{shim} is not a PE — exec-form spawning can't launch it"
            );
        }
        assert!(path_hit_is_native(
            Path::new("/usr/local/bin/pixtuoid-hook"),
            ""
        ));
        assert!(path_hit_is_native(Path::new("/opt/pixtuoid-hook.sh"), ""));
    }

    #[test]
    fn resolution_refuses_a_pathext_shim_and_falls_through_to_the_exe_sibling() {
        let dir = TempDir::new().unwrap();
        let sibling = dir.path().join(hook_sibling_name());
        std::fs::write(&sibling, "").unwrap();
        let shim = dir.path().join("npm").join("pixtuoid-hook.cmd");
        let exe_dir = || Ok(dir.path().to_path_buf());

        assert_eq!(
            default_hook_binary_from(|| Some(shim.clone()), "exe", exe_dir).unwrap(),
            sibling,
            "a PATHEXT shim must never outrank the exe sibling"
        );
        assert_eq!(
            default_hook_binary_from(|| Some(shim.clone()), "", exe_dir).unwrap(),
            shim,
            "the filter must not reject anything where there is no PATHEXT"
        );

        let empty = TempDir::new().unwrap();
        let e = default_hook_binary_from(|| None, "exe", || Ok(empty.path().to_path_buf()))
            .unwrap_err()
            .to_string();
        assert!(e.contains(HOOK_OVERRIDE_ENV), "got: {e}");
    }

    #[cfg(unix)]
    fn mode_of(p: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[cfg(unix)]
    #[test]
    fn a_fresh_lock_sidecar_is_created_owner_only() {
        let dir = TempDir::new().unwrap();
        let lock_path = dir.path().join("settings.json.lock");
        drop(open_lock_sidecar(&lock_path).unwrap());
        assert_eq!(
            mode_of(&lock_path),
            0o600,
            "a fresh lock sidecar must be owner-only from the open itself"
        );
    }

    #[test]
    fn the_shim_locate_remedy_names_an_escape_hatch_the_cli_accepts() {
        use clap::Parser;
        assert!(
            crate::cli::Cli::try_parse_from(["pixtuoid", "connect", "codex", "--hook-path", "/x"])
                .is_err(),
            "if --hook-path is ever re-added, revisit this remedy wording"
        );
        assert!(SHIM_LOCATE_REMEDY.contains(HOOK_OVERRIDE_ENV));
        assert!(!SHIM_LOCATE_REMEDY.contains("--hook-path"));
    }

    #[test]
    fn expand_tilde_home_some_expands_leading_tilde_only() {
        let home = Path::new("/home/u");
        // Expected paths are built via `join`, never a hardcoded `/`: the
        // comparison must stay structural to hold on Windows too.
        assert_eq!(expand_tilde(Path::new("~"), Some(home)), home.to_path_buf());
        assert_eq!(
            expand_tilde(Path::new("~/claw"), Some(home)),
            home.join("claw")
        );
        assert_eq!(
            expand_tilde(Path::new(r"~\claw"), Some(home)),
            home.join("claw")
        );
        assert_eq!(
            expand_tilde(Path::new("  ~/claw  "), Some(home)),
            home.join("claw")
        );
        assert_eq!(
            expand_tilde(Path::new("~foo"), Some(home)),
            PathBuf::from("~foo")
        );
        assert_eq!(
            expand_tilde(Path::new("~user/p"), Some(home)),
            PathBuf::from("~user/p")
        );
        assert_eq!(
            expand_tilde(Path::new("rel/~/x"), Some(home)),
            PathBuf::from("rel/~/x")
        );
        assert_eq!(
            expand_tilde(Path::new("/abs/x"), Some(home)),
            PathBuf::from("/abs/x")
        );
    }

    #[test]
    fn expand_tilde_home_none_trims_only_never_expands() {
        assert_eq!(
            expand_tilde(Path::new("  /abs/x  "), None),
            PathBuf::from("/abs/x")
        );
        assert_eq!(
            expand_tilde(Path::new("~/claw"), None),
            PathBuf::from("~/claw")
        );
        assert_eq!(expand_tilde(Path::new("~"), None), PathBuf::from("~"));
    }

    #[test]
    fn nonempty_abs_env_requires_an_absolute_path() {
        const KEY: &str = "PIXTUOID_TEST_NONEMPTY_ABS_ENV";
        for unset in ["", "   ", "rel/x", "~/x"] {
            temp_env::with_var(KEY, Some(unset), || {
                assert_eq!(nonempty_abs_env(KEY), None, "{unset:?} must read as unset");
            });
        }
        // A leading-slash path is NOT absolute on Windows (no drive prefix).
        let abs = if cfg!(windows) { "C:/abs/x" } else { "/abs/x" };
        temp_env::with_var(KEY, Some(abs), || {
            assert_eq!(nonempty_abs_env(KEY), Some(PathBuf::from(abs)));
        });
        temp_env::with_var_unset(KEY, || {
            assert_eq!(nonempty_abs_env(KEY), None, "a missing var is unset");
        });
    }

    #[test]
    fn rename_with_retry_moves_file() {
        let dir = TempDir::new().unwrap();
        let from = dir.path().join("src.tmp");
        let to = dir.path().join("dst.json");
        std::fs::write(&from, "hello").unwrap();
        rename_with_retry(&from, &to).unwrap();
        assert!(!from.exists());
        assert_eq!(std::fs::read_to_string(&to).unwrap(), "hello");
    }

    #[cfg(unix)]
    #[test]
    fn lock_config_refuses_a_symlinked_lock_file() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("settings.json");
        std::fs::write(&target, "{}").unwrap();
        let lock_path = sibling(&target, "lock");
        let decoy = dir.path().join("decoy");
        std::fs::write(&decoy, "x").unwrap();
        std::os::unix::fs::symlink(&decoy, &lock_path).unwrap();
        assert!(
            lock_config(&target).is_err(),
            "a symlinked <target>.lock must be refused, not followed"
        );
    }

    #[test]
    fn resolve_symlink_regular_file_returns_as_is() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("plain.json");
        std::fs::write(&file, "{}").unwrap();
        assert_eq!(resolve_symlink(&file), file);
    }

    #[test]
    fn resolve_symlink_nonexistent_returns_as_is() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("missing");
        assert_eq!(resolve_symlink(&path), path);
    }

    #[cfg(unix)]
    #[test]
    fn resolve_symlink_follows_single_hop() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("real.json");
        std::fs::write(&target, "{}").unwrap();
        let link = dir.path().join("link.json");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert_eq!(resolve_symlink(&link), target);
    }

    #[cfg(unix)]
    #[test]
    fn resolve_symlink_follows_chain() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("real.json");
        std::fs::write(&target, "{}").unwrap();
        let mid = dir.path().join("mid.json");
        std::os::unix::fs::symlink(&target, &mid).unwrap();
        let link = dir.path().join("link.json");
        std::os::unix::fs::symlink(&mid, &link).unwrap();
        assert_eq!(resolve_symlink(&link), target);
    }

    #[cfg(unix)]
    #[test]
    fn resolve_symlink_dangling_returns_target() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("nonexistent.json");
        let link = dir.path().join("link.json");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert_eq!(resolve_symlink(&link), target);
    }

    #[cfg(unix)]
    #[test]
    fn resolve_symlink_relative_target() {
        let dir = TempDir::new().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let target = sub.join("real.json");
        std::fs::write(&target, "{}").unwrap();
        let link = dir.path().join("link.json");
        std::os::unix::fs::symlink(Path::new("sub/real.json"), &link).unwrap();
        let resolved = resolve_symlink(&link);
        assert_eq!(
            std::fs::canonicalize(&resolved).unwrap(),
            std::fs::canonicalize(&target).unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolve_symlink_cycle_terminates_after_budget() {
        // A 2-node cycle a→b→a: lstat + readlink both succeed on every hop
        // without following, so only the hop budget stops the walk.
        let dir = TempDir::new().unwrap();
        let a = dir.path().join("a.link");
        let b = dir.path().join("b.link");
        std::os::unix::fs::symlink(&b, &a).unwrap();
        std::os::unix::fs::symlink(&a, &b).unwrap();
        let resolved = resolve_symlink(&a);
        assert!(resolved == a || resolved == b, "got {resolved:?}");
    }

    #[test]
    fn read_config_missing_returns_empty_string() {
        let dir = TempDir::new().unwrap();
        assert_eq!(read_config(&dir.path().join("nope.json")).unwrap(), "");
    }

    #[test]
    fn read_config_empty_file_returns_empty_string() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("empty.json");
        std::fs::write(&p, "").unwrap();
        assert_eq!(read_config(&p).unwrap(), "");
    }

    #[test]
    fn read_config_returns_raw_content() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("c.toml");
        std::fs::write(&p, "a = 1\n").unwrap();
        assert_eq!(read_config(&p).unwrap(), "a = 1\n");
    }

    #[cfg(unix)]
    #[test]
    fn write_config_atomic_through_symlink_preserves_link() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("real.json");
        std::fs::write(&target, "{}").unwrap();
        let link = dir.path().join("link.json");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        write_config_atomic(&link, "{\"a\":1}").unwrap();
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "{\"a\":1}");
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_refuses_a_symlink_planted_at_the_tmp() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("settings.json");
        std::fs::write(&target, "{}").unwrap();
        let victim = dir.path().join("victim");
        std::fs::write(&victim, "PRECIOUS").unwrap();
        std::os::unix::fs::symlink(&victim, sibling(&target, "tmp")).unwrap();

        lock_config(&target)
            .unwrap()
            .write_atomic("{\"hooks\":{}}")
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            "PRECIOUS",
            "the planted symlink must not be followed — victim untouched"
        );
        assert!(
            std::fs::read_to_string(&target).unwrap().contains("hooks"),
            "the real target still receives the write (the .tmp symlink was reclaimed)"
        );
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_refuses_a_hardlink_planted_at_the_tmp() {
        // O_NOFOLLOW ignores HARDLINKS — O_EXCL is what covers this one.
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("settings.json");
        std::fs::write(&target, "{}").unwrap();
        let victim = dir.path().join("victim");
        std::fs::write(&victim, "PRECIOUS").unwrap();
        std::fs::hard_link(&victim, sibling(&target, "tmp")).unwrap();

        lock_config(&target)
            .unwrap()
            .write_atomic("{\"hooks\":{}}")
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            "PRECIOUS",
            "O_EXCL must reclaim the hardlink, not write through it — victim untouched"
        );
    }

    #[test]
    fn lock_config_excludes_a_second_locker_until_dropped() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("settings.json");
        let guard = lock_config(&p).unwrap();
        let err = lock_config(&p).expect_err("a second lock on the same config must fail");
        assert!(err.to_string().contains("could not lock"), "got: {err:#}");
        drop(guard);
        lock_config(&p).expect("the lock is released when the guard drops");
    }

    #[test]
    fn write_atomic_under_a_held_guard_does_not_self_deadlock() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("settings.json");
        let guard = lock_config(&p).unwrap();
        guard.write_atomic("{\"a\":1}").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{\"a\":1}");
    }

    #[test]
    fn lock_config_creates_missing_parent_dir() {
        // Every other lock_config test pre-creates the parent (the TempDir root),
        // so this is the only one that fails if the create_dir_all is dropped.
        let dir = TempDir::new().unwrap();
        let parent = dir.path().join("sub/nested");
        assert!(!parent.exists(), "precondition: parent must not exist yet");
        let p = parent.join("settings.json");
        let _guard = lock_config(&p).expect("lock_config must create the missing parent");
        assert!(parent.is_dir(), "the missing parent chain was created");
    }

    #[cfg(unix)]
    #[test]
    fn lock_config_resolves_symlinks_to_the_real_target() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("real.json");
        std::fs::write(&target, "{}").unwrap();
        let link = dir.path().join("link.json");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let guard = lock_config(&link).unwrap();
        assert_eq!(guard.target(), target);
        assert!(dir.path().join("real.json.lock").exists());
        assert!(lock_config(&target).is_err(), "same lock via either path");
    }

    #[cfg(unix)]
    #[test]
    fn config_lock_read_pins_the_lock_time_resolution() {
        let dir = TempDir::new().unwrap();
        let old = dir.path().join("old.json");
        std::fs::write(&old, "old-content").unwrap();
        let new = dir.path().join("new.json");
        std::fs::write(&new, "new-content").unwrap();
        let link = dir.path().join("link.json");
        std::os::unix::fs::symlink(&old, &link).unwrap();

        let guard = lock_config(&link).unwrap();
        // The concurrent retarget lands inside the locked round.
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&new, &link).unwrap();

        assert_eq!(
            guard.read().unwrap(),
            "old-content",
            "the read is pinned to the lock-time target"
        );
    }

    #[test]
    fn checked_home_join_errors_without_home() {
        let err = checked_home_join(None, ".reasonix/settings.json").unwrap_err();
        assert_eq!(
            err.to_string(),
            "cannot resolve the home directory (HOME/USERPROFILE unset); set one and reconnect"
        );
    }

    #[test]
    fn checked_home_join_joins_a_resolved_home() {
        assert_eq!(
            checked_home_join(Some("/home/u".into()), ".reasonix/settings.json").unwrap(),
            PathBuf::from("/home/u/.reasonix/settings.json")
        );
    }

    #[cfg(unix)]
    #[test]
    fn write_config_atomic_preserves_target_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("settings.json");
        std::fs::write(&p, "{}").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();

        write_config_atomic(&p, "{\"a\":1}").unwrap();

        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "a user-tightened settings.json must not be widened by a rewrite"
        );
    }

    #[cfg(unix)]
    #[test]
    fn write_config_atomic_creates_new_files_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("settings.json");

        write_config_atomic(&p, "{}").unwrap();

        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "settings.json can carry API keys — a fresh file defaults tight"
        );
    }

    #[test]
    fn lock_and_tmp_names_use_string_append() {
        let p = Path::new("config.local.toml");
        assert_eq!(sibling(p, "lock"), Path::new("config.local.toml.lock"));
        assert_eq!(sibling(p, "tmp"), Path::new("config.local.toml.tmp"));
    }

    #[test]
    fn default_hook_binary_sibling_appends_exe_suffix() {
        // Pin the per-platform LITERAL; re-computing via EXE_SUFFIX would be
        // tautological.
        #[cfg(unix)]
        assert_eq!(hook_sibling_name(), "pixtuoid-hook");
        #[cfg(windows)]
        assert_eq!(hook_sibling_name(), "pixtuoid-hook.exe");
    }
}
