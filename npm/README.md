# npm distribution

Ships pixtuoid's prebuilt binaries via `npm i -g pixtuoid`, using the
**`optionalDependencies` + per-platform-packages** pattern (esbuild / Biome /
git-cliff). No `postinstall`, no download-on-install, `--ignore-scripts`-safe.

## How it works

```
npm i -g pixtuoid
  └─ pixtuoid (launcher, pure JS — installs everywhere)
       optionalDependencies (npm installs ONLY the one matching os/cpu/libc):
         @pixtuoid/cli-darwin-arm64   os:darwin cpu:arm64
         @pixtuoid/cli-darwin-x64     os:darwin cpu:x64
         @pixtuoid/cli-linux-x64      os:linux  cpu:x64           (static musl, universal)
         @pixtuoid/cli-linux-arm64    os:linux  cpu:arm64 libc:glibc
         @pixtuoid/cli-win32-x64      os:win32  cpu:x64
         @pixtuoid/cli-win32-arm64    os:win32  cpu:arm64
```

Each platform package is a **pure payload** (both native binaries, no `bin`).
The launcher exposes the two commands; its shims resolve the installed platform
package via `require.resolve` and exec the native binary:

- **`bin/pixtuoid.js`** — the TUI launcher. May print an error + exit non-zero if no
  prebuilt binary matches the host.
- **`bin/pixtuoid-hook.js`** — the hook shim. **ALWAYS exits 0, never writes stderr,
  never throws** (invariant #5: never block the agent). A Node hop adds ~tens of
  ms; the zero-hop optimization (the Sources panel targeting the sibling
  native binary directly) is a tracked follow-up.
- **`bin/resolve.js`** — shared platform→package map + the `PIXTUOID_BINARY`
  dev/CI override.

## Files (committed)

| File | Role |
|---|---|
| `pixtuoid/package.json` | launcher manifest (version + the 6 dep pins are `0.0.0` placeholders, stamped at publish) |
| `pixtuoid/bin/{pixtuoid.js,pixtuoid-hook.js,resolve.js}` | the two shims + shared resolver |
| `generate.mjs` | emits the 6 `@pixtuoid/cli-*` packages from prebuilt binaries + stamps the launcher |

The per-platform packages are **generated, not committed** (see `.gitignore`).

## Release flow (the `npm` job in `.github/workflows/release.yml`)

On a stable `v*` tag (skipped for `-rc`/`-win` pre-releases, like crates.io +
homebrew): download the `artifact-<target>` tarballs the build matrix already
produced → extract both binaries per target → `node generate.mjs --version <tag>
--artifacts <dir>` → `npm publish` the 6 platform packages first, the launcher
last. The version is sourced from the git tag (single source of truth, same as
the crates.io publish) — never a separate `package.json`.

## Local dry-run (no publish)

```bash
just build --release                                  # produces target/release/{pixtuoid,pixtuoid-hook}
mkdir -p /tmp/a/aarch64-apple-darwin
cp target/release/pixtuoid target/release/pixtuoid-hook /tmp/a/aarch64-apple-darwin/
node npm/generate.mjs --version 0.6.0 --artifacts /tmp/a   # (needs all 6 targets present; fake the others to test)
# exercise the shim against a real binary without publishing:
PIXTUOID_BINARY="$PWD/target/release" node npm/pixtuoid/bin/pixtuoid.js --version
```

For a full registry round-trip, publish the generated packages to a local
[verdaccio](https://verdaccio.org/) and `npm i -g pixtuoid --registry http://localhost:4873`.
