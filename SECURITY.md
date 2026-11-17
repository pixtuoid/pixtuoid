# Security Policy

## Supported Versions

Fixes land on the latest released version; there are no maintained back-branches.
Upgrade to the newest release (`cargo install pixtuoid pixtuoid-hook`, `brew upgrade`,
or on macOS and Windows `npm i -g pixtuoid`) to receive security fixes.

| Version        | Supported |
|----------------|-----------|
| latest         | ✅        |
| older releases | ❌        |

## Privacy posture

pixtuoid is **telemetry-free and local by default**:

- **No outbound connections.** The binary makes none: no analytics, no
  crash-report upload, no update check, no "phone home". A crash writes its
  backtrace to a *local* file and only offers a pre-filled GitHub URL for you to
  open yourself. The dependencies are audited for advisories daily with
  `cargo-deny` (`.github/workflows/audit.yml`).
- **No listener unless you start one.** Only `pixtuoid serve` listens on a
  network. It announces this machine's name and its key's SPKI hash there
  (DNS-SD `_pixtuoid._tcp`), which pairs no one, and gives a device nothing
  until it is paired.
- **Your session data stays on your machine unless you run `pixtuoid serve`.**
  pixtuoid reads your agent CLIs' transcripts (such as `~/.claude/projects` and
  `~/.codex/sessions`) **read-only**, to work out what each sprite is doing.
  Beyond that announcement, only `pixtuoid serve` sends anything, and only to
  paired devices: agent status, model, usage, repo names (a project directory's
  last component), which agent CLIs you have and how each is connected, and
  this machine's name and OS. No path, prompt, tool input or transcript leaves;
  the wire is the allowlist in [`remote.rs`](crates/pixtuoid-core/src/remote.rs).

## Trust boundaries

Each part of pixtuoid that handles untrusted or privileged input, and what
bounds it.

### The hook shim (`pixtuoid-hook`)

Your agent CLI runs it, and it forwards one JSON line from its stdin to the
office over local IPC, never a network.

- **On Unix** it writes to a Unix domain socket at a per-user path:
  `$XDG_RUNTIME_DIR/pixtuoid.sock`, else `/tmp/pixtuoid-<uid>/pixtuoid.sock` in
  a per-user `0700` directory; `PIXTUOID_SOCKET` overrides it. The socket is
  created `0600`, owner-only. The `/tmp` fallback directory is made with a
  TOCTOU-safe `mkdir` and its owner and mode are checked, and the shim checks
  the connected peer's uid (`getpeereid`/`SO_PEERCRED`) before writing. So
  another user can't squat the path to turn the hooks off, or race a listener
  onto it to read the payload.
- **On Windows** it writes to a named pipe, `\\.\pipe\pixtuoid-<user>`,
  with an owner-only DACL. The shim checks that the pipe server's token user
  SID matches its own (`GetNamedPipeServerProcessId`, then a token compare)
  before writing, which closes the same squat-and-read attack in Windows'
  machine-wide pipe namespace (#495).
- **It never blocks the agent.** It exits `0` on any error, and a watchdog
  bounds its send (pixtuoid-hook's `transport::WRITE_TIMEOUT`). It never runs
  or shells out to anything in the payload.

### The office's socket listener

- It binds that owner-only socket with a flock'd, `O_NOFOLLOW`, atomic-rename
  bind, so only the local user can connect.
- A second instance subscribes over it to every payload the owner accepts,
  after checking that the owner is this user. Another process of this user
  could subscribe too, which crosses no boundary: it already runs with this
  user's access.

### What reaches your terminal

- The CLI's plain-text output (the `--headless` summary, the `doctor` report,
  the Sources and install output) **strips control characters** from untrusted
  wire values (`strip_control_chars`), so a crafted transcript can't smuggle
  escape sequences into piped output.
- The live half-block TUI draws into its own cell grid rather than echoing raw
  bytes to the terminal. Its agent labels come from project-directory path
  components, read over the user's own `0600` socket and transcripts.

### Hook installation

It runs when you explicitly *connect* or *disconnect* a source, on this machine
or from a device paired with `pixtuoid serve`. Past that, a launch only rewrites the
plugin files pixtuoid owns, and loads them through a connected CLI's own
subcommand where it has one (`converge_owned`).

- Where the agent CLI has a plugin system (Claude Code, Codex CLI, Hermes
  Agent, Reasonix and Antigravity CLI), pixtuoid installs a local plugin through the
  CLI's own plugin subcommands, so the CLI writes its own settings. For those
  CLIs, connect and disconnect also remove every hook in the CLI's own config
  that runs pixtuoid's shim, hand-written ones included. A hook running a shim
  renamed through `PIXTUOID_HOOK` is removed only when it carries the
  `PIXTUOID_SOURCE` stamp.
- Otherwise pixtuoid edits the CLI's config through one writer, advisory-locked
  and `fsync`ed with an atomic rename, that **keeps the file's permissions,
  follows stow symlinks, and changes only pixtuoid's own entries**. Every other
  setting survives: a YAML or TOML file is edited in place, comments and
  formatting included, and a JSON file is written out again.
- Installs are idempotent and reversible: *disconnect* removes the plugin or
  the hook entries.

### `pixtuoid serve`

The only network listener.

- **Who may connect.** It binds IPv4 on every interface and, before any TLS,
  drops a peer that isn't private, link-local or loopback. That drops a
  Tailscale tailnet's
  [`100.64.0.0/10`](https://tailscale.com/kb/1015/100.x-addresses) too. It
  speaks TLS 1.3 only.
- **Pairing.** Its key is made on first use. A device pins that key's SPKI hash
  from the QR code `pixtuoid pair` prints, beside a one-time code that expires
  (`CODE_LIFETIME`) and is redeemed for a device token. The QR is a link on
  pixtuoid.dev whose fields ride the fragment, which a browser keeps to itself
  ([RFC 3986 §3.5](https://www.rfc-editor.org/rfc/rfc3986#section-3.5)):
  a phone without the app opens a `/pair/` page there that runs no script.
- **What it stores.** The key, the hashes of the code and tokens, and each
  device's name and pairing time sit in one owner-only file in the state
  directory, never in a dotfiles-prone `~/.config`. `pair --forget` forgets one
  device by the id `pair --list` shows, and `pair --reset` every device; either
  ends the device's stream.
- **What a device may do.** After its hello a device can only ask, and a line
  past `MAX_LINE_BYTES` or not an ask ends the connection. Its one write
  connects or disconnects an agent CLI, as the Sources panel would and through
  the same install path. Writes run one at a time server-wide, each waiting out
  `MIN_WRITE_GAP` since the last, checked again against the pairing before it
  starts, and logged with the device's id (`pair --list`).
- **Limits.** Connections are capped (`MAX_CONNS`), each address to one
  connection awaiting its hello, and one that sends no hello in time
  (`HELLO_TIMEOUT`) is dropped.

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
