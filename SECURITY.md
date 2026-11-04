# Security Policy

## Supported Versions

Fixes land on the latest released version; there are no maintained back-branches.
Upgrade to the newest release (`cargo install pixtuoid`, `brew upgrade`, or
`npm i -g pixtuoid`) to receive security fixes.

| Version        | Supported |
|----------------|-----------|
| latest         | ✅        |
| older releases | ❌        |

## Privacy posture

pixtuoid is **telemetry-free and local by default**:

- **No outbound connections.** The binary makes no outbound network connections — no
  analytics, crash-reporting upload, update check, or "phone home". (The crash
  hook writes a backtrace to a *local* file and only ever offers a pre-filled
  GitHub URL for you to open manually.) The dependency set is audited daily for
  advisories via `cargo-deny` (see `.github/workflows/audit.yml`).
- **No listener unless you start one.** Only `pixtuoid serve` listens on a
  network; past announcing this machine's name and its key's SPKI hash there
  (DNS-SD `_pixtuoid._tcp`), which pairs no one, it gives a device nothing until paired.
- **Your session data stays on your machine unless you run `pixtuoid serve`.**
  pixtuoid reads your agent CLIs'
  transcripts (e.g. `~/.claude/projects`, `~/.codex/sessions`) **read-only**, to
  derive what each sprite is doing. Past the announcement above, only
  `pixtuoid serve` sends anything, to paired devices: agent status, model, usage,
  repo names (a project directory's last component), which agent CLIs you have
  and how each is connected, and this machine's name and OS. No path,
  prompt, tool input or transcript leaves; the wire is the allowlist in [`remote.rs`](crates/pixtuoid-core/src/remote.rs).

## Trust boundaries / attack surface

The components that handle untrusted or privileged input, and how they're bounded:

1. **The hook shim (`pixtuoid-hook`)** is invoked by your agent CLI and forwards a
   single JSON line from stdin to the office over a **Unix domain socket** (a
   per-user runtime path — `$XDG_RUNTIME_DIR/pixtuoid.sock`, else a per-user
   `0700` directory `/tmp/pixtuoid-<uid>/pixtuoid.sock`; `PIXTUOID_SOCKET`
   overrides — the socket itself created `0600`, owner-only). The `/tmp` fallback
   dir is created with a TOCTOU-safe `mkdir` + ownership/mode validation, and the
   shim verifies the connected peer's uid (`getpeereid`/`SO_PEERCRED`) before
   writing — so another user cannot squat the (formerly flat, predictable)
   rendezvous path to disable the hook plane, nor intercept the payload by racing
   a listener onto it. On **Windows** the transport is a named pipe
   `\\.\pipe\pixtuoid-<user>` with an owner-only DACL, and the shim likewise
   verifies the pipe **server's** token user SID matches ours
   (`GetNamedPipeServerProcessId` → token compare) before writing — closing the
   same squat-and-intercept vector on the machine-global pipe namespace (#495).
   It is a *local IPC*, never on a network. The shim is hardened to **never block the agent**: it always exits
   `0` on any error, and a hard watchdog bounds its send (pixtuoid-hook's
   `transport::WRITE_TIMEOUT`). It does not execute or shell out to anything in
   the payload.

2. **The office's socket listener** binds that owner-only socket with a flock'd,
   `O_NOFOLLOW`, atomic-rename bind — only the local user can connect. A second
   instance subscribes over it to every payload the owner accepts, after checking
   the owner is this user; another process of this user could too, which crosses
   no boundary: it already runs with this user's access. The CLI's
   plain-text surfaces (the `--headless` summary, the `doctor` report, the
   Sources/install output) **strip control characters** from untrusted wire values
   (`strip_control_chars`) so a crafted transcript can't smuggle escape sequences
   into piped output. The live half-block TUI renders into its own cell grid (agent
   labels derive from project-directory path components, read over the user's own
   `0600` socket and transcripts), not by echoing raw bytes to the terminal.

3. **Hook installation** (when you explicitly *connect* a source) goes through
   the agent CLI's own plugin system where it has one — Claude Code, Codex, Hermes,
   Reasonix and Antigravity get a local plugin managed through their own plugin
   subcommands, so each CLI writes its own settings — and otherwise edits the CLI's config through a single advisory-locked,
   `fsync` + atomic-rename writer that **preserves the file's permissions,
   follows stow symlinks, and changes only pixtuoid's own entries**. A
   rewritten config keeps every other setting, though not its comments or
   formatting.
   Installs are idempotent and reversible (*disconnect* removes the plugin or the
   hook entries), and either one also strips the hook entries a release before
   the plugin wrote into the CLI's own config. pixtuoid never writes another
   tool's config except on an explicit connect/disconnect; on launch it may run a
   connected, enabled Codex's own `codex plugin add` to refresh a stale copy of
   the plugin.

4. **`pixtuoid serve`**, the only network listener, binds IPv4 on every interface
   and, before any TLS, drops a peer that isn't private, link-local or loopback,
   a Tailscale tailnet's
   [`100.64.0.0/10`](https://tailscale.com/kb/1015/100.x-addresses) included. It speaks TLS 1.3 only.
   Its key is made on first use, and a device pins that key's SPKI hash from the
   QR code `pixtuoid pair` prints, beside a one-time code that expires
   (`CODE_LIFETIME`) and is redeemed for a device token. The QR is a link on
   pixtuoid.dev whose fields ride the fragment, which a browser keeps to
   itself: a phone without the app opens a `/pair/` page there that runs no
   script. The key and the hashes of
   the code and tokens sit in one owner-only file in the state directory, never
   in a dotfiles-prone `~/.config`. `pair --reset` forgets every device, ending
   its stream. Past its hello a device can only ask, and a line past
   `MAX_LINE_BYTES` or not an ask ends the connection. Its one write connects or
   disconnects an agent CLI, as the Sources panel would and through the same
   install path; writes run one at a time server-wide, each waiting out
   `MIN_WRITE_GAP` since the last, re-checked against the pairing before it
   starts and logged with a short hash of the device. Connections are capped
   (`MAX_CONNS`), and one that hasn't said hello in time (`HELLO_TIMEOUT`) is dropped.

If you find a way to cross one of these boundaries (e.g. a transcript or hook
payload that escapes the terminal, a non-owner socket connect, an install path
traversal, any outbound connection, or a way into `serve` without pairing),
please report it.

## Reporting a Vulnerability

Report privately via
[GitHub private vulnerability reporting](https://github.com/pixtuoid/pixtuoid/security/advisories/new).

The [org's security policy](https://github.com/pixtuoid/.github/blob/main/SECURITY.md)
sets the response times.

**Do not** open a public issue for security vulnerabilities.
