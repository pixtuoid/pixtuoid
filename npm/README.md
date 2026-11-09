# npm distribution

Ships pixtuoid's prebuilt binaries via `npm i -g pixtuoid`, using the
**`optionalDependencies` + per-platform-packages** pattern (esbuild / Biome /
git-cliff). No `postinstall`, no download-on-install, `--ignore-scripts`-safe.

## How it works

```
npm i -g pixtuoid
  └─ pixtuoid (launcher, pure JS — installs everywhere)
       optionalDependencies, one per generate.mjs TARGETS entry (npm installs
       ONLY the one matching os/cpu):
         @pixtuoid/cli-<os>-<cpu>     named by process.platform / process.arch
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
- **`bin/resolve.js`** — the platform→package naming generate.mjs also uses + the `PIXTUOID_BINARY`
  dev/CI override.

## Files (committed)

| File | Role |
|---|---|
| `pixtuoid/package.json` | launcher manifest (`0.0.0` version placeholder; the version and the platform deps are stamped at publish) |
| `pixtuoid/bin/{pixtuoid.js,pixtuoid-hook.js,resolve.js}` | the two shims + shared resolver |
| `generate.mjs` | emits a `@pixtuoid/cli-*` package per TARGETS entry from prebuilt binaries + stamps the launcher |

The per-platform packages are **generated, not committed** (see `.gitignore`).

## Release flow (the `npm` job in `.github/workflows/release.yml`)

On a stable `v*` tag (skipped for `-rc`/`-win` pre-releases, like crates.io +
homebrew): download the `artifact-<target>` tarballs the build matrix already
produced → extract each target's binaries and notices → `node generate.mjs
--version <tag> --artifacts <dir>` → `npm publish` the platform packages first,
the launcher last. The version is sourced from the git tag (single source of
truth, same as the crates.io publish) — never a separate `package.json`.
