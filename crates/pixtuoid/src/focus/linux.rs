//! Linux focus glue: core's `pid_parent` for the ancestor walk; window
//! ownership and activation ride ONE channel per environment — sway's IPC
//! when `$SWAYSOCK` is present (`swaymsg`), hyprland's when
//! `$HYPRLAND_INSTANCE_SIGNATURE` is (`hyprctl`), niri's when `$NIRI_SOCKET`
//! is (`niri msg`), else the X11/EWMH
//! `_NET_ACTIVE_WINDOW` protocol (the wmctrl mechanism) via x11rb — which
//! covers i3 too (X11-native; `$I3SOCK` deliberately does NOT route to
//! `swaymsg`). The walk exists because the agent process owns no surface —
//! its terminal emulator does. GNOME Wayland forbids focus-steal by design —
//! every channel simply fails there → the caller's silent no-op, per the ONE
//! failure rule.
//!
//! The walk logic is tested in `focus::tests`, the channel choice and niri's
//! window lookup here.

use super::ProcessTable;

const SWAY_ENV: &str = "SWAYSOCK";
const HYPRLAND_ENV: &str = "HYPRLAND_INSTANCE_SIGNATURE";
const NIRI_ENV: &str = "NIRI_SOCKET";

/// Which pid-addressable focus channel this environment implies. Read ONCE so
/// `focusable`, `activate_os` and doctor's verdict can't diverge — a compositor
/// added to one but missed in another would report a pid focusable, then
/// silently no-op on activate.
pub(crate) enum LinuxFocusChannel {
    Sway,
    Hyprland,
    Niri,
    X11,
}

impl LinuxFocusChannel {
    /// The compositor IPC doctor names as the backend; `None` for EWMH, whose
    /// verdict doctor must still weigh against a Wayland session.
    pub(crate) fn ipc_backend(&self) -> Option<&'static str> {
        match self {
            Self::Sway => Some("sway IPC (swaymsg)"),
            Self::Hyprland => Some("hyprland IPC (hyprctl)"),
            Self::Niri => Some("niri IPC (niri msg)"),
            Self::X11 => None,
        }
    }
}

/// EMPTY (or whitespace-only) counts as UNSET, NOT bare presence: systemd user
/// units and non-forwarded ssh sessions routinely leave an exported-but-blank
/// `SWAYSOCK` behind, and reading that as "sway is running" routes both halves
/// to `swaymsg` on a host where EWMH would have worked.
pub(crate) fn detect_channel() -> LinuxFocusChannel {
    let set = |name| pixtuoid_core::platform::path_env(name).is_some();
    if set(SWAY_ENV) {
        LinuxFocusChannel::Sway
    } else if set(HYPRLAND_ENV) {
        LinuxFocusChannel::Hyprland
    } else if set(NIRI_ENV) {
        LinuxFocusChannel::Niri
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
            LinuxFocusChannel::Niri => niri_window_id(pid).is_some(),
            LinuxFocusChannel::X11 => x11_window_of(pid).is_some(),
        }
    }
}

/// The id of a niri window `pid` owns, from `niri msg -j windows`. niri's
/// `focus-window` addresses a window by id only, so both halves ride this one
/// lookup.
fn niri_window_id(pid: i32) -> Option<u64> {
    let out = std::process::Command::new("niri")
        .args(["msg", "-j", "windows"])
        .output()
        .ok()?;
    let windows = serde_json::from_slice::<serde_json::Value>(&out.stdout).ok()?;
    niri_window_of(&windows, i64::from(pid))
}

/// niri lists windows as a flat array whose `pid` is the client's (null for a
/// client niri can't attribute). Several windows of one pid (a single-instance
/// terminal) resolve to the first: focus is app-level by design.
fn niri_window_of(windows: &serde_json::Value, pid: i64) -> Option<u64> {
    windows
        .as_array()?
        .iter()
        .find(|w| w.get("pid").and_then(serde_json::Value::as_i64) == Some(pid))?
        .get("id")?
        .as_u64()
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
        LinuxFocusChannel::Niri => niri_window_id(pid).is_some_and(|id| {
            let id = id.to_string();
            run_detached("niri", &["msg", "action", "focus-window", "--id", &id])
        }),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `niri msg -j windows` on niri 26.04, recorded with titles blanked and
    /// fields no lookup reads dropped; the last window is the null-pid shape.
    const NIRI_WINDOWS: &str = r#"[
        {"id":4,"title":"t","app_id":"org.telegram.desktop","pid":14707,"workspace_id":7,"is_focused":false},
        {"id":15,"title":"t","app_id":"kitty","pid":165907,"workspace_id":2,"is_focused":false},
        {"id":18,"title":"t","app_id":"kitty","pid":222335,"workspace_id":2,"is_focused":true},
        {"id":21,"title":"t","app_id":"xwayland-satellite","pid":null,"workspace_id":2,"is_focused":false}
    ]"#;

    #[test]
    fn niri_window_of_maps_a_terminal_pid_to_its_window_id() {
        let windows: serde_json::Value = serde_json::from_str(NIRI_WINDOWS).unwrap();
        assert_eq!(niri_window_of(&windows, 222_335), Some(18));
        assert_eq!(niri_window_of(&windows, 165_907), Some(15));
    }

    #[test]
    fn niri_window_of_misses_a_pid_no_window_owns() {
        let windows: serde_json::Value = serde_json::from_str(NIRI_WINDOWS).unwrap();
        assert_eq!(niri_window_of(&windows, 4242), None);
        assert_eq!(niri_window_of(&serde_json::json!({"Err": "x"}), 4242), None);
    }

    #[test]
    fn a_niri_socket_alone_routes_focus_to_niri() {
        let only_niri = [
            (SWAY_ENV, None),
            (HYPRLAND_ENV, None),
            (NIRI_ENV, Some("/run/user/1000/niri.wayland-1.2169.sock")),
        ];
        temp_env::with_vars(only_niri, || {
            assert!(matches!(detect_channel(), LinuxFocusChannel::Niri));
        });
        let blank_niri = [
            (SWAY_ENV, None),
            (HYPRLAND_ENV, None),
            (NIRI_ENV, Some(" ")),
        ];
        temp_env::with_vars(blank_niri, || {
            assert!(
                matches!(detect_channel(), LinuxFocusChannel::X11),
                "a blank NIRI_SOCKET is a leftover"
            );
        });
    }
}
