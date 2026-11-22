# integrations — Raycast extension agent guide

The **Raycast extension** in [`raycast/`](raycast/): a self-contained
TypeScript/Node project, not Rust, and a thin presenter over the
`pixtuoid … --json` CLI contract. Its two commands, `package.json`'s
`commands`, are Manage Sources (connect/disconnect over
`pixtuoid sources|connect|disconnect --json`) and Start Floating Window.
Cross-cutting rules: the root [`AGENTS.md`](../AGENTS.md). This guide sits
here, not in `raycast/`, because the Store review rejects an extension folder
that ships an `AGENTS.md` (`no-agent-scaffolding` in raycast/extensions'
[`.greptile/config.json`](https://github.com/raycast/extensions/blob/main/.greptile/config.json)).

## What it is

- A shell over the CLI that does not bundle the binary: it resolves it through
  the login shell's `$PATH` and a `binaryPath` preference.
- `raycast/src/pixtuoid.ts` is the CLI bridge; `raycast/src/manage-sources.tsx`
  and `raycast/src/start-floating.tsx` are the command UIs.
- No server and no state of its own: every fact comes from the CLI, its
  `--json` rows, and Start Floating Window's ready line, exit code and stderr.

## The wire types are generated

`SourceStatus` and `OutcomeRow` are generated, never hand-typed:

1. The Rust serde types in `crates/pixtuoid/src/sources.rs` emit committed
   JSON Schemas, `raycast/contract/source-status.schema.json` and
   `raycast/contract/outcome-row.schema.json`, through their `schemars`
   derives.
2. `npm run gen:contract` (json-schema-to-typescript) regenerates
   `raycast/src/contract.ts` and `raycast/src/contract-outcome.ts` from those
   schemas.
3. `raycast/src/pixtuoid.ts` re-exports the generated types.

- Never hand-edit `contract.ts` or `contract-outcome.ts`; eslint and Prettier
  ignore them. The root guide's [Build & test](../AGENTS.md#build--test) says
  when to regenerate them; commit every file the regeneration rewrites.
- A producer shape change can't drift by hand, because three gates catch it:
  - the struct↔schema golden tests, `*_schema_matches_the_committed_contract`
    (`just test`);
  - the schema↔type freshness check: raycast CI regenerates both files and
    fails on a diff;
  - the type↔usage check, `tsc --noEmit`.
- At runtime `raycast/src/conforms.ts` checks every CLI row against the same
  committed schemas and fails closed on drift (`npm test`), so a stale schema
  breaks the extension, not only the types.
- `outcome_row_json_shape_is_the_raycast_contract` pins `OutcomeRow`'s exact
  wire JSON; `crates/pixtuoid/tests/cli_json.rs`'s `sources --json` golden and
  the schema test pin `SourceStatus`'s. `OutcomeRow`'s doc comment in
  `sources.rs` owns the published-wire rule.
- `raycast/contract/floating-launch.json`, Start Floating Window's args, usage
  exit and error prefix, is hand-written; `cli_packaging`'s
  `the_floating_launch_contract_holds` pins it against the binary.

## Toolchain policy

`raycast/package.json` cannot carry a comment, so these rules live here.

- Keep toolchain bumps within what Raycast declares; check the peers, don't
  guess:
  - `eslint` and `typescript` stay within the installed
    `@raycast/eslint-config`'s peerDependencies ranges; read them before a
    bump.
  - `@types/node` stays on its current major: `.github/dependabot.yml` ignores
    its major updates, and minors still flow.
  - `@raycast/api`'s exact peer is a warning-level mismatch npm tolerates
    under the committed lockfile, not a pin the manifest must equal.
- `ray build` type-checks with its own bundled tsc (read its version from the
  installed `@raycast/api`), so `raycast/tsconfig.json` stays parseable by
  both that tsc and the local one:
  - `moduleResolution: "Bundler"`;
  - an explicit `types: ["node"]`, since TypeScript 6.0 defaults `types` to
    `[]`
    ([announcement](https://devblogs.microsoft.com/typescript/announcing-typescript-6-0/),
    "`types` now defaults to `[]`");
  - no `ignoreDeprecations` value the bundled tsc rejects.

## Gates

- CI runs `.github/workflows/raycast.yml`'s `check` job on a Linux runner when
  a PR changes a path its `scope` job lists, and `ci-gate` judges it. Run its
  steps locally before calling a change done.
  - `ray build` writes `raycast/raycast-env.d.ts`, the manifest's generated
    `Preferences` that `tsc` reads.
  - `ray lint` validates the manifest, icons and metadata and runs the
    Prettier pass.
- The pre-push hook still runs `just preflight` on a change scoped to
  `raycast/`.
- Install scripts fail closed, as the site's do
  ([its README's Checks](../site/README.md#checks)): `strict-allow-scripts`
  with an exact-version `allowScripts`, enforced in CI by the npm version
  `setup-npm` installs from `site/package.json`'s `packageManager`. Review
  them with `npm install-scripts ls`.
- `npm run audit` is plain `npm audit --audit-level=low`, as the site's is.
  `npm audit` has no per-advisory ignore, so if an unfixable advisory recurs
  here, restore the per-advisory allow-list script
  (`raycast/scripts/audit-adjudicated.mjs`, deleted in #856) rather than lower
  `--audit-level`, which blinds a whole severity band to hide one id.
  - Unfixable is realistic: a transitive chain we own no link of.
  - An `overrides` patch can green the audit over code that throws (#792).
