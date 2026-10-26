#!/usr/bin/env bash
set -euo pipefail

fail() {
    echo "ci-observability behavior test: $*" >&2
    exit 1
}

workflow_step_script() {
    local yaml_file="$1"
    local step_name="$2"
    STEP_NAME="$step_name" yq -e -r '
        [.jobs[].steps[] | select(.name == strenv(STEP_NAME)) | .run]
        | select(length == 1)
        | .[0]
    ' "$yaml_file" || fail "$yaml_file has no single step named \"$step_name\""
}

test_dir="$(mktemp -d)"
trap 'rm -rf "$test_dir"' EXIT

fake_bin="$test_dir/bin"
mkdir -p "$fake_bin"

# The queue's release-PR exclusion copies release-plz's branch prefix.
release_prefix="$(yq -p toml -oy -e -r '.workspace.pr_branch_prefix' release-plz.toml)" ||
    fail "release-plz.toml has no pr_branch_prefix"
yq -o=json '.' .mergify.yml | jq -e --arg c "-head ~= ^$release_prefix" \
    '(.queue_rules | length > 0) and all(.queue_rules[]; any(.queue_conditions[]; . == $c))' >/dev/null ||
    fail ".mergify.yml has a queue that admits release-plz's ${release_prefix}* PRs"

# A `tools:` word that is no mise.toml key is dropped by MISE_ENABLE_TOOLS and
# installs nothing, so the job runs whatever the runner has, or fails late. A computed value would escape that
# check, and setup-tools writes it to GITHUB_ENV, so only setup-cargo-just's
# own forwarding of its input may carry an expression.
mise_keys="$(yq -p toml -o=json '.tools | keys | .[]' mise.toml | jq -r '.')" || fail "mise.toml has no [tools]"
tools_words=""
for f in .github/workflows/*.yml .github/actions/*/action.y*ml; do
    values="$(yq -o=json '.' "$f" | jq -r '.. | objects | select((.uses // "") | test("/setup-(tools|cargo-just)$")) | (.with.tools // empty)')" ||
        fail "cannot read the tools: values of $f"
    while IFS= read -r value; do
        [[ -n "$value" ]] || continue
        # shellcheck disable=SC2016  # the literal GitHub expression syntax
        if [[ "$value" == *'${{'* ]]; then
            [[ "$f" == .github/actions/setup-cargo-just/action.yml && "$value" == '${{ inputs.tools }}' ]] ||
                fail "$f passes a computed tools: $value"
            continue
        fi
        tools_words+="${value// /$'\n'}"$'\n'
    done <<<"$values"
done
[[ -n "$tools_words" ]] || fail "no setup-tools/setup-cargo-just caller passes tools"
unknown_tools="$(comm -23 <(sort -u <<<"$tools_words") <(sort <<<"$mise_keys"))"
[[ -z "$unknown_tools" ]] || fail "tools: words that are no mise.toml [tools] key: $unknown_tools"
# The justfile reads `mise env` on every run, so a mise.toml that needs `mise
# trust` (a `[tool_alias]` table does) breaks just in each fresh checkout.
# A clean env: mise trusts every file under CI=true, and a caller's mise
# config or state could trust it too.
mise_untrusted() {
    mkdir -p "$test_dir/$1/home" && cp "$2" "$test_dir/$1/mise.toml"
    (cd "$test_dir/$1" && env -i PATH="$PATH" HOME="$test_dir/$1/home" MISE_OFFLINE=1 mise env --paths >/dev/null 2>&1)
}
mise_untrusted untrusted mise.toml ||
    fail "mise.toml needs \`mise trust\`, so a fresh checkout's just fails"
{ cat mise.toml && printf '\n[tool_alias]\nprobe = "aqua:jqlang/jq"\n'; } >"$test_dir/aliased.toml"
! mise_untrusted aliased "$test_dir/aliased.toml" ||
    fail "the untrusted-mise probe passes a [tool_alias] table, so it cannot fire"

# ── wait-ci-gate.yml: both release paths wait for their own commit's ci-gate ──
wait_workflow=.github/workflows/wait-ci-gate.yml
wait_step="Wait for ci-gate on this commit"
wait_script="$(workflow_step_script "$wait_workflow" "$wait_step")"
wait_env() { STEP_NAME="$wait_step" yq -e -r ".jobs[].steps[] | select(.name == strenv(STEP_NAME)) | .env.$1" "$wait_workflow"; }
wait_check="$(wait_env CHECK_NAME)" || fail "\"$wait_step\" has no CHECK_NAME"
[[ "$wait_check" == "$(yq -e -r '.jobs.gate.name' .github/workflows/ci.yml)" ]] ||
    fail "\"$wait_step\" waits for $wait_check, not ci.yml's gate"
[[ "$(wait_env SHA)" == "\${{ github.sha }}" ]] || fail "\"$wait_step\" waits on another commit than its caller's"

# Each call answers the next of FAKE_CHECK_PAGES, the last one repeating, and
# logs its endpoint.
cat >"$fake_bin/gh" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
[[ "$1" == api ]]
echo "$2" >>"$CHECK_CALLS.endpoints"
jq_expr="."
while (($#)); do
    [[ "$1" == --jq ]] && jq_expr="$2"
    shift
done
calls=$(($(cat "$CHECK_CALLS" 2>/dev/null || echo 0) + 1))
echo "$calls" >"$CHECK_CALLS"
page="$(sed -n "${calls}p" <<<"$FAKE_CHECK_PAGES")"
[[ -n "$page" ]] || page="$(tail -n 1 <<<"$FAKE_CHECK_PAGES")"
[[ "$page" != error ]] || exit 1
jq -r "$jq_expr" <<<"$page"
STUB
# A wait that never ends is cut off by its job timeout; here, by the fifth sleep.
cat >"$fake_bin/sleep" <<'STUB'
#!/usr/bin/env bash
n=$(($(cat "$CHECK_CALLS.sleeps" 2>/dev/null || echo 0) + 1))
echo "$n" >"$CHECK_CALLS.sleeps"
((n < 5))
STUB
chmod +x "$fake_bin/gh" "$fake_bin/sleep"

check_page() {
    jq -cn --arg app "$1" --arg status "$2" --arg conclusion "$3" \
        '{check_runs: [{app: {slug: $app}, status: $status, conclusion: (if $conclusion == "" then null else $conclusion end), started_at: "2026-10-01T00:00:00Z"}]}'
}
no_runs='{"check_runs":[]}'
assert_wait() {
    local expect="$1" label="$2" pages="$3"
    rm -f "$test_dir/check-calls" "$test_dir/check-calls.sleeps"
    if PATH="$fake_bin:$PATH" CHECK_CALLS="$test_dir/check-calls" CHECK_NAME="$wait_check" \
        FAKE_CHECK_PAGES="$pages" GH_TOKEN=test-token POLL_SECONDS=0 REPOSITORY=owner/repo SHA=abc1234 \
        bash -c "$wait_script" >/dev/null 2>&1; then
        [[ "$expect" == pass ]] || fail "the release wait passed $label"
    else
        [[ "$expect" == fail ]] || fail "the release wait failed $label"
    fi
}
assert_wait pass "a ci-gate that passes after running" \
    "$no_runs"$'\n'"$(check_page github-actions in_progress "")"$'\n'"$(check_page github-actions completed success)"
assert_wait fail "an API failure" error
assert_wait fail "a failed ci-gate" "$(check_page github-actions completed failure)"
assert_wait fail "a cancelled ci-gate" "$(check_page github-actions completed cancelled)"
assert_wait fail "a skipped ci-gate" "$(check_page github-actions completed skipped)"
assert_wait fail "another app's passing check of that name" "$(check_page impostor completed success)"

# ── release-plz.yml: only a merged release PR waits for CI and publishes ──
release_workflow=.github/workflows/release-plz.yml
detect_step="Detect a merged release PR; refuse a stale one"
detect_script="$(workflow_step_script "$release_workflow" "$detect_step")"
detect_prefix="$(STEP_NAME="$detect_step" yq -e -r '.jobs[].steps[] | select(.name == strenv(STEP_NAME)) | .env.PR_BRANCH_PREFIX' "$release_workflow")" ||
    fail "\"$detect_step\" has no PR_BRANCH_PREFIX"
[[ "$detect_prefix" == "$release_prefix" ]] ||
    fail "\"$detect_step\" looks for $detect_prefix*, not release-plz.toml's $release_prefix*"
assert_detect() {
    local expect="$1" label="$2" pages="$3" output_file="$test_dir/detect-output"
    : >"$output_file"
    rm -f "$test_dir/check-calls" "$test_dir/check-calls.endpoints"
    if ! PATH="$fake_bin:$PATH" CHECK_CALLS="$test_dir/check-calls" FAKE_CHECK_PAGES="$pages" GH_TOKEN=test-token \
        GITHUB_OUTPUT="$output_file" PR_BRANCH_PREFIX="$detect_prefix" REPOSITORY=owner/repo SHA=abc1234 \
        bash -c "$detect_script" >/dev/null 2>&1; then
        [[ "$expect" == error ]] || fail "the release detection exited non-zero on $label"
        grep -q '^release=true$' "$output_file" && fail "the release detection failed open on $label"
        return 0
    fi
    [[ "$expect" != error ]] || fail "the release detection exited zero on $label"
    grep -qx "release=$expect" "$output_file" ||
        fail "the release detection did not answer release=$expect for $label: $(<"$output_file")"
}
pr_heads() { jq -cn '[$ARGS.positional[] | {head: {ref: ., sha: "\(.)-head"}}]' --args "$@"; }
# After a release PR is found: the merge's parent, then the release commit as
# its comparison with the PR's head lists it first.
release_pages() {
    printf '%s\n' '{"parents":[{"sha":"main123"},{"sha":"side123"}]}' \
        "$(jq -cn --arg parent "$1" --arg committed "$2" '{commits: [
            {parents: [{sha: $parent}], commit: {author: {date: "2026-10-01T00:00:00Z"}, committer: {date: $committed}}},
            {parents: [{sha: "x"}, {sha: "y"}], commit: {author: {date: "z"}, committer: {date: "z"}}}]}')"
}
release_head="${detect_prefix}v1.2.3"
assert_endpoints() {
    local want
    want="$(printf 'repos/owner/repo/%s\n' commits/abc1234/pulls commits/abc1234 "compare/main123...$release_head-head")"
    [[ "$(<"$test_dir/check-calls.endpoints")" == "$want" ]] ||
        fail "the release detection on $1 asked $(<"$test_dir/check-calls.endpoints"), not $want"
}
written="2026-10-01T00:00:00Z"
assert_detect true "a merged release PR" "$(pr_heads "$release_head")"$'\n'"$(release_pages main123 "$written")"
assert_endpoints "a merged release PR"
assert_detect true "a release PR among others" "$(pr_heads feat/x "$release_head")"$'\n'"$(release_pages main123 "$written")"
assert_endpoints "a release PR among others"
assert_detect error "a release PR cut from an older main" "$(pr_heads "$release_head")"$'\n'"$(release_pages old123 "$written")"
assert_detect error "a release PR rebased onto main" \
    "$(pr_heads "$release_head")"$'\n'"$(release_pages main123 2026-10-01T00:05:00Z)"
assert_detect error "an API failure on the comparison" \
    "$(pr_heads "$release_head")"$'\n'"$(release_pages main123 "$written" | head -n 1)"$'\n'error
assert_detect false "an ordinary PR" "$(pr_heads feat/x)"
assert_detect false "a branch merely naming the prefix" "$(pr_heads "feat/${detect_prefix}x")"
assert_detect false "a direct push with no PR" "$(pr_heads)"
assert_detect error "an API failure" error

# ── release-plz.yml: a homebrew-core test miss warns, once per miss, and never
# holds the release PR ──
brew_step="homebrew-core's test calls only what this tree can run"
brew_script="$(workflow_step_script "$release_workflow" "$brew_step")"
brew_dir="$test_dir/brew"
brew_bin="$brew_dir/bin"
mkdir -p "$brew_bin" "$brew_dir/target/debug"
cat >"$brew_bin/curl" <<'STUB'
#!/usr/bin/env bash
[[ -n "${FAKE_FORMULA:-}" ]] || exit 22
cat "$FAKE_FORMULA"
STUB
printf '#!/usr/bin/env bash\n' >"$brew_bin/cargo"
# This tree dropped `gone-cmd` and the source `renamed`, and `connect needs-cli`
# fails with its `failed` row and two lines of cause; `wrong-id` and
# `wrong-outcome` fail with a row the formula's assertions refuse.
cat >"$brew_dir/target/debug/pixtuoid" <<'STUB'
#!/usr/bin/env bash
case "$1 ${2:-}" in
"gone-cmd --help") exit 2 ;;
"connect needs-cli") printf '[{"id":"needs-cli","outcome":"failed"}]\n' && printf 'line one\nline two\n' >&2 && exit 1 ;;
"connect renamed") printf 'unknown source\n' >&2 && exit 1 ;;
"connect wrong-id") printf '[{"id":"other","outcome":"failed"}]\n' && exit 1 ;;
"connect wrong-outcome") printf '[{"id":"wrong-outcome","outcome":"connected"}]\n' && exit 1 ;;
esac
STUB
chmod +x "$brew_bin/curl" "$brew_bin/cargo" "$brew_dir/target/debug/pixtuoid"
assert_brew() {
    local label="$1" formula="$2" want="$3" out lines=0 warnings=0 summary="$brew_dir/summary"
    : >"$summary"
    printf '%s\n' "$formula" >"$brew_dir/formula.rb"
    out="$(cd "$brew_dir" && PATH="$brew_bin:$PATH" GITHUB_STEP_SUMMARY="$summary" \
        FAKE_FORMULA="${formula:+$brew_dir/formula.rb}" bash -c "$brew_script" 2>&1)" ||
        fail "the homebrew step held the release on $label: $out"
    if [[ -n "$out" ]]; then
        lines="$(wc -l <<<"$out" | tr -d ' ')"
        warnings="$(grep -c '^::warning::' <<<"$out" || true)"
    fi
    [[ "$lines" == "$want" && "$warnings" == "$want" ]] ||
        fail "the homebrew step on $label wanted $want one-line warnings, printed: $out"
    if ((want == 0)); then
        [[ ! -s "$summary" ]] || fail "the homebrew step on $label wrote a summary: $(<"$summary")"
    else
        [[ "$(grep -c '^### ' "$summary")" == 1 ]] ||
            fail "the homebrew step on $label wanted one heading: $(<"$summary")"
    fi
}
assert_brew "a failed fetch" "" 1
assert_brew "a test block with no subcommand" $'  test do\n    assert_match version.to_s, shell_output("#{bin}/pixtuoid --version")\n  end' 1
grep -q 'shape changed' "$brew_dir/summary" || fail "the homebrew step didn't name an unrecognized test block"
assert_brew "a dropped subcommand and a failing connect" \
    $'  test do\n    system bin/"pixtuoid", "gone-cmd", testpath/"x"\n    shell_output("#{bin}/pixtuoid connect needs-cli --json")\n    shell_output("#{bin}/pixtuoid connect works --json")\n  end' 2
{ grep -q 'gone-cmd' "$brew_dir/summary" && grep -q 'needs-cli.*line one line two' "$brew_dir/summary"; } ||
    fail "the homebrew step didn't name each miss with its cause: $(<"$brew_dir/summary")"
assert_brew "a test block this tree runs" $'  test do\n    shell_output("#{bin}/pixtuoid connect works --json")\n  end' 0
assert_brew "an expected failure this tree fails" $'  test do\n    shell_output("#{bin}/pixtuoid connect needs-cli --json", 1)\n  end' 0
assert_brew "an expected failure this tree passes" $'  test do\n    shell_output("#{bin}/pixtuoid connect works --json", 1)\n  end' 1
grep -q 'exit 1, and this tree exits 0' "$brew_dir/summary" ||
    fail "the homebrew step didn't name the exit it expected: $(<"$brew_dir/summary")"
assert_brew "an expected failure of a source this tree renamed" $'  test do\n    shell_output("#{bin}/pixtuoid connect renamed --json", 1)\n  end' 1
grep -q 'renamed.*print its .failed. row.*unknown source' "$brew_dir/summary" ||
    fail "the homebrew step didn't name the missing row: $(<"$brew_dir/summary")"
for refused in wrong-id wrong-outcome; do
    assert_brew "an expected failure printing $refused" "  test do
    shell_output(\"#{bin}/pixtuoid connect $refused --json\", 1)
  end" 1
    grep -q "$refused.*print its .failed. row" "$brew_dir/summary" ||
        fail "the homebrew step didn't refuse $refused's row: $(<"$brew_dir/summary")"
done

# ── apt-install: a failed or stalled update or download retries under the
# action's own bound, and the install never runs under it or after a failed
# download.
apt_action=.github/actions/apt-install/action.yml
apt_script="$(yq -e -r '.runs.steps[0].run' "$apt_action")" || fail "apt-install has no run step"
apt_attempts="$(yq -e -r '.runs.steps[0].env.ATTEMPTS' "$apt_action")" || fail "apt-install sets no ATTEMPTS"
apt_bound="$(yq -e -r '.runs.steps[0].env.ATTEMPT_TIMEOUT' "$apt_action")" || fail "apt-install sets no ATTEMPT_TIMEOUT"
((apt_attempts > 1)) || fail "apt-install allows $apt_attempts attempt(s), so nothing retries"
apt_dir="$test_dir/apt"
mkdir -p "$apt_dir/bin"
# shellcheck disable=SC2016 # The stubs read their fixtures when they run.
printf '%s\n' '#!/usr/bin/env bash' 'printf "timeout %s\n" "$*" >>"$APT_LOG"; while [[ "$1" == -* ]]; do shift 2; done; shift; "$@"' >"$apt_dir/bin/timeout"
# shellcheck disable=SC2016
printf '%s\n' '#!/usr/bin/env bash' '"$@"' >"$apt_dir/bin/sudo"
# Fails the first APT_FAILS calls of the kind APT_FAIL_ON names, and logs the
# mirror each update leads with.
cat >"$apt_dir/bin/apt-get" <<'STUB'
#!/usr/bin/env bash
printf 'apt-get %s\n' "$*" >>"$APT_LOG"
case " $* " in *" update "*) kind=update ;; *" --download-only "*) kind=download ;; *) exit 0 ;; esac
[[ "$kind" == update ]] && awk -F '\t' '$2 == "priority:1" { print "lead " $1 }' "$APT_MIRRORS" >>"$APT_LOG"
[[ "$kind" == "$APT_FAIL_ON" ]] || exit 0
n=$(($(cat "$APT_LOG.n" 2>/dev/null || echo 0) + 1))
echo $n >"$APT_LOG.n"
((n > APT_FAILS))
STUB
chmod +x "$apt_dir/bin/"*
# The runner image's list (actions/runner-images configure-apt-sources.sh).
printf 'http://azure.archive.ubuntu.com/ubuntu/\tpriority:1\nhttps://archive.ubuntu.com/ubuntu/\tpriority:2\nhttps://security.ubuntu.com/ubuntu/\tpriority:3\n' >"$apt_dir/mirrors.image"
run_apt() {
    rm -f "$apt_dir/log" "$apt_dir/log.n"
    cp "$apt_dir/mirrors.image" "$apt_dir/mirrors"
    PATH="$apt_dir/bin:$PATH" APT_LOG="$apt_dir/log" APT_FAIL_ON="$1" APT_FAILS="$2" PACKAGES="pkg-a pkg-b" \
        RECOMMENDS="${3:-true}" ATTEMPTS="$apt_attempts" ATTEMPT_TIMEOUT="$apt_bound" APT_MIRRORS="$apt_dir/mirrors" \
        bash -c "$apt_script" >/dev/null 2>&1
}
apt_count() { grep -c -e "$1" "$apt_dir/log" || true; }
run_apt none 0 || fail "apt-install failed with a healthy mirror"
[[ "$(apt_count "^timeout -k 10 $apt_bound sudo apt-get .*install -y --download-only pkg-a pkg-b$")" == 1 ]] ||
    fail "apt-install did not bound the package download by its ATTEMPT_TIMEOUT: $(<"$apt_dir/log")"
[[ "$(apt_count '^apt-get .*install -y --no-download pkg-a pkg-b$')" == 1 ]] ||
    fail "apt-install did not install the downloaded packages: $(<"$apt_dir/log")"
if grep -q '^timeout .*--no-download' "$apt_dir/log"; then fail "apt-install bounded the install itself"; fi
for kind in update download; do
    run_apt "$kind" $((apt_attempts - 1)) || fail "apt-install did not recover from a failed $kind on its last attempt"
    pattern=" update\$"
    [[ "$kind" == download ]] && pattern=" --download-only pkg-a pkg-b\$"
    [[ "$(apt_count "^timeout -k 10 $apt_bound sudo apt-get .*$pattern")" == "$apt_attempts" ]] ||
        fail "apt-install did not retry each failed $kind: $(<"$apt_dir/log")"
    run_apt "$kind" "$apt_attempts" && fail "apt-install passed after every $kind failed"
    [[ "$(apt_count '--no-download')" == 0 ]] || fail "apt-install installed after every $kind failed"
done
# Each attempt leads with the next mirror, and the rest of the list keeps its
# place.
run_apt update $((apt_attempts - 1)) || fail "apt-install did not recover from failed updates"
want=""
for ((i = 0; i < apt_attempts; i++)); do
    if ((i % 2 == 0)); then want+="https://archive.ubuntu.com/ubuntu/ "; else want+="http://azure.archive.ubuntu.com/ubuntu/ "; fi
done
leads="$(grep '^lead ' "$apt_dir/log" | cut -d' ' -f2 | tr '\n' ' ')"
[[ "$leads" == "$want" ]] || fail "apt-install's attempts led with \"$leads\", not \"$want\""
if [[ "$(grep -c . "$apt_dir/mirrors")" != 3 ]] ||
    ! grep -q $'^https://security\.ubuntu\.com/ubuntu/\tpriority:3$' "$apt_dir/mirrors"; then
    fail "apt-install's rotation touched lines outside it: $(<"$apt_dir/mirrors")"
fi
run_apt none 0 false || fail "apt-install failed without recommends"
[[ "$(apt_count '^apt-get .*install -y --no-install-recommends')" == 2 ]] ||
    fail "apt-install did not drop recommends from both the download and the install: $(<"$apt_dir/log")"

# ── path-changed: a PR's merge commit against its base parent; anything it
# cannot diff counts as changed.
changed_script="$(yq -e -r '.runs.steps[0].run' .github/actions/path-changed/action.yml)" ||
    fail "path-changed has no run step"
repo="$test_dir/changed-repo"
git init -q "$repo" && git -C "$repo" config user.email t@t && git -C "$repo" config user.name t
echo a >"$repo/lock" && echo a >"$repo/other" && echo a >"$repo/gone" && git -C "$repo" add . && git -C "$repo" commit -qm base
git -C "$repo" checkout -qb pr && echo b >"$repo/other" && git -C "$repo" rm -q gone && git -C "$repo" commit -qam pr
git -C "$repo" checkout -q - && git -C "$repo" merge -q --no-ff --no-edit pr
changed() {
    : >"$test_dir/changed-output"
    (cd "$1" && GITHUB_EVENT_NAME="$2" GITHUB_OUTPUT="$test_dir/changed-output" CHANGED_PATHS="$3" bash -c "$changed_script") ||
        fail "path-changed exited non-zero"
    sed -n 's/^changed=//p' "$test_dir/changed-output"
}
[[ "$(changed "$repo" pull_request other)" == true ]] || fail "path-changed missed a path the PR changes"
[[ "$(changed "$repo" pull_request lock)" == false ]] || fail "path-changed reported a path the PR leaves alone"
[[ "$(changed "$repo" pull_request "lock other")" == true ]] || fail "path-changed missed one changed path among several"
[[ "$(changed "$repo" pull_request gone)" == true ]] || fail "path-changed missed a path the PR deletes"
if (cd "$repo" && GITHUB_EVENT_NAME=pull_request GITHUB_OUTPUT=/dev/null CHANGED_PATHS="lock no-such-file" bash -c "$changed_script") >/dev/null 2>&1; then
    fail "path-changed judged a path that exists on neither side"
fi
[[ "$(changed "$repo" push other)" == false ]] || fail "path-changed judged a push as a PR"
git init -q "$test_dir/no-parent" && echo a >"$test_dir/no-parent/lock" && git -C "$test_dir/no-parent" add lock &&
    git -C "$test_dir/no-parent" -c user.email=t@t -c user.name=t commit -q -m only
[[ "$(changed "$test_dir/no-parent" pull_request lock 2>"$test_dir/no-parent.err")" == true ]] ||
    fail "path-changed passed a diff it could not run: $(<"$test_dir/no-parent.err")"
grep -q 'bad revision' "$test_dir/no-parent.err" ||
    fail "the parentless repo's diff did not fail on its missing parent: $(<"$test_dir/no-parent.err")"

# ── hermetic-nightly.yml: the file job carries out the plan `hermetic::issues` made ──
file_script="$(workflow_step_script .github/workflows/hermetic-nightly.yml "File the planned issues")"
file_bin="$test_dir/file-bin"
mkdir -p "$file_bin"
cat >"$file_bin/gh" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$GH_LOG"
[[ "$*" != "${FAKE_FAIL:-}" ]] || exit 1
if [ "$1 $2" = "issue list" ]; then
    printf '%s' '[{"number":5,"body":"<!-- hermetic-source: hermes -->\nx"},{"number":7,"body":"y\n  <!-- hermetic-leg: macos-1 -->  "},{"number":8,"body":"<!-- hermetic-source: codex -->"},{"number":1658,"body":"hermetic: a pin bump failed"}]'
fi
while [ $# -gt 1 ]; do
    if [ "$1" = --body-file ]; then cat "$2" >>"$GH_BODIES"; fi
    shift
done
STUB
chmod +x "$file_bin/gh"
plans="$test_dir/file-plans"
mkdir -p "$plans"
# The step in a dir holding $1 as its plan.json, $2 the one gh call that fails: its status.
file_plan() {
    : >"$test_dir/gh.log"
    : >"$test_dir/gh.bodies"
    printf '%s' "$1" >"$plans/plan.json"
    (cd "$plans" && PATH="$file_bin:$PATH" GH_LOG="$test_dir/gh.log" GH_BODIES="$test_dir/gh.bodies" FAKE_FAIL="$2" \
        HERMETIC_LABEL=hermetic HERMETIC_LIMIT=1000 GH_TOKEN=t GH_REPO=o/r RUNNER_TEMP="$test_dir" \
        bash -c "$file_script") >"$test_dir/file.out" 2>&1
}
opened='<!-- hermetic-source: hermes -->\nopened'
plan='{"actions":[{"do":"open","title":"hermetic: hermes","body":"'"$opened"'"},{"do":"comment","number":7,"body":"commented"},{"do":"reopen","number":5,"body":"reopened"},{"do":"close","number":8,"body":"closed"}],"lost":[]}'
file_plan "$plan" '' || fail "the file step failed a plan it could carry out: $(<"$test_dir/file.out")"
listed="issue list --label hermetic --state all --limit 1000 --json number,body"
created="issue create --title hermetic: hermes --label hermetic --body-file $test_dir/body.md"
expected="$listed
$created
issue comment 7 --body-file $test_dir/body.md
issue reopen 5
issue comment 5 --body-file $test_dir/body.md
issue comment 8 --body-file $test_dir/body.md
issue close 8"
[[ "$(<"$test_dir/gh.log")" == "$expected" ]] || fail "the plan's actions became other gh calls: $(<"$test_dir/gh.log")"
[[ "$(<"$test_dir/gh.bodies")" == $'<!-- hermetic-source: hermes -->\nopened\ncommented\nreopened\nclosed' ]] ||
    fail "an action's body did not reach its call: $(<"$test_dir/gh.bodies")"
# One gh call that fails leaves the rest carried out, and the step red naming it.
if file_plan "$plan" 'issue comment 7 --body-file '"$test_dir/body.md"; then
    fail "the file step passed with an action it could not carry out"
fi
grep -qxF 'issue close 8' "$test_dir/gh.log" || fail "a failed call stopped the actions after it: $(<"$test_dir/gh.log")"
grep -qF '::error::could not comment issue 7' "$test_dir/file.out" || fail "the failed action is not named: $(<"$test_dir/file.out")"
# An issue the planner never marks, and an open led by no marker, are refused, red and named.
if file_plan '{"actions":[{"do":"close","number":1658,"body":"x"},{"do":"reopen","number":99,"body":"x"},{"do":"open","title":"t","body":"x\n<!-- hermetic-source: forged -->"},{"do":"comment","number":7,"body":"y"}],"lost":[]}' ''; then
    fail "the file step passed acting on an issue the planner would never mark"
fi
for refused in 'close issue 1658' 'reopen issue 99' 'open issue t'; do
    grep -qF "::error::refused to $refused" "$test_dir/file.out" || fail "the refused $refused is not named: $(<"$test_dir/file.out")"
done
[[ "$(<"$test_dir/gh.log")" == "$listed"$'\n'"issue comment 7 --body-file $test_dir/body.md" ]] ||
    fail "a refused action reached gh, or stopped the rest: $(<"$test_dir/gh.log")"
# With no list of the hermetic issues, every number is refused, and an open still files.
if file_plan "$plan" "$listed"; then
    fail "the file step passed without the hermetic issues listed"
fi
[[ "$(grep -c '^issue \(comment\|reopen\|close\)' "$test_dir/gh.log")" == 0 ]] ||
    fail "an unlisted number reached gh: $(<"$test_dir/gh.log")"
grep -qxF "$created" "$test_dir/gh.log" || fail "a failed list stopped the open: $(<"$test_dir/gh.log")"
# A leg that reported no verdicts turns the step red once its actions are done.
if file_plan '{"actions":[{"do":"comment","number":7,"body":"x"}],"lost":["macos, shard 1 reported no verdicts"]}' ''; then
    fail "the file step passed with a lost leg"
fi
grep -qF '::error::macos, shard 1 reported no verdicts' "$test_dir/file.out" || fail "the lost leg is not named: $(<"$test_dir/file.out")"
grep -qxF "issue comment 7 --body-file $test_dir/body.md" "$test_dir/gh.log" || fail "a lost leg stopped the actions: $(<"$test_dir/gh.log")"
# Nothing to do: no call but the list, and green.
file_plan '{"actions":[],"lost":[]}' '' || fail "the file step failed an empty plan: $(<"$test_dir/file.out")"
[[ "$(<"$test_dir/gh.log")" == "$listed" ]] || fail "an empty plan made a gh call: $(<"$test_dir/gh.log")"

# ── site.yml: Playwright's dry run hands apt-install exactly its missing
# packages.
pw_script="$(workflow_step_script .github/workflows/site.yml "Find Chromium's missing system packages")"
pw_bin="$test_dir/pw-bin"
mkdir -p "$pw_bin"
cat >"$pw_bin/npx" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$FAKE_REPORT"
exit "${FAKE_STATUS:-0}"
STUB
chmod +x "$pw_bin/npx"
pw_run() {
    : >"$test_dir/pw.out"
    FAKE_REPORT="$1" FAKE_STATUS="$2" GITHUB_OUTPUT="$test_dir/pw.out" PATH="$pw_bin:$PATH" \
        bash -c "$pw_script" >/dev/null 2>&1
}
pw_run 'All system dependencies are installed.' 0 || fail "the Playwright step failed with nothing missing"
[[ "$(<"$test_dir/pw.out")" == "packages=" ]] || fail "nothing missing still asked apt-install for: $(<"$test_dir/pw.out")"
pw_run $'Missing system dependencies (2):\n  libasound2t64\n  libnss3' 1 ||
    fail "the Playwright step failed on a report naming missing packages"
[[ "$(<"$test_dir/pw.out")" == "packages=libasound2t64 libnss3 " ]] ||
    fail "the Playwright step handed apt-install: $(<"$test_dir/pw.out")"
