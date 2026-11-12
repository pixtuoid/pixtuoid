//! Linux focus glue: core's `pid_parent` for the ancestor walk; window
//! ownership and activation ride ONE channel per environment — sway's IPC
//! when `$SWAYSOCK` is present (`swaymsg`), hyprland's when
//! `$HYPRLAND_INSTANCE_SIGNATURE` is (`hyprctl`), else the X11/EWMH
//! `_NET_ACTIVE_WINDOW` protocol (the wmctrl mechanism) via x11rb — which
//! covers i3 too (X11-native; `$I3SOCK` deliberately does NOT route to
//! `swaymsg`). The walk exists because the agent process owns no surface —
//! its terminal emulator does. GNOME Wayland forbids focus-steal by design —
//! every channel simply fails there → the caller's silent no-op, per the ONE
//! failure rule.
//!
//! codecov-ignored glue; the walk logic is tested in `focus::tests`.

use super::ProcessTable;

// The SINGLE source `detect_channel` and doctor's `activation_backend` read, so
// an upstream rename can't drift the copies.
pub(crate) const SWAY_ENV: &str = "SWAYSOCK";
pub(crate) const HYPRLAND_ENV: &str = "HYPRLAND_INSTANCE_SIGNATURE";

/// Which pid-addressable focus channel this environment implies. Read ONCE so
/// `focusable` and `activate_os` can't diverge — a 4th compositor added to one
/// but missed in the other would report a pid focusable, then silently no-op on
/// activate.
enum LinuxFocusChannel {
    Sway,
    Hyprland,
    X11,
}

/// EMPTY (or whitespace-only) counts as UNSET, NOT bare presence: systemd user
/// units and non-forwarded ssh sessions routinely leave an exported-but-blank
/// `SWAYSOCK` behind, and reading that as "sway is running" routes both halves
/// to `swaymsg` on a host where EWMH would have worked.
fn detect_channel() -> LinuxFocusChannel {
    if pixtuoid_core::platform::path_env(SWAY_ENV).is_some() {
        LinuxFocusChannel::Sway
    } else if pixtuoid_core::platform::path_env(HYPRLAND_ENV).is_some() {
        LinuxFocusChannel::Hyprland
    } else {
        LinuxFocusChannel::X11
    }
}

pub(crate) struct OsProcessTable;

impl ProcessTable for OsProcessTable {
    fn ppid(&self, pid: i32) -> Option<i32> {
        pixtuoid_core::source::pid_parent(pid)
    }

    fn focusable(&self, pid: i32) -> bool {
        match detect_channel() {
            LinuxFocusChannel::Sway => tree_lists_pid("swaymsg", &["-t", "get_tree"], pid),
            LinuxFocusChannel::Hyprland => tree_lists_pid("hyprctl", &["clients", "-j"], pid),
            LinuxFocusChannel::X11 => x11_window_of(pid).is_some(),
        }
    }
}

/// Whether the compositor's JSON tree lists a node with `"pid": <pid>`. Full
/// serde parse + recursive scan rather than a substring match: both tools vary
/// pretty-vs-compact output by tty.
fn tree_lists_pid(cmd: &str, args: &[&str], pid: i32) -> bool {
    let Ok(out) = std::process::Command::new(cmd).args(args).output() else {
        return false;
    };
    serde_json::from_slice::<serde_json::Value>(&out.stdout)
        .is_ok_and(|v| json_tree_has_pid(&v, i64::from(pid)))
}

fn json_tree_has_pid(v: &serde_json::Value, pid: i64) -> bool {
    match v {
        serde_json::Value::Object(m) => {
            m.get("pid").and_then(serde_json::Value::as_i64) == Some(pid)
                || m.values().any(|c| json_tree_has_pid(c, pid))
        }
        serde_json::Value::Array(a) => a.iter().any(|c| json_tree_has_pid(c, pid)),
        _ => false,
    }
}

/// Find an X11 window whose `_NET_WM_PID` matches, via x11rb.
fn x11_window_of(pid: i32) -> Option<u32> {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt};
    let (conn, screen_num) = x11rb::connect(None).ok()?;
    let root = conn.setup().roots[screen_num].root;
    let net_client_list = conn
        .intern_atom(false, b"_NET_CLIENT_LIST")
        .ok()?
        .reply()
        .ok()?
        .atom;
    let net_wm_pid = conn
        .intern_atom(false, b"_NET_WM_PID")
        .ok()?
        .reply()
        .ok()?
        .atom;
    let clients = conn
        .get_property(false, root, net_client_list, AtomEnum::WINDOW, 0, u32::MAX)
        .ok()?
        .reply()
        .ok()?;
    for win in clients.value32()? {
        if let Ok(Ok(prop)) = conn
            .get_property(false, win, net_wm_pid, AtomEnum::CARDINAL, 0, 1)
            .map(|c| c.reply())
            && prop.value32().and_then(|mut v| v.next()) == Some(pid as u32)
        {
            return Some(win);
        }
    }
    None
}

/// Activate `pid`'s window on the same channel `focusable` matched it on.
pub(crate) fn activate_os(pid: i32) -> bool {
    match detect_channel() {
        LinuxFocusChannel::Sway => {
            let criteria = format!("[pid={pid}] focus");
            run_detached("swaymsg", &[criteria.as_str()])
        }
        LinuxFocusChannel::Hyprland => {
            let target = format!("pid:{pid}");
            run_detached("hyprctl", &["dispatch", "focuswindow", target.as_str()])
        }
        LinuxFocusChannel::X11 => x11_activate(pid).unwrap_or(false),
    }
}

/// Run a compositor IPC command with EVERY stdio stream nulled, and report
/// whether it succeeded.
///
/// The caller is the crossterm event loop, INSIDE the TUI's raw-mode alternate
/// screen. Inherited stdio — `Command::status`'s default — would let the child
/// paint into the office, where ratatui's diff-based redraw leaves it until
/// those cells next change, and would hand the child the raw-mode tty the TUI
/// is polling for keystrokes.
fn run_detached(cmd: &str, args: &[&str]) -> bool {
    std::process::Command::new(cmd)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn x11_activate(pid: i32) -> Option<bool> {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{ClientMessageEvent, ConnectionExt, EventMask};
    let win = x11_window_of(pid)?;
    let (conn, screen_num) = x11rb::connect(None).ok()?;
    let root = conn.setup().roots[screen_num].root;
    let net_active = conn
        .intern_atom(false, b"_NET_ACTIVE_WINDOW")
        .ok()?
        .reply()
        .ok()?
        .atom;
    // Source indication 2 = a pager/direct user action (the wmctrl value).
    let ev = ClientMessageEvent::new(32, win, net_active, [2, 0, 0, 0, 0]);
    conn.send_event(
        false,
        root,
        EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
        ev,
    )
    .ok()?;
    conn.flush().ok()?;
    Some(true)
}
