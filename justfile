# Project task runner. The git hooks, ci*.yml and release.yml call these recipes
# rather than restating their commands, so a command changes in one place;
# `workspace-version` names the one exception.
#
# Recipes are grouped by intent (see `just --list`):
#   rust     — build, test and lint the repo (Rust, shell, workflows), plus the
#              on-demand e2e / capture / fixture recipes
#   site     — the Astro landing page under site/ (npm + the wasm build)
#   gen      — regenerate + check committed artifacts
#   release  — what release.yml builds and checks: the cross builds, the .deb,
#              the version read, and the Node gates (npm-check)
#   meta     — tooling setup, the local gate (preflight), the fixture
#              gates, and the gates' selftests

# One dialect on every platform: bash, strict. Git Bash is preinstalled on GHA
# windows runners, so every recipe stays single-sourced cross-platform
# (ci-tests.yml's windows jobs call recipes, never inline commands).
set shell := ["bash", "-euo", "pipefail", "-c"]

# Recipe arguments reach the shell as "$1".."$@", never as `{{ param }}`: an
# interpolated value is re-parsed as shell code, so a quote or `$(...)` in it
# runs.
set positional-arguments

# ── variables ─────────────────────────────────────────────────────
# just evaluates these globally regardless of position; kept at the top so the
# file's config lives in one place.

# The published API surface: the ONLY two crates whose public API is a contract
# (the binary lib target is not). Single-sourced here so both gates over it —
# api-surface / api-surface-check — can't drift; a newly-published crate is
# added in ONE place. release-plz semver-checks every crate with a library, so
# release-plz.toml switches the check off for the binary's lib target.
PUBLISHED_CRATES := "pixtuoid-core pixtuoid-scene"

# Standalone shell FILES share one authority so formatting and lint coverage
# cannot drift. Shell embedded in YAML is a second population this cannot cover:
# workflow `run:` blocks go to actionlint, composite-action ones to
# `actionlint-composites`. Both are shellcheck-only — shfmt cannot rewrite a
# scalar in place — so adding a file here is not enough for embedded shell.
SHELL_SOURCES := "scripts/lib/*.sh .githooks/* policy/ci-observability/*.sh"

# The tools `lint` refuses to start without: `setup-tools` brew-installs the
# first list and cargo-installs the second.
LINT_BREW_TOOLS := "shfmt actionlint shellcheck zizmor yq jq check-jsonschema gitleaks"
LINT_CARGO_TOOLS := "cargo-machete cargo-deny lychee"

# The nightly the api-surface goldens are pinned to (rustdoc JSON is
# nightly-only). Provisioned by `_api-toolchain`.
API_NIGHTLY := "nightly-2026-07-22"

# The cargo-public-api the api/ goldens are reproducible against — tool-exact:
# a different version rewrites them all. ci-lint.yml installs its own copy;
# `_api-toolchain` asserts the pair, so divergence fails loud instead of
# churning goldens.
API_PUBLIC_API := "0.52.0"

# The cargo-hawk `hawk` runs, and the exact rustc it links (its
# rust-toolchain.toml): hawk refuses any other, so a bump moves both, and the
# installer's hash with them; a workspace `rust-version` past HAWK_RUST fails
# `just hawk` until they move.
HAWK_VERSION := "0.1.15"
HAWK_RUST := "1.99.0"
HAWK_INSTALLER_SHA256 := "4e1b85fc15d3d4efa9b3bd7c62620655cc5504b7164272292043e3794f54111f"

# The non-linux triples `doc-check` renders, one per OS release.yml ships.
# rustdoc links nothing, so a triple's std is all it needs while no dependency on
# it builds C (`cargo doc` still runs build scripts).
DOC_TARGETS := "x86_64-pc-windows-msvc aarch64-apple-darwin"

# The zone every test run reads the clock in, so a test anchored to its
# writer's own zone fails on their machine, not first in CI (#1377). Its +05:45
# is neither UTC nor a whole hour, so epoch and local hours never coincide.
# chrono reads `TZ` on Unix only (chrono 0.4.45 `src/offset/local/unix.rs:92`);
# Windows tests keep the runner's own zone.
TEST_TZ := "Asia/Kathmandu"

# List available recipes.
default:
    @just --list

# ── rust ──────────────────────────────────────────────────────────

# Format check only — fast, gates pre-commit. The justfile has a canonical
# format too (`just --fmt`), so it is checked with the Rust; just gives its
# formatter no cross-version guarantee, so `setup-just` pins CI's just and a
# local one on another version may disagree.
[group('rust')]
fmt-check:
    cargo fmt --all --check
    just --fmt --check

# Apply formatting in place.
[group('rust')]
fmt:
    cargo fmt --all
    just --fmt

# Pairs with the shellcheck house rule: shellcheck lints, shfmt formats. `-i 4`
# (4-space) matches the prevailing style; no `-ci` so case bodies stay
# un-indented as written.
[doc('Shell-format check over repository shell sources')]
[group('rust')]
shfmt-check:
    shfmt -i 4 -d {{ SHELL_SOURCES }}

[doc('Apply shfmt formatting in place over repository shell sources')]
[group('rust')]
shfmt-fix:
    shfmt -i 4 -w {{ SHELL_SOURCES }}

[doc('Run shellcheck over repository shell sources')]
[group('rust')]
shellcheck:
    shellcheck {{ SHELL_SOURCES }}

# Lint the GitHub Actions workflows (actionlint): YAML schema, expression types,
# action input/output names, runner labels, AND shellcheck over every `run:`
# block (so a shell bug inside a workflow is caught at author time, not on a red
# main). Needs shellcheck on PATH for the run-block checks.
[doc('Lint the GitHub Actions workflows (actionlint + shellcheck over run: blocks)')]
[group('rust')]
actionlint:
    actionlint

# The blind spot the recipe above cannot cover: actionlint models WORKFLOWS, so
# it discovers only .github/workflows and rejects an action.yml outright
# ("jobs section is missing"). Shell that moves from a workflow into a composite
# action therefore loses its shellcheck coverage silently. Pull each `run:` out
# ourselves and check it with the same linter.
[doc('Shellcheck every run: block inside the composite actions (actionlint cannot parse action.yml)')]
[group('rust')]
actionlint-composites:
    #!/usr/bin/env bash
    set -euo pipefail
    shopt -s nullglob
    actions=(.github/actions/*/action.y*ml) # GitHub accepts action.yaml too
    ((${#actions[@]})) || { echo "error: no composite actions found" >&2; exit 1; }
    work="$(mktemp -d)"
    trap 'rm -rf "$work"' EXIT
    checked=0
    skipped=()
    for action in "${actions[@]}"; do
        count="$(yq '[.runs.steps[] | select(has("run"))] | length' "$action")"
        ((count)) || continue # a pure `uses:` composite has no shell to check
        for i in $(seq 0 $((count - 1))); do
            # The default should never fire — a composite run step must name a
            # shell — but bash is what actionlint assumes for a workflow step.
            shell="$(yq -r ".runs.steps | map(select(has(\"run\"))) | .[$i].shell // \"bash\"" "$action")"
            case "$shell" in
            bash | sh) ;;
            # pwsh/python are not shellcheck's to judge, but a bounded gate that
            # does not name what it dropped reads as full coverage.
            *)
                skipped+=("$action run step $i ($shell)")
                continue
                ;;
            esac
            script="$work/$(echo "$action" | tr /. __)-$i.$shell"
            { echo "#!/usr/bin/env $shell"; yq -r ".runs.steps | map(select(has(\"run\"))) | .[$i].run" "$action"; } >"$script"
            shellcheck -s "$shell" "$script" || { echo "  ^ from $action run step $i" >&2; exit 1; }
            checked=$((checked + 1))
        done
    done
    ((checked > 0)) || { echo "error: no composite run: blocks were checked" >&2; exit 1; }
    echo "$checked composite run: blocks shellchecked"
    ((${#skipped[@]})) && printf '  skipped (not a shellcheck dialect): %s\n' "${skipped[@]}"
    exit 0

# Security audit for workflows/actions/Dependabot. zizmor owns the parser and
# audit catalog; .github/zizmor.yml records its deliberate exceptions, and every
# other accepted finding is suppressed at its exact source location with a WHY.
# The operating MODE is env-derived, not chosen here, and the asymmetry is
# deliberate: tokenless it runs OFFLINE (it says so on stderr) and skips every
# audit that needs the GitHub API (`RUST_LOG=debug zizmor` names each;
# typosquat-uses still runs, at reduced confidence). ci-lint.yml's hygiene job passes
# GH_TOKEN, so those DO gate in CI: there the recipe refuses to run tokenless.
# Same call as `links` (--offline) and `deny` (advisories in `deny-advisories`): a
# check whose verdict depends on the network and an upstream feed must not
# redden a push of unchanged code. Do NOT auto-export `gh auth token` to close
# the gap — it puts a real token on the wire on every pre-push run and makes the
# local gate depend on gh auth + API rate limits, the exact flakiness those two
# siblings were written to avoid.
[doc('Audit GitHub automation security with zizmor')]
[group('rust')]
zizmor:
    @if [ -n "${GITHUB_ACTIONS:-}" ] && [ -z "${GH_TOKEN:-}" ]; then \
        echo "error: zizmor would run offline in CI and skip its online audits; give this step a GH_TOKEN" >&2; \
        exit 1; \
    fi
    # Explicit: a linked worktree's discovery finds the MAIN checkout's config.
    zizmor --config .github/zizmor.yml --strict-collection .

# action_behavior_test.sh runs the workflows' own shell against stubs, which no
# static contract can do.
[doc('Check the CI contracts actionlint and zizmor cannot see')]
[group('rust')]
ci-observability:
    bash policy/ci-observability/check.sh
    bash policy/ci-observability/action_behavior_test.sh

# Every committed JSON Schema, held to the metaschema. These are contracts a
# consumer reads at runtime — the raycast ones pin the `--json` shape — and
# nothing else parses them: a broken one is invisible until the consumer
# refuses to start.
[doc('Validate every committed JSON Schema against the metaschema (check-jsonschema)')]
[group('rust')]
json-schemas:
    #!/usr/bin/env bash
    set -euo pipefail
    shopt -s nullglob
    schemas=(integrations/raycast/contract/*.schema.json)
    ((${#schemas[@]})) || { echo "error: no committed JSON Schemas found" >&2; exit 1; }
    check-jsonschema --check-metaschema "${schemas[@]}"
    echo "${#schemas[@]} JSON Schemas validated"

# Link + anchor check (lychee) over the tree's markdown, hidden dirs included
# (lychee skips them by default) and .gitignore respected: every relative link
# must resolve and every `#anchor` exist. `--offline`, so external-URL decay is
# not gated (it's flaky on the PR path).
[doc('Offline link + anchor check (lychee) over the repo markdown — no network, .gitignore-aware')]
[group('rust')]
links:
    # Source CSS uses Vite package specifiers; its module graph belongs to the
    # site build, while this gate owns documentation links and anchors.
    lychee --offline --include-fragments --extensions md --hidden .

# Clippy across the workspace, warnings denied.
[group('rust')]
clippy:
    cargo clippy --workspace --all-targets -- -D warnings

# Unused-dependency check.
[group('rust')]
machete:
    cargo machete

# License + supply-chain gate (bans/licenses/sources). Advisories are NOT here:
# an overnight RustSec advisory must not block a push of unchanged code.
[group('rust')]
deny:
    cargo deny check bans licenses sources

# Advisories judge the whole graph, so they run where it or its policy changes
# (ci-lint's deny job) and on audit.yml's schedule.
[group('rust')]
deny-advisories:
    cargo deny check advisories

# Architecture invariant #1, mechanized: pixtuoid-core + pixtuoid-scene depend
# on only the crates deny-arch.toml lists.
[group('rust')]
arch:
    #!/usr/bin/env bash
    set -euo pipefail
    # The crate boundary already makes this a COMPILER fact; the allowlist pins
    # it at the dep-tree level too (a transitive pull-in via a feature would slip
    # past the boundary), and a crate nobody listed fails instead of passing
    # unseen, as a denylist's omission does. deny-arch.toml says how it sees
    # every feature and target, the point a bare `cargo tree` misses.
    cargo deny --config deny-arch.toml check bans
    # An empty `allow` permits everything, and windows-sys, core's cfg(windows)
    # dependency, proves the graph spans targets: the same list minus it must fail.
    trimmed="$(mktemp)"
    trap 'rm -f "$trimmed"' EXIT
    grep -v '^    "windows-sys",$' deny-arch.toml >"$trimmed"
    if out="$(cargo deny --config "$trimmed" check bans 2>&1)" || ! grep -Fq "crate 'windows-sys = " <<<"$out"; then
        echo "ARCH SELFTEST: deny-arch.toml without windows-sys did not fail on windows-sys"; exit 1
    fi
    echo "arch: pixtuoid-core + pixtuoid-scene depend only on deny-arch.toml's crates"

# Fast, independent lint checks in parallel.
[group('rust')]
lint:
    #!/usr/bin/env bash
    set -euo pipefail
    # Fail fast with an actionable message when a lint tool is missing, instead
    # of a bare `command not found` (exit 127) buried in a parallel job's log.
    missing=()
    for t in {{ LINT_BREW_TOOLS }} {{ LINT_CARGO_TOOLS }}; do
        command -v "$t" &>/dev/null || missing+=("$t")
    done
    if (( ${#missing[@]} )); then
        printf 'error: missing lint tool(s): %s — run `just setup-tools`\n' "${missing[*]}" >&2
        exit 1
    fi
    # Per-check logs; dump only the failures so a green run stays quiet.
    tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
    run() { local n="$1"; shift; if "$@" >"$tmp/$n.log" 2>&1; then printf '  \033[32m✓ %s\033[0m\n' "$n"; else printf '  \033[31m✗ %s\033[0m\n' "$n"; cat "$tmp/$n.log"; return 1; fi; }
    pids=(); fail=0
    run fmt     just fmt-check          & pids+=($!)
    run genart  just gen-art-check       & pids+=($!)
    run genbanner just gen-banner-check  & pids+=($!)
    run machete just machete            & pids+=($!)
    run deny    just deny                & pids+=($!)
    run arch    just arch                & pids+=($!)
    run shfmt   just shfmt-check         & pids+=($!)
    run shell   just shellcheck           & pids+=($!)
    run actions just actionlint          & pids+=($!)
    run composites just actionlint-composites & pids+=($!)
    run zizmor  just zizmor              & pids+=($!)
    run ci-obs  just ci-observability     & pids+=($!)
    run schemas just json-schemas         & pids+=($!)
    run links   just links               & pids+=($!)
    run tuidrive just tuidrive-selftest   & pids+=($!)
    run e2escrub just e2e-scrub-selftest  & pids+=($!)
    run starhist just star-history-selftest & pids+=($!)
    run bmcbtn  just bmc-button-selftest  & pids+=($!)
    run fixpii  just fixture-pii          & pids+=($!)
    run piiself just fixture-pii-selftest & pids+=($!)
    for p in "${pids[@]}"; do wait "$p" || fail=1; done
    [[ $fail -eq 0 ]]

# The regen recipes call it too. No plain `cargo test` fallback: its shared
# process and nextest's per-test processes pass different suites (#1104's omp
# hang showed under only one), so a fallback runs a suite CI never ran. The
# recipe adds no `--workspace`, which would override a caller's `-p`; a caller
# meaning every member passes it.
[doc('Run the tests under cargo-nextest; forwards args (e.g. -p <crate> <filter>)')]
[group('rust')]
test *args:
    @cargo nextest --version &>/dev/null || { echo 'error: cargo-nextest is not installed — run `just setup-tools`' >&2; exit 1; }
    @TZ={{ TEST_TZ }} cargo nextest run "$@"

# The filter forwards to every target, and one matching nothing in a target is
# not an error: `just bench 360` runs every 360x240 case, `just bench hook` only
# the hook-transport fold.
[doc('Render-path + wire-path criterion benchmarks; forwards a filter')]
[group('rust')]
bench *args:
    cargo bench -p pixtuoid-scene --bench render_frame -- "$@"
    cargo bench -p pixtuoid-core --bench decode_reduce -- "$@"
    cargo bench -p pixtuoid --no-default-features --bench render_tiles -- "$@"

[doc('Frame pacing through the real TUI painter per protocol: frame time, budget overruns, interval jitter, bytes per frame')]
[group('rust')]
bench-pacing:
    cargo run --release -p pixtuoid --example pacing -- target/pacing/report.json

# Local only, never CI: a shared runner's wall clock is too noisy for a frame
# budget. `--live` runs in this terminal, the only run a real parser sees.
[doc('The fluency gate: the release binary through a transition into a storm (or dusk); fail when frames shown past their interval pass HITCH_RATE_MS_PER_S in scripts/pace-check.py')]
[group('rust')]
pace-check *args:
    cargo build --release -p pixtuoid --bins --example pacing
    python3 scripts/pace-check.py "$@"

# Catches code that silently only builds, or is only used, with `native` or
# `audio` on. `--no-dev-deps` builds no test, so scene's no-default tests
# lint and run on their own.
[doc('Feature-powerset clippy — every feature subset compiles warning-free; scene no-default tests pass')]
[group('rust')]
hack:
    #!/usr/bin/env bash
    set -euo pipefail
    command -v cargo-hack &>/dev/null || { echo "error: cargo-hack not found — run \`just setup-tools\`" >&2; exit 1; }
    cargo hack --feature-powerset --no-dev-deps clippy --workspace -- -D warnings
    cargo clippy -p pixtuoid-scene --no-default-features --all-targets -- -D warnings
    just test -p pixtuoid-scene --no-default-features
    # The powerset builds no example or test, and the drift watch's compile only under `upstream`.
    cargo clippy -p pixtuoid --no-default-features --features upstream --lib --tests --example drift_watch -- -D warnings

# Same toolchain gotcha as `api-surface` and `gen-wasm`, and it bites HARDER
# here because the compiler's own advice is wrong: a Homebrew cargo ahead of the
# rustup proxy on PATH ships only the host std, so the cross-lint dies on E0463
# "can't find crate for `core`" while suggesting `rustup target add
# x86_64-pc-windows-msvc` for a target rustup already has. Prepending the proxy
# (a no-op on CI, where it is already first) fixes it; the explicit preflight
# then owns the genuinely-missing case with an accurate message. It catches
# cfg(windows)-only compile and lint errors locally; a string path assert still
# fails only in `windows-test`.
[doc('Cross-lint the workspace for x86_64-pc-windows-msvc via clippy (no linking; ubuntu runner suffices)')]
[group('rust')]
check-windows:
    #!/usr/bin/env bash
    set -euo pipefail
    export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
    rustup target list --toolchain stable --installed | grep -q x86_64-pc-windows-msvc \
        || { echo "needs the target: rustup target add x86_64-pc-windows-msvc" >&2; exit 1; }
    cargo clippy --workspace --all-targets --target x86_64-pc-windows-msvc -- -D warnings

# The other-unix arms compile on none of the OSes release.yml ships, so only this
# builds them. `portable` instead of the defaults: `audio`'s `alsa-sys` build
# script can't probe a cross target.
[doc('Cross-lint the workspace for x86_64-unknown-freebsd, the stand-in for every other unix (no linking)')]
[group('rust')]
check-other-unix:
    #!/usr/bin/env bash
    set -euo pipefail
    # rustup's proxy cargo, so `--target` finds the std added below (see `check-windows`).
    export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
    target=x86_64-unknown-freebsd
    rustup target list --installed | grep -qx "$target" || rustup target add "$target"
    cargo clippy --workspace --all-targets --target "$target" --no-default-features --features pixtuoid/portable -- -D warnings

# Catches a dep bump (or newer stdlib use) that silently raises the floor past
# the version we advertise to crates.io consumers of pixtuoid-core. CI-only in
# practice (installs a pinned toolchain + a full check), NOT in preflight.
# Reads the version from Cargo.toml so there's one source of truth.
[doc('Check the workspace builds on the declared MSRV (rust-version in Cargo.toml)')]
[group('rust')]
msrv:
    #!/usr/bin/env bash
    set -euo pipefail
    msrv="$(grep -m1 '^rust-version' Cargo.toml | sed -E 's/.*"([0-9]+\.[0-9]+(\.[0-9]+)?)".*/\1/')"
    echo "declared MSRV: $msrv"
    rustup toolchain install "$msrv" --profile minimal --no-self-update >/dev/null 2>&1 || true
    # Clear RUSTFLAGS so the default linker is used: this gate verifies
    # COMPILATION on the floor and must not also require the lld that
    # `.cargo/config.toml` pins for x86_64 Linux. (RUSTFLAGS env overrides
    # target.*.rustflags wholesale.)
    RUSTFLAGS="" rustup run "$msrv" cargo check --workspace

# Reproduce release-plz's semver verdict LOCALLY. Not a gate and not in CI:
# release-plz runs cargo-semver-checks itself on the release PR and RAISES the
# bump when it finds a break, so this exists only so a human can see the same
# answer before dispatching. Needs network for the baseline crates, and
# cargo-semver-checks on PATH (`cargo binstall cargo-semver-checks`).
[doc("Reproduce release-plz's semver verdict for the published crates (local, not a gate)")]
[group('rust')]
semver:
    cargo semver-checks $(printf -- '--package %s ' {{ PUBLISHED_CRATES }})

# Public-API surface snapshot for the PUBLISHED libraries. COMPLEMENTS
# release-plz's own semver check: that answers "is the release bump enough?" on
# the release PR, this shows *what* changed as a reviewable golden diff at
# review time. Goldens live in `api/<crate>.txt` —
# `cargo public-api -s` output (`-s` omits blanket-impl noise like
# `Into`/`Receiver`; auto-derived `Clone`/`Serialize`/… STAY, since
# adding/removing a derive IS a public-API change). cargo-public-api takes one
# crate per call, so a golden file is regenerated per crate. rustdoc JSON is
# nightly-only, so it PINS `API_NIGHTLY`. `check` diffs instead of writing; CI
# runs it as `api-surface-check`.
[doc('Regenerate the api/<crate>.txt public-API goldens (cargo-public-api + pinned nightly); `check` diffs them')]
[group('rust')]
api-surface mode="": _api-toolchain
    #!/usr/bin/env bash
    set -euo pipefail
    mode="$1"
    case "$mode" in
    "") out=api ;;
    check) out=$(mktemp -d); trap 'rm -rf "$out"' EXIT ;;
    *) echo "usage: just api-surface [check]" >&2; exit 2 ;;
    esac
    # cargo-public-api only honors RUSTUP_TOOLCHAIN when the invoked `cargo` is
    # the rustup PROXY. A Homebrew/system cargo ahead of it on PATH ignores the
    # env, so cargo-public-api falls back to rust-toolchain.toml's STABLE pin and
    # dies on `-Z` (nightly-only). Prepend the rustup bin so the proxy wins (a
    # no-op on CI, where it's already first).
    export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
    fail=0
    for crate in {{ PUBLISHED_CRATES }}; do
        RUSTUP_TOOLCHAIN={{ API_NIGHTLY }} cargo public-api -p "$crate" -s > "$out/$crate.txt"
        if [ "$out" != api ] && ! diff -u "api/$crate.txt" "$out/$crate.txt"; then
            echo "error: public API of $crate drifted from api/$crate.txt — run 'just api-surface' and commit the update" >&2
            fail=1
        fi
    done
    exit "$fail"

[doc("Fail if a published crate's public API drifted from the api/ goldens (CI-only)")]
[group('rust')]
api-surface-check: (api-surface "check")

# Both halves of the goldens' reproducibility contract: refuse a mismatched
# cargo-public-api, then self-provision the pinned nightly (rustdoc JSON is
# nightly-only) if it isn't already installed. The minimal profile carries
# rustdoc (bundled with rustc), all cargo-public-api needs.
[private]
_api-toolchain:
    #!/usr/bin/env bash
    set -euo pipefail
    have="$(cargo public-api --version 2>/dev/null | awk '{print $NF}' || true)"
    if [ -z "$have" ]; then
        echo "error: cargo-public-api not installed — goldens need {{ API_PUBLIC_API }} (just setup-tools)" >&2
        exit 1
    fi
    if [ "$have" != "{{ API_PUBLIC_API }}" ]; then
        echo "error: cargo-public-api $have installed, goldens need {{ API_PUBLIC_API }} (just setup-tools)" >&2
        exit 1
    fi
    command -v rustup >/dev/null || { echo "rustup not found — install {{ API_NIGHTLY }} manually for api-surface" >&2; exit 1; }
    rustup toolchain list | grep -q '{{ API_NIGHTLY }}' && exit 0
    echo "installing {{ API_NIGHTLY }} (api-surface needs nightly rustdoc JSON)…" >&2
    rustup toolchain install {{ API_NIGHTLY }} --profile minimal

# `pixtuoid_web`'s `pub` surface is the wasm exports the site's JS calls,
# outside the workspace: hawk's `--exclude-crate` case.
[doc('Fail on a `pub` item no other workspace crate needs (cargo-hawk; the full tier gates on it); forwards args, e.g. --fix')]
[group('rust')]
hawk *args: _hawk-toolchain
    #!/usr/bin/env bash
    set -euo pipefail
    # rustup's proxy cargo, so `+{{ HAWK_RUST }}` selects the toolchain (see `check-windows`).
    export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
    bin="$PWD/target/cargo-hawk-{{ HAWK_VERSION }}"
    if [ ! -x "$bin/cargo-hawk" ]; then
        installer="$(mktemp)"
        trap 'rm -f "$installer"' EXIT
        curl --proto '=https' --tlsv1.2 -fsSL -o "$installer" \
            "https://github.com/astral-sh/hawk/releases/download/{{ HAWK_VERSION }}/cargo-hawk-installer.sh"
        # The installer pins each archive's hash; this pins the installer.
        echo "{{ HAWK_INSTALLER_SHA256 }}  $installer" | shasum -a 256 -c - >/dev/null
        CARGO_HAWK_UNMANAGED_INSTALL="$bin" sh "$installer" >&2
    fi
    # The `-A`: AGENTS.md's Visibility rule stops at `pub(crate)`, never private.
    PATH="$bin:$PATH" cargo +{{ HAWK_RUST }} hawk check --target-dir target/hawk \
        --exclude-crate pixtuoid_web -D warnings -A hawk::unnecessary_restricted_visibility "$@"

[private]
_hawk-toolchain:
    #!/usr/bin/env bash
    set -euo pipefail
    rustup toolchain list | grep -q '^{{ HAWK_RUST }}-' \
        || rustup toolchain install {{ HAWK_RUST }} --profile minimal --no-self-update >&2

# Doc-rendering gate. Two things `cargo build`/`clippy`/`nextest` can't see:
# (1) build every item's docs, private ones included, with EVERY rustdoc
# warning as an error — rustdoc resolves links only on the items it renders, so
# a public-only build lets a private item's links rot unseen, while
# `private_intra_doc_links` still fires on a public doc naming a private item
# (the link docs.rs would render broken). The broken/private intra-doc-link
# classes are already `deny` in `[workspace.lints.rustdoc]`; `-D warnings` adds
# bare URLs, invalid HTML, redundant links, and any future rustdoc lint. "Every
# item" spans every unit rustdoc renders: the `pixtuoid` bin, the examples, and
# each `DOC_TARGETS` triple, whose `cfg` arms the host pass compiles out
# (`cfg(target_os = "linux")` arms render only in CI); (2) RUN the doctests —
# nextest does not.
[doc('Doc gate: cargo doc (private items, bin, examples, DOC_TARGETS) with -D warnings + the doctests nextest skips (CI-only)')]
[group('rust')]
doc-check: _doc-targets
    #!/usr/bin/env bash
    set -euo pipefail
    # rustup's proxy cargo, so `--target` finds the std `_doc-targets` added (see `check-windows`).
    export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
    doc() { RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items "$@"; }
    host="$(rustc -vV | sed -n 's/^host: //p')"
    for target in "" {{ DOC_TARGETS }}; do
        [ "$target" = "$host" ] && continue # the "" pass already rendered it
        # The `pixtuoid` bin shares its lib's name, so the workspace pass skips it
        # (cargo still warns the two share one output path: cargo#6313).
        doc -p pixtuoid --bin pixtuoid ${target:+--target "$target"}
        doc --workspace ${target:+--target "$target"}
    done
    doc --workspace --examples
    cargo test --doc --workspace

[private]
_doc-targets:
    #!/usr/bin/env bash
    set -euo pipefail
    command -v rustup >/dev/null || { echo "rustup not found — add the std for {{ DOC_TARGETS }} manually for doc-check" >&2; exit 1; }
    installed="$(rustup target list --installed)"
    for target in {{ DOC_TARGETS }}; do
        grep -qx "$target" <<<"$installed" || rustup target add "$target"
    done

[doc('Coverage + JUnit XML — ci-tests.yml runs it on every tier with its matrix `features` as args, the full one uploading it (needs llvm-cov + nextest)')]
[group('rust')]
coverage *args:
    TZ={{ TEST_TZ }} cargo llvm-cov nextest --workspace --codecov --output-path codecov.json --profile ci "$@"

# Injects bugs into the CHANGED lines and checks the tests catch them — the
# "do your assertions have TEETH?" dimension that
# line/region coverage can't see (a covered-but-toothless assertion). DIFF-scoped
# (`--in-diff` vs `$MUTANTS_BASE`, default origin/main) so cost scales with the
# change, not the whole tree; reads `.cargo/mutants.toml` (nextest + the
# untestable/timing exclusions). ADVISORY — CI runs it NON-blocking; a surviving
# mutant is a hint to strengthen a test, not a merge gate. Run on a
# reducer/decoder/layout PR; forwards args (e.g. `just mutants --list`). Needs
# cargo-mutants + nextest.
[doc('Mutation-test the diff vs origin/main (cargo-mutants --in-diff) — advisory')]
[group('rust')]
mutants *args:
    #!/usr/bin/env bash
    set -euo pipefail
    base="${MUTANTS_BASE:-origin/main}"
    mkdir -p target
    git diff "$base...HEAD" > target/mutants.diff
    # Gate on the MUTANT COUNT, not on `.rs` changes. cargo-mutants has no
    # --error-on-zero and exits 0 having tested nothing when the diff yields no
    # mutants — a vacuous green reading as "teeth verified". A `.rs`-changes
    # check is NOT sufficient: a diff of test files plus `exclude_globs` entries
    # yields zero mutants and passes it. `--list` enumerates without running
    # (sub-second), so the pre-check is cheap.
    #
    # A FAILING tool and an empty result are reported separately. Folding them
    # sends the reader to inspect their diff when the real cause is a missing
    # cargo-mutants or an unparseable .cargo/mutants.toml — misdirection is the
    # failure class this gate exists to remove, so it must not commit it.
    if ! listed=$(cargo mutants --in-diff target/mutants.diff --list 2>/dev/null); then
        echo "error: \`cargo mutants --list\` failed — the mutant count is unknown." >&2
        echo "  Usually a missing cargo-mutants (\`cargo binstall cargo-mutants\`) or an" >&2
        echo "  unparseable .cargo/mutants.toml. Rerunning with stderr shown:" >&2
        cargo mutants --in-diff target/mutants.diff --list >/dev/null || true
        exit 1
    fi
    if [ -z "$listed" ]; then
        echo "error: the diff vs $base yields ZERO mutants — nothing would be tested." >&2
        echo "  Either there are no .rs changes, or every changed .rs is test code" >&2
        echo "  or excluded by .cargo/mutants.toml (exclude_globs OR exclude_re —" >&2
        echo "  the latter can empty a file's mutants function by function)." >&2
        echo "  Run from a branch touching mutable production Rust, or set MUTANTS_BASE." >&2
        exit 1
    fi
    TZ={{ TEST_TZ }} cargo mutants --in-diff target/mutants.diff "$@"

# Record a conformance fixture from bytes a real CLI actually sent. Hook-only
# sources have no persistent corpus — hook events are transient — so their
# fixtures are the ONLY wire evidence they have, and the ones this tree has not
# re-recorded yet were composed by hand. One BILLED model turn per run.
# `{prompt}` expands to the shared scenario prompt; a custom one is a quoted arg.
#   just capture-fixture cursor tool-run cursor-agent -p --trust '{prompt}'
#   just capture-fixture kimi permission-flow "$SHELL"   # drive the TUI yourself
[doc('Record a conformance fixture from a real CLI run (BILLED — one model turn)')]
[group('rust')]
capture-fixture source scenario *cmd:
    cargo run --release -q -p pixtuoid-core --example capture_fixture -- "$@"

[doc('Strip every committed fixture of what no decoder reads, in place (local, unbilled)')]
[group('rust')]
restrip-fixtures:
    cargo run --release -q -p pixtuoid-core --example capture_fixture -- --strip-corpus

# The corpus census, every transcript-bearing source in one pass — the drift half
# of the pair: fixtures catch a decode regression, real bytes catch the wire
# changing under us. Roster and roots both come from the registry.
[doc('Census every transcript-bearing source against its real local corpus')]
[group('rust')]
corpus-all:
    #!/usr/bin/env bash
    set -uo pipefail
    cc=target/release/examples/corpus_check
    [ -x "$cc" ] || { echo "run: just build --release --examples" >&2; exit 2; }
    # Read the roster BEFORE the loop: as a process substitution its exit status
    # is unobservable, so a roster that dies would feed an empty loop and the
    # census would report "everything clean" having censused nothing.
    roster="$("$cc" --roster)" || { echo "corpus_check --roster failed" >&2; exit 2; }
    [ -n "$roster" ] || { echo "corpus_check --roster returned no rows" >&2; exit 2; }
    rc=0
    n=0
    uncovered=()
    while IFS=$'\t' read -r id _ kind _; do
        [ "$kind" = transcript ] || continue
        n=$((n + 1))
        echo "── $id"
        "$cc" "$id"
        # 3 is "no corpus on this host" — never ran that CLI here. It is NOT a
        # defect, and it must not read as covered either, so it is reported apart
        # from both.
        case $? in
        0) ;;
        3) uncovered+=("$id") ;;
        *) rc=1 ;;
        esac
    done <<<"$roster"
    # `-n "$roster"` checked the TEXT; this checks the PARSE. A roster that is
    # non-empty but yields zero transcript rows — a column insertion, a rename of
    # the `transcript` literal — reproduces the exact silent-empty census the
    # guard above was added for.
    [ "$n" -gt 0 ] || { echo "roster parsed 0 transcript rows — column layout changed?" >&2; exit 2; }
    if [ ${#uncovered[@]} -gt 0 ]; then
        echo "NOT COVERED (no local corpus): ${uncovered[*]}"
    fi
    exit "$rc"

# Never-panic fuzz ONE source's transcript decoder over a JSONL corpus DIR
# (on-demand; not in preflight/CI — points at local or public real sessions, not
# committed data). SOURCE is a registered source name (see `registered_source_names`):
# every line is routed through THAT source's registry line_decoder — no shape
# guessing, so a newer source can't be silently misrouted to decode_cc_line.
# Exits non-zero on any panic. Examples:
#   just fuzz claude-code ~/.claude/projects   # your CC sessions (newest formats)
#   just fuzz codex ~/.codex/sessions          # your Codex rollouts
#   just fuzz grok ~/.grok/sessions            # grok ACP transcripts
#   just fuzz omp ~/.omp/agent/sessions        # omp sessions
#   # a PUBLIC real-session corpus, so drift shows up without waiting for your own
#   # sessions to hit the shape. Its codex samples are single .json objects,
#   # which this recipe's *.jsonl glob does not admit.
#   git clone --depth 1 https://github.com/daaain/claude-code-log /tmp/ccl && just fuzz claude-code /tmp/ccl/dev-docs/messages
[doc('Never-panic fuzz a source decoder over a JSONL corpus dir: just fuzz claude-code ~/.claude/projects')]
[group('rust')]
fuzz source dir:
    #!/usr/bin/env bash
    set -euo pipefail
    source="$1"
    dir="$2"
    [ -d "$dir" ] || { echo "error: corpus dir '$dir' does not exist" >&2; exit 1; }
    # Guard the corpus BEFORE fuzzing: a dir with no .jsonl feeds the fuzzer
    # zero lines, and it exits 0 — reporting the never-panic contract verified
    # having tested nothing.
    [ -n "$(find "$dir" -name '*.jsonl' -print -quit)" ] || { echo "error: no .jsonl files under '$dir' — nothing to fuzz" >&2; exit 1; }
    cargo build --release --example decoder_fuzz -p pixtuoid-core
    find "$dir" -name '*.jsonl' -print0 | xargs -0 cat | ./target/release/examples/decoder_fuzz "$source"

# Hermetic OpenClaw daemon live-e2e: drives the REAL shim with crafted gateway
# envelopes on an isolated socket and asserts the lobster's
# idle/busy/degraded/down via the headless `daemons=` line. Zero gateway, zero
# model calls. Same on-demand local tier as `fuzz` — it needs a release build
# and an ExitWatch backend (macOS kqueue / Linux pidfd), so it is not a CI gate.
[doc('Hermetic OpenClaw daemon live-e2e (needs `just build --release`)')]
[group('rust')]
openclaw-e2e:
    scripts/lib/tier-openclaw-hermetic.sh

# N REAL `openclaw gateway run` processes, each in its own throwaway
# OPENCLAW_HOME on its own port, feeding one headless pixtuoid: one
# `openclaw@<port>` row per gateway, instance-local death, and OpenClaw's OWN
# `plugins list` confirming our plugin loads. Zero model calls, zero account
# footprint, but it needs a real `openclaw` on PATH — same on-demand local tier
# as `openclaw-e2e`. Ports are forwarded (default: the script's `PORTS`).
[doc('Multi-gateway live-e2e against the REAL openclaw CLI (needs `just build --release`)')]
[group('rust')]
openclaw-multi-e2e *ports:
    scripts/lib/tier-openclaw-multi.sh "$@"

# The EXPENSIVE one: a real `openclaw gateway run` PLUS one real model turn on
# the claude-cli backend, proving the gateway's lobster and its backend's `cc·`
# desk sprite coexist live. Real account footprint (your gateway's channels
# connect) and it bills a turn, so no CI job runs it: a change to the headless
# `agents=`/`daemons=` line it greps breaks it silently until it is run by hand.
[doc('OpenClaw + claude-cli backend live-e2e — REAL gateway AND one BILLED model turn')]
[group('rust')]
openclaw-backend-e2e:
    scripts/lib/tier-openclaw-backend.sh

# The broadest tier: launches each installed agent CLI non-interactively and
# asserts ITS badge renders. One real model turn PER CLI, on each provider's own
# account — the only proof a real CLI's real output reaches a real sprite.
[doc('Live multi-source e2e — every installed agent CLI, one BILLED turn each')]
[group('rust')]
live-sources *ids:
    scripts/lib/tier-live-sources.sh "$@"

# Two builds: one would unify the examples' `harness` feature into the binary
# under test.
[doc('Build what `just hermetic` runs, and run no CLI')]
[group('rust')]
hermetic-build:
    cargo build --release -q -p pixtuoid -p pixtuoid-hook
    cargo build --release -q -p pixtuoid-core --example hermetic --example capture_fixture --example bump

[doc('Run each profiled agent CLI against a scripted model (aimock) and judge what pixtuoid made of it')]
[group('rust')]
hermetic *args: hermetic-build
    cargo run --release -q -p pixtuoid-core --example hermetic -- "$@"

# The nightly's bump leg: snapshots rewritten for the re-recorded fixtures,
# then judged as CI judges them.
[doc('Re-record a source at a release, moving its pin, and write the bump as a patch')]
[group('rust')]
hermetic-bump source release out: (hermetic "--cli" release "--record" source)
    TZ={{ TEST_TZ }} INSTA_UPDATE=always INSTA_FORCE_PASS=1 cargo test -q -p pixtuoid-core --test sources
    TZ={{ TEST_TZ }} cargo test -q -p pixtuoid-core --test sources
    cargo run --release -q -p pixtuoid-core --example bump -- patch "$1" "$3"

# Compile the workspace; extra args are forwarded:
#   just build                                # debug
#   just build --release                      # release
#   just build --release --bins --examples    # what ci-tests.yml's smoke job builds
[doc('Compile the workspace; forwards args (e.g. --release --bins --examples)')]
[group('rust')]
build *args:
    cargo build --workspace "$@"

# ── site ──────────────────────────────────────────────────────────
# The Astro landing page — a Node project under site/, checked by
# .github/workflows/site.yml. See site/README.md.

[doc('Install the site npm deps + the e2e browser (run once per clone)')]
[group('site')]
site-setup:
    npm --prefix site ci
    npx --prefix site playwright install chromium chromium-headless-shell

# The site's config asserts each demo the manifests name exists, and they are
# gitignored; a look change re-renders with `just gen-media --only site`.
[doc('Render site/public/demos when absent or its manifests changed')]
[group('site')]
site-demos:
    #!/usr/bin/env sh
    set -eu
    stamp=target/site-demos.inputs
    want=$(cat scripts/media.json scripts/gen-media.py site/src/themes.json site/src/weather.json | shasum | cut -d' ' -f1)
    # The rendered listing rides in the stamp, so a lost or half-written demo
    # re-renders instead of passing on the inputs alone.
    have() { find site/public/demos -type f -exec cksum {} + 2>/dev/null | sort | shasum | cut -d' ' -f1; }
    [ "$(cat "$stamp" 2>/dev/null)" = "$want $(have)" ] && exit 0
    test -x .venv/bin/python3 || { echo "needs the venv: python3 -m venv .venv && .venv/bin/pip install -r requirements-dev.txt"; exit 1; }
    .venv/bin/python3 scripts/gen-media.py --only site
    mkdir -p target && echo "$want $(have)" > "$stamp"

[doc('Site dev server with HMR → http://localhost:4321/ (foreground; agents: site-dev-bg)')]
[group('site')]
site-dev: site-demos
    npm --prefix site run dev

# Agent-facing dev-server lifecycle (Astro's `--background`): the daemon has no
# stdin/TTY tie, so it survives the launching shell — the foreground `astro dev`
# quits on stdin EOF, which killed agent-driven servers between commands.
# Readiness = the DEV-ONLY /_astro/status health endpoint (preview 404s it);
# the astro bin is called directly like playwright.config.ts does (same cwd, no
# npm wrapper layer). NOTE: dev and preview share port 4321 — stop the daemon
# (site-dev-stop) before `just site-e2e`, or its webServer spawn fails loud.
[doc('Dev server as a background daemon (survives stdin EOF) — waits on /_astro/status; stop: just site-dev-stop')]
[group('site')]
site-dev-bg: site-demos
    #!/usr/bin/env sh
    set -eu
    cd site
    node node_modules/astro/bin/astro.mjs dev --background
    tries=60 step=0.5
    for _ in $(seq 1 "$tries"); do
        if curl -fsS -m 2 http://localhost:4321/_astro/status >/dev/null 2>&1; then
            echo "ready → http://localhost:4321/  (logs: cd site && npx astro dev logs --follow)"
            exit 0
        fi
        sleep "$step"
    done
    echo "site-dev-bg: daemon started but /_astro/status not ready after $tries polls, ${step}s apart" >&2
    exit 1

[doc('Stop the background dev server (astro dev stop; no-op if none is running)')]
[group('site')]
site-dev-stop:
    cd site && node node_modules/astro/bin/astro.mjs dev stop

[doc('Site static tier: `npm run verify` (site/package.json owns the steps; site CI adds e2e + lighthouse)')]
[group('site')]
site-check: site-demos
    npm --prefix site run verify

[doc('Auto-format the site')]
[group('site')]
site-fmt:
    npm --prefix site run format

[doc('E2E smoke suite vs the PRODUCTION build (astro preview) — the runtime-contract gate')]
[group('site')]
site-e2e: gen-wasm site-demos
    #!/usr/bin/env sh
    set -eu
    cd site
    export GH_STARS_E2E=1
    npm run build
    npx playwright test

# ── gen ───────────────────────────────────────────────────────────
# Regenerate the committed artifacts that derive from a single source of truth,
# and check the committed copies (each `*-check` header says against what).

# No gen-media: media renders from main in media-regen.yml, never in a PR.
[doc('Regenerate what a look-changing PR commits (sprites + banner + icons + README sections + cutaway golden)')]
[group('gen')]
gen: gen-art gen-banner gen-icons gen-readme gen-cutaway-golden

[doc("Regenerate the bundled pack's generated sprites (every @Nx variant + the 1x pieces it owns) from scripts/gen-art.py")]
[group('gen')]
gen-art:
    python3 scripts/gen-art.py crates/pixtuoid-scene/sprites/default

# Stdlib-only, so `lint` runs it without the venv `gen-check` needs.
[doc('Fail if a committed generated sprite differs from what scripts/gen-art.py draws')]
[group('gen')]
gen-art-check:
    python3 scripts/gen-art.py --selftest
    python3 scripts/gen-art.py --check crates/pixtuoid-scene/sprites/default

# After gen-art: the golden is the pack's art as the office draws it.
[doc("Regenerate the README's banner: its art golden from the pack, then docs/images/banner/ from scripts/gen-banner.py")]
[group('gen')]
gen-banner:
    SNAPSHOTS=overwrite just test -p pixtuoid-scene --lib the_readme_banner_art_is_the_office_s_own
    python3 scripts/gen-banner.py docs/images/banner

# Stdlib-only, so `lint` runs it too; the golden's own drift is a test.
[doc('Fail if the committed README banner differs from what scripts/gen-banner.py draws')]
[group('gen')]
gen-banner-check:
    python3 scripts/gen-banner.py --selftest
    python3 scripts/gen-banner.py --check docs/images/banner

# Not in `gen`: it downloads its pinned fonts.
[doc("Regenerate the scene's Fusion Pixel faces + their licenses (crates/pixtuoid-scene/fonts/) from scripts/gen-fonts.py")]
[group('gen')]
gen-fonts:
    python3 scripts/gen-fonts.py

[doc('Sync README install/features/tools sections from site/src/*.json')]
[group('gen')]
gen-readme:
    node scripts/gen-readme.mjs

# Regenerate the --json contract chain after changing `SourceStatus` or
# `OutcomeRow`: re-emit their JSON Schemas from the Rust serde types, then
# regenerate the Raycast TS types from them. The freshness gates (the
# `*_schema_matches_the_committed_contract` golden tests in `just test`, and
# the raycast CI's `gen:contract` diff) FAIL until you run this — so the Rust
# producer and the TS consumer can't hand-drift. Needs raycast deps installed
# (`npm --prefix integrations/raycast ci`).
[doc('Regenerate the --json contract: the SourceStatus + OutcomeRow JSON Schemas (Rust) + the Raycast TS types')]
[group('gen')]
gen-contract:
    SNAPSHOTS=overwrite just test -p pixtuoid --lib schema_matches_the_committed_contract
    npm --prefix integrations/raycast run gen:contract

[doc("Regenerate the cutaway's pinned-frame digests (crates/pixtuoid-scene/src/cutaway/canvas.golden)")]
[group('gen')]
gen-cutaway-golden:
    SNAPSHOTS=overwrite just test -p pixtuoid-scene --lib the_canvas_paints_the_pinned_frames

# Pure node:builtins — no npm ci.
[doc('Fail if the committed README drifted from site data (features/sources/install.json)')]
[group('gen')]
gen-readme-check:
    node scripts/gen-readme.mjs --check

# Args are forwarded; scripts/gen-media.py's docstring owns the jobs, flags and
# toolchain.
[doc('Regenerate docs/images/ + site/public/demos/ from scripts/media.json')]
[group('gen')]
gen-media *args:
    .venv/bin/python3 scripts/gen-media.py "$@"

[doc("Regenerate the site's and the README's pix icons from the bundled sprite-pack palette")]
[group('gen')]
gen-icons:
    .venv/bin/python3 scripts/gen-pix-icons.py

# The wasm-bindgen CLI must match the crate's version exactly; Cargo.lock is that authority.
[private]
_wasm-bindgen-version:
    @awk '$0 == "name = \"wasm-bindgen\"" { getline; gsub(/"/, "", $3); print $3; found = 1 } END { exit !found }' Cargo.lock

# The output is gitignored; CI builds it through .github/actions/gen-wasm.
# Toolchain gotcha: the PATH cargo/rustc may be Homebrew's, which has NO wasm32
# std — and even `rustup run stable cargo` fails because cargo resolves `rustc`
# via PATH. So the recipe prepends the RUSTUP toolchain bin (via `rustup which`)
# and invokes that cargo explicitly.
[doc('Build pixtuoid-web (wasm) + JS glue into site/public/wasm/')]
[group('site')]
gen-wasm:
    #!/usr/bin/env sh
    set -eu
    want="$(just _wasm-bindgen-version)"
    have="$(wasm-bindgen --version 2>/dev/null || true)"
    [ "${have#wasm-bindgen }" = "$want" ] || { echo "needs wasm-bindgen-cli $want (Cargo.lock's), found ${have:-none}: cargo install wasm-bindgen-cli --locked --version $want"; exit 1; }
    command -v wasm-opt >/dev/null || { echo "needs wasm-opt: brew install binaryen"; exit 1; }
    command -v rustup >/dev/null || { echo "needs rustup (Homebrew rust has no wasm std)"; exit 1; }
    rustup target list --toolchain stable --installed | grep -q wasm32-unknown-unknown \
        || { echo "needs the wasm target: rustup target add wasm32-unknown-unknown"; exit 1; }
    TB="$(dirname "$(rustup which --toolchain stable rustc)")"
    PATH="$TB:$PATH" "$TB/cargo" build -p pixtuoid-web --target wasm32-unknown-unknown --profile wasm-release
    wasm-bindgen --target web --out-dir site/public/wasm \
        target/wasm32-unknown-unknown/wasm-release/pixtuoid_web.wasm
    wasm-opt -Oz -o site/public/wasm/pixtuoid_web_bg.wasm site/public/wasm/pixtuoid_web_bg.wasm

# Bloat gate for the wasm the site ships. Size: the hero must stay
# a lazy-load behind the poster, so a silent size regression (a dep pulling in
# formatting machinery, an accidental debug build) fails loudly. The cap is on
# the GZIPPED size, because the wire cost is what the poster is hiding.
# Raw is REPORTED, never gated as wire: the runner prices the wasm gzipped, as
# GitHub Pages ships it (`startPagesLikeProxy`), so site/lighthouserc.json sees raw
# growth only as parse/compile cost (its CPU-time assertions). WIRE cost is gated
# twice on purpose — here, naming the wasm, and there via its byte budgets under
# simulated throttling, sized to admit a wasm AT this cap. The cap is growth
# budget for the scene the hero runs, not a margin over today's payload, so a
# regression shows as the printed gap shrinking, not as a red.
[doc('Fail if the built wasm is missing or over its gzipped size cap')]
[group('site')]
gen-wasm-check:
    #!/usr/bin/env sh
    set -eu
    W=site/public/wasm/pixtuoid_web_bg.wasm
    # -s, not -f: an EMPTY wasm passes -f, and the ratio below divides
    # by its size. Failing here says what is wrong; failing there says "division
    # by 0".
    test -s "$W" || { echo "missing or empty $W — run 'just gen-wasm'"; exit 1; }
    # Not tuned to the last KB: this measures gzip locally while the CDN does its
    # own.
    CAP=524288
    # Compress to a FILE, not through a pipe: POSIX sh has no `pipefail`, so
    # `gzip … | wc -c` reports wc's status and a broken gzip would measure zero
    # bytes and pass the cap unconditionally — the gate would go green exactly
    # when it stopped working.
    GZ=$(mktemp)
    trap 'rm -f "$GZ"' EXIT
    gzip -9 -c "$W" > "$GZ"
    WIRE=$(wc -c < "$GZ" | tr -d ' ')
    RAW=$(wc -c < "$W" | tr -d ' ')
    test "$WIRE" -le "$CAP" || { echo "$W gzips to $WIRE bytes (> $CAP cap) — investigate the bloat"; exit 1; }
    # Report the headroom, don't just pass silently. A ratchet you can only read
    # at the moment it breaks gives no warning that it is about to — and a prose
    # estimate of the size drifts unnoticed precisely because every run is green.
    # Raw rides along with its RATIO, not bare: a bare byte count has nothing to
    # be read against. The ratio does — it is gzipped-over-raw, so RISING means
    # new poorly-compressible code and falling means new sprite text.
    echo "wasm $WIRE / $CAP bytes gzipped ($((WIRE * 100 / CAP))% of cap, $(((CAP - WIRE) / 1024)) KB headroom; $RAW raw, compressing to $((WIRE * 100 / RAW))%)"

[doc('Fail if anything `just gen` writes has drifted')]
[group('gen')]
gen-check: compare-selftest gen-readme-check gen-art-check gen-banner-check gen-icons-check

# The icons also land in the site's committed assets and change only with their
# source, so their drift stays a gate.
[doc('Fail if a committed pix icon differs from what gen-pix-icons draws')]
[group('gen')]
gen-icons-check:
    #!/usr/bin/env sh
    set -eu
    test -x .venv/bin/python3 || { echo "needs the venv: python3 -m venv .venv && .venv/bin/pip install -r requirements-dev.txt"; exit 1; }
    .venv/bin/python3 scripts/gen-pix-icons.py --selftest
    .venv/bin/python3 scripts/gen-pix-icons.py --check

# scripts/gen-media.py's docstring says what `--check` compares. The README's
# media only: the site's demos are rendered in CI and never committed. On a PR
# a drift here is evidence for REVIEW.md's Render and look, not a gate; main's
# land through media-regen.yml.
# Requires the .venv; it builds the examples it renders with.
[doc("Diff the README's committed media against what gen-media renders")]
[group('gen')]
gen-media-check:
    #!/usr/bin/env sh
    set -eu
    test -x .venv/bin/python3 || { echo "needs the venv: python3 -m venv .venv && .venv/bin/pip install -r requirements-dev.txt"; exit 1; }
    .venv/bin/python3 scripts/gen-media.py --check --only docs

# ── release ───────────────────────────────────────────────────────

# packaging-build/action.yml keeps its own just-free parse of the same line —
# that composite deliberately never installs just (see ci-builds.yml).
[doc("Print the workspace version — release.yml's tag check and release-plz.yml's tag assertion read it")]
[group('release')]
workspace-version:
    @grep -m1 '^version' Cargo.toml | cut -d'"' -f2

# Pass `true` for targets that need the Docker-backed `cross` toolchain
# (CI installs it via taiki-e/install-action with `tool: cross`); anything but true/false
# fails loudly (the case below).
[doc('Cross-compile a release for ONE target triple (release.yml build matrix)')]
[group('release')]
build-target target cross="false":
    #!/usr/bin/env bash
    set -euo pipefail
    target="$1"
    use_cross="$2"
    # Anything but the two legal words means the caller's positional args
    # shifted, so fail loudly rather than infer "not true, so cargo".
    case "$use_cross" in
    true | false) ;;
    *)
        echo "error: cross must be 'true' or 'false', got '$use_cross' (positional args shifted?)" >&2
        exit 1
        ;;
    esac
    # Every LINUX artifact drops `audio` (musl can't link ALSA statically; the
    # aarch64 cross image has no ALSA headers), so prebuilt Linux binaries ship
    # SILENT and Linux audio is a from-source feature (#633; see
    # docs/CONFIGURATION.md). Every other default feature rides `portable`
    # (pixtuoid's Cargo.toml). Derived here, not passed: the flags are a
    # property of the target. $flags stays UNQUOTED below — quoting the empty
    # non-Linux case would pass cargo an empty positional arg.
    flags=""
    case "$target" in
    *linux*) flags="--no-default-features --features portable" ;;
    esac
    if [ "$use_cross" = "true" ]; then
        cross build --release --target "$target" $flags
    else
        cargo build --release --target "$target" $flags
    fi

# Globbed, not listed, so a font embedded later ships its notice too: a font's
# notice is any .txt under the `fonts/` beside it. The tree mirrors the repo's
# (licenses/<crate>/fonts/…), as pixtuoid's .deb assets do with the same glob.
[doc("Copy LICENSE + every embedded font's notice (licenses/) into a release archive's DIR")]
[group('release')]
stage-notices dir:
    #!/usr/bin/env bash
    set -euo pipefail
    cp LICENSE "$1/"
    find crates/*/fonts -name '*.txt' | while IFS= read -r notice; do
        dest="$1/licenses/${notice#crates/}"
        mkdir -p "$(dirname "$dest")"
        cp "$notice" "$dest"
    done

# `--no-build`: the target is already built by `build-target`. Needs cargo-deb
# (CI installs it via taiki-e/install-action with `tool: cargo-deb`).
[doc('Package the .deb for ONE already-built target (release.yml deb job)')]
[group('release')]
deb target:
    cargo deb -p pixtuoid --no-build --no-strip --target "$1"
    cargo deb -p pixtuoid-hook --no-build --no-strip --target "$1"

# The repo's NODE-side gate (no cargo): the npm package generator AND the bundled
# OpenClaw plugin contract.
#   - npm/generate.test.mjs — the ONLY validation of npm/generate.mjs. release.yml
#     runs it as a hard gate right before `npm publish`, and ci-lint.yml's `npm-gen` job
#     so a generator regression is caught at review time, not at the tag-push.
#   - scripts/{openclaw,dsh}-plugin.test.mjs — drive each RENDERED plugin the way
#     its CLI's loader does. The Rust side can only grep a template as a string, so
#     this is the only place a plugin's runtime contract (never block its host /
#     never forward content / openclaw's gateway identity, dsh's emit order) is
#     actually EXECUTED.
# NOT in preflight: a Rust pre-push shouldn't require a Node toolchain. Needs Node ≥ 22.
[doc('Node gates: the npm package generator + the OpenClaw and dsh plugin contracts (CI + release; not in preflight)')]
[group('release')]
npm-check:
    node --test npm/generate.test.mjs scripts/openclaw-plugin.test.mjs scripts/dsh-plugin.test.mjs

# ── meta ──────────────────────────────────────────────────────────

# The local gate `.githooks/pre-push` runs: lint + clippy, fast over warm build
# caches. Tests are CI's: in the hook, every worktree's push re-ran the whole
# suite on one shared machine. `full` adds the feature powerset and the tests,
# built with linux-test's features — the Rust recipes CI's lint/clippy/hack/test
# jobs run; what it still can't see are ci.yml's other jobs.
[doc('Local gate: lint → clippy; `full` = lint → clippy → hack → test')]
[group('meta')]
preflight mode="":
    #!/usr/bin/env bash
    set -euo pipefail
    mode="$1"
    case "$mode" in
    "") just lint && just clippy ;;
    full) just lint && just clippy && just hack && just test --features pixtuoid/upstream ;;
    *) echo "usage: just preflight [full]" >&2; exit 2 ;;
    esac

# Install the dev tools every check + recipe relies on (idempotent). Prefers
# cargo-binstall (prebuilt) and falls back to cargo install (compiles).
[doc('Install the dev tools the checks + recipes need (idempotent)')]
[group('meta')]
setup-tools:
    #!/usr/bin/env bash
    set -euo pipefail
    # cargo-public-api rides API_PUBLIC_API — the tool-exact story lives there.
    # cargo-edit: `cargo set-version --workspace` raises a release PR's version
    # by hand for a break cargo-semver-checks cannot see
    # (docs/CONTRIBUTING.md#releasing).
    tools=(cargo-nextest {{ LINT_CARGO_TOOLS }} cargo-hack cargo-edit cargo-insta cargo-public-api@{{ API_PUBLIC_API }})
    if command -v cargo-binstall &>/dev/null; then
        cargo binstall -y "${tools[@]}"
    else
        echo "cargo-binstall not found — compiling from source (slow)." >&2
        echo "brew install cargo-binstall (or cargo install cargo-binstall) to grab prebuilt binaries instead." >&2
        cargo install "${tools[@]}"
    fi
    # The rust-analyzer component powers the editor / AI-agent LSP (go-to-def,
    # find-references — the tool the "change all N keying sites in lockstep"
    # invariants depend on). rust-toolchain.toml pins only rustfmt+clippy, so
    # without this the `~/.cargo/bin/rust-analyzer` rustup shim errors with
    # "Unknown binary" and the LSP silently degrades to grep. Idempotent; skipped
    # cleanly when rustup is absent (e.g. a distro-packaged toolchain).
    if command -v rustup &>/dev/null; then
        rustup component add rust-analyzer >/dev/null 2>&1 ||
            echo "could not add the rust-analyzer component — install it for LSP support" >&2
    fi
    # `LINT_BREW_TOOLS`, via brew. gitleaks is among them because `just
    # fixture-pii` is a REQUIRED gate that does not degrade to a weaker scan
    # without it (a weaker scan is what it replaced).
    for t in {{ LINT_BREW_TOOLS }}; do
        command -v "$t" &>/dev/null && continue
        if command -v brew &>/dev/null; then
            brew install "$t" || true
        fi
    done
    # Re-verify AFTER the install attempts: a `brew install` that exits 0 without
    # putting the binary on PATH (transient failure), or no brew at all, must be
    # caught here — not silently pass as a successful setup.
    missing=()
    for t in {{ LINT_BREW_TOOLS }}; do
        command -v "$t" &>/dev/null || missing+=("$t")
    done
    if (( ${#missing[@]} )); then
        echo "error: ${missing[*]} still missing after setup — install via your package manager (e.g. brew install ${missing[*]}); \`just lint\` needs it." >&2
        exit 1
    fi
    # Activate the local pre-push gate (dormant by default in a fresh clone, so CI
    # would otherwise be the only gate). Idempotent. CI runs every gate itself,
    # so a skipped local hook still meets them at merge.
    git config core.hooksPath .githooks

# The size gate's own negative control: a size cap that stops measuring reports
# success for any artifact at all, which no linter can see. This pins the
# FAIL-OPEN class specifically — the pipe hazard gen-wasm-check's gzip
# step is written around: driving the real recipe with a gzip that exits 1 must
# red it.
# Not covered, deliberately: the over-cap and empty-artifact arms, which would
# have to mutate the built wasm to exercise. Their failures are loud; the
# fail-open one is the silent class worth a test.
[doc('Self-test the wasm size gate: prove it still reds when its measurement breaks')]
[group('meta')]
[private]
wasm-check-selftest:
    #!/usr/bin/env sh
    set -eu
    stub=$(mktemp -d)
    trap 'rm -rf "$stub"' EXIT
    printf '#!/bin/sh\nexit 1\n' > "$stub/gzip"
    chmod +x "$stub/gzip"
    if PATH="$stub:$PATH" just gen-wasm-check >/dev/null 2>&1; then
        echo "wasm-check-selftest: FAIL — gen-wasm-check passed with a broken gzip;"
        echo "  the size measurement is fail-OPEN. Did the gzip call become a pipe?"
        exit 1
    fi
    just gen-wasm-check >/dev/null
    echo "wasm-check-selftest: OK (reds on a broken measurement, greens on a real one)"

# The pixel comparator is the primitive under `gen-check` and
# `gen-media-check`; an always-green comparator reports success for any render
# at all. Its own recipe because it needs only Pillow, while `gen-check` needs
# every generator's toolchain: a developer who cannot run that gate should still
# be able to run this.
[doc('Self-test the pixel comparator that gen-check and gen-media-check ride on')]
[group('meta')]
compare-selftest:
    #!/usr/bin/env sh
    set -eu
    # Prefer the venv (the same Pillow gen-media.py uses), but do not require it:
    # any python3 with Pillow answers the question this recipe asks.
    if [ -x .venv/bin/python3 ]; then py=.venv/bin/python3; else py=python3; fi
    "$py" scripts/compare-screenshots.py --selftest

# Default features off: the watch plays no audio, so the runner needs no ALSA.
[doc('Check every name the sources read and the installers register against upstream (network)')]
[group('meta')]
drift-watch:
    cargo run -q -p pixtuoid --no-default-features --features upstream --example drift_watch

# The pty driver's pure halves — the ANSI stripper, the composer comparison, the
# gate/menu wording. Each fails silently, at the price of a BILLED turn: a broken
# stripper just stops matching, and the capture comes back empty blaming the
# CLI.
[doc("Self-test the TUI capture driver's pure logic")]
[group('meta')]
tuidrive-selftest:
    python3 scripts/lib/tuidrive.py --selftest

# The git env scrub, both copies: `e2e_init_repo` and the pre-push hook. A git
# hook exports GIT_DIR/GIT_INDEX_FILE into every child and those OUTRANK
# `git -C <dir>`, so an unscrubbed helper COMMITS to the developer's real repo
# while printing nothing. The suite runs the unscrubbed form first and proves
# it leaks, so a scrub that stopped scrubbing cannot pass. Hermetic: one
# mktemp -d, no network, no real repo.
[doc("Self-test the git env scrub — the e2e helper and the pre-push hook")]
[group('meta')]
e2e-scrub-selftest:
    bash scripts/lib/e2e-common-selftest.sh

# The README star chart's renderer — bitmap font, axes, paging. (Its copied
# office colours are pinned from Rust: scene's `tests/readme_chart_palette.rs`.)
[doc("Self-test the README star-chart renderer")]
[group('meta')]
star-history-selftest:
    python3 scripts/star-history.py --selftest

# The README's Buy Me a Coffee button: count parsing, pack sprites, layout.
[doc("Self-test the README Buy Me a Coffee button renderer")]
[group('meta')]
bmc-button-selftest:
    python3 scripts/bmc-button.py --selftest

# The recorder refuses a capture carrying its own identity, but that check runs
# ONCE, on the capturer's terminal. This re-scans what is actually COMMITTED, so
# a fixture added by hand, edited later, or captured before the check existed is
# covered too. gitleaks, not a hand-rolled scanner: `.gitleaks.toml` says why.
[doc("Scan the committed fixture tree for secrets and the recorder's identity")]
[group('meta')]
fixture-pii:
    #!/usr/bin/env bash
    set -euo pipefail
    # TWO passes, and the split is load-bearing: the credential sweep wants
    # gitleaks' default allowlist (which waives filesystem-shaped strings), and
    # the identity rules are destroyed by it — a global allowlist outranks a
    # rule-scoped one. `.gitleaks-identity.toml` carries the proof.
    tree=crates/pixtuoid-core/tests/sources
    gitleaks dir "$tree" -c .gitleaks.toml          --no-banner --redact
    gitleaks dir "$tree" -c .gitleaks-identity.toml --no-banner --redact

# Prove both configs can FAIL, and that neither fires on the paths a fixture or a
# CI-path assertion legitimately carries. The direction that matters is "must
# fire": an identity rule moved into `.gitleaks.toml` is silently waived there by
# the default global allowlist, and `fixture-pii` would still exit 0 — the exact
# half-dead state this pair was split to prevent. Credential probes are assembled
# at RUNTIME so no token-shaped literal is ever committed.
[doc('Prove the fixture-pii configs red on a leak and green on a legitimate path')]
[group('meta')]
fixture-pii-selftest:
    #!/usr/bin/env bash
    set -euo pipefail
    d=$(mktemp -d); trap 'rm -rf "$d"' EXIT
    mkdir -p "$d/probe" "$d/quiet"
    # ONE probe per file, and the assertion is the exact SET of files each config
    # reports — not "did it find something". A config whose rules are half dead
    # still fires on its surviving rule, so an aggregate check greens on the very
    # state this exists to catch.
    printf '/home/alice\n'          > "$d/probe/identity-home.txt"
    printf '/Users/bob/notes.txt\n' > "$d/probe/identity-users.txt"
    # A username that STARTS with a placeholder token, and the Windows separator:
    # the two shapes a looser rule admits.
    printf '/Users/dev-ops\n'  > "$d/probe/identity-prefix.txt"
    printf 'C:\\Users\\bob\n'  > "$d/probe/identity-win.txt"
    printf -- '--Users-carol-Desktop-proj--\n' > "$d/probe/identity-dashed.txt"
    printf 'mcp__internal_tracker\n' > "$d/probe/identity-mcp.txt"
    printf '{"user_email":"a.person@gmail.com"}\n' > "$d/probe/identity-email.txt"
    # The one identity class with no shape of its own — reachable only under the
    # label a CLI renders it beneath.
    printf 'Git user: Ada Lovelace\n' > "$d/probe/identity-gituser.txt"
    printf '{"authorization":"Bearer %s"}\n' "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9x" \
        > "$d/probe/identity-bearer.txt"
    # Assembled, and deliberately NOT `AKIAIOSFODNN7EXAMPLE` — gitleaks' default
    # allowlist waives AWS's own documentation key, so that one proves nothing.
    printf 'aws_key = "AKIA%s"\n' "QYZ3K7RFVD2NMXWB" > "$d/probe/cred-aws.txt"
    # A real secret wearing a wire identifier's PREFIX: an unanchored wire-id
    # allowlist waives it by substring.
    printf '{"api_key":"msg_%s"}\n' "Xq7RvN2bK9wLpT4mZs8cHf1jY6dQ3aGe0uVi5nBr" > "$d/probe/cred-disguised.txt"
    # The RESIDUAL, pinned so it is visible rather than assumed closed: a secret
    # that lands INSIDE the wire-id allowlist's own length window (`.gitleaks.toml`) still rides the
    # allowlist. Any shape-based waiver admits a secret wearing that shape; what
    # stops this class is `no_identity_key_holds_a_value_outside_a_pinned_exemption`,
    # which reads the committed bytes — and an exempt capture, being outside it, is
    # the one place a human read is still the only check.
    printf '{"api_key":"msg_%s"}\n' "Kd8sQm2zXv6bTn4wRj9c" > "$d/quiet/cred-residual.txt"
    printf '/Users/dev/x\n/home/runner/work\n/home/ubuntu\n/home/linuxbrew\n/Users/Shared\nmcp__exampleThing\nC:\\Users\\Me\n/Users/dev.\n' \
        > "$d/quiet/identity.txt"
    printf -- '--private-tmp-pixtuoid-capture-proj--\n--Users-dev-proj--\n-home-runner-work\n' \
        > "$d/quiet/identity-dashed.txt"
    printf 'dev@example.com\nbot@users.noreply.github.com\nx@localhost\n' > "$d/quiet/email.txt"
    printf 'Git user: dev\nGit user: runner\nGit user: ubuntu\n' > "$d/quiet/identity-gituser.txt"
    printf 'Bearer short\n' > "$d/quiet/bearer.txt"
    printf 'msg_%s\n%s\n' "0123456789abcdefghij" "20260815_120000_a1b2c3" > "$d/quiet/cred.txt"
    fired() {
        gitleaks dir "$2" -c "$1" --no-banner --redact --report-format json \
            --report-path "$d/out.json" >/dev/null 2>&1 || true
        jq -r '[.[].File | split("/") | last] | unique | join(",")' "$d/out.json"
    }
    fail=0
    for spec in ".gitleaks.toml=cred-aws.txt,cred-disguised.txt" \
                ".gitleaks-identity.toml=identity-bearer.txt,identity-dashed.txt,identity-email.txt,identity-gituser.txt,identity-home.txt,identity-mcp.txt,identity-prefix.txt,identity-users.txt,identity-win.txt"; do
        cfg=${spec%%=*}; want=${spec#*=}
        got=$(fired "$cfg" "$d/probe")
        if [ "$got" != "$want" ]; then
            echo "fixture-pii-selftest: FAIL — $cfg reported [$got], want [$want]" >&2
            fail=1
        fi
        got=$(fired "$cfg" "$d/quiet")
        if [ -n "$got" ]; then
            echo "fixture-pii-selftest: FAIL — $cfg fired on legitimate paths [$got]" >&2
            fail=1
        fi
    done
    [ "$fail" -eq 0 ] || exit 1
    echo "fixture-pii-selftest: OK (each rule reds on its own probe, green on legitimate paths)"
