#!/usr/bin/env bash
# Proves the pre-push hook's git env scrub. It runs an unscrubbed hook first and
# shows it leaks — without that negative control this file would pass against a
# scrub that does nothing, which is the exact failure mode #893 shipped twice.
#
# Everything happens inside one `mktemp -d`; the real repo is never a target.
set -uo pipefail

# This file's fixture setup builds repos, so it must not inherit a relocating
# GIT_DIR — #893, inside the file that pins #893. `.githooks/pre-push` scrubs at
# its entry now, but this suite also runs directly, and `submodule foreach`
# exports GIT_DIR with no hook involved. It has to happen HERE, before anything
# builds a repo.
# shellcheck disable=SC2046  # the unset githooks(5) prescribes (https://git-scm.com/docs/githooks) — the list must word-split
unset $(git rev-parse --local-env-vars)

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

fails=0
# Takes the message, then the command to run — never `$?`, which after a `[ ]`
# reports the condition and is overwritten by the next one (SC2319).
check() {
    local msg="$1"
    shift
    if "$@"; then
        printf '  ok   %s\n' "$msg"
    else
        printf '  FAIL %s\n' "$msg" >&2
        fails=$((fails + 1))
    fi
}

root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT

# --- the pre-push hook's scrub -----------------------------------------------
# Enumerated rather than named, so a hook added later fails HERE instead of
# inheriting the gap silently. pre-commit is exempt while it only runs
# `cargo fmt`; drop it from this list the moment it fans out to anything else.
hooks_exempt_from_scrub="pre-commit"

# A PATH with no real just or mise, whatever the machine keeps in its
# system dirs: only what the hooks and stubs run.
sysbin="$root/sysbin"
mkdir -p "$sysbin"
for b in bash git dirname; do ln -s "$(command -v "$b")" "$sysbin/$b"; done

# What the hook hands `stub` (the command it runs for just) at its `exec`,
# given the GIT_DIR a LINKED worktree's push exports. Stubbing asserts the env
# contract alone — no toolchain, no network, no push, which is why CI can run
# this at all. `inherit` keeps the caller's PATH behind the stub; `bare` leaves
# only `$sysbin`. Leaves the run's dir in `hook_run`.
hook_hands() {
    local hook="$1" stub="$2" path_mode="$3" path
    hook_run=$(mktemp -d "$root/hookXXXX")
    mkdir -p "$hook_run/bin" "$hook_run/repo"
    git init -q "$hook_run/repo"
    # Reports EVERY local-env-var still set, not just GIT_DIR: a scrub narrowed
    # to GIT_DIR alone leaves GIT_INDEX_FILE relocating the index, which is half
    # of what outranks `git -C <dir>` in #893, and would otherwise pass here.
    cat >"$hook_run/bin/$stub" <<STUB
#!/usr/bin/env bash
printf '%s ' "\$@" >"$hook_run/argv"
for v in \$(git rev-parse --local-env-vars); do [ -n "\${!v:-}" ] && printf '%s ' "\$v"; done >"$hook_run/seen"
exit 0
STUB
    chmod +x "$hook_run/bin/$stub"
    path="$hook_run/bin:$sysbin"
    [ "$path_mode" = inherit ] && path="$hook_run/bin:$PATH"
    (cd "$hook_run/repo" && PATH="$path" GIT_DIR="$hook_run/repo/.git" \
        GIT_INDEX_FILE="$hook_run/repo/.git/index" GIT_WORK_TREE="$hook_run/repo" \
        bash "$hook" origin y </dev/null) >/dev/null 2>&1
}

hook_hands_just() {
    hook_hands "$1" just inherit
    # Two channels: stdout is the leak list; the exit status (cat's own) says
    # whether the hook reached `exec` at all. Folding "died before exec" into
    # stdout as a sentinel would satisfy the control's `-n` and flip its
    # diagnosis when the harness itself is broken.
    cat "$hook_run/seen" 2>/dev/null
}

# The hook reached its `exec` AND handed the child no repo-relocating env.
# Split from a bare `-z` because a `seen` never written reads identically to
# one written empty — the died-before-exec vacuous pass.
hook_scrubs_clean() {
    local out
    out=$(hook_hands_just "$1") || return 1
    [ -z "$out" ]
}

# Written out, never derived from a real hook: a control cut with `grep -v`
# orphans the guard's `if` body, so the mutant exits before `just` and the
# control silently reports "no leak" while testing nothing.
printf '#!/usr/bin/env bash\nexec just preflight\n' >"$root/unscrubbed-hook"
check "the harness actually poisons a hook's env (control fires)" \
    test -n "$(hook_hands_just "$root/unscrubbed-hook")"

# The died-before-exec detector needs its own control: without one, an edit
# that breaks the exit-status channel turns that class back into a silent pass.
printf '#!/usr/bin/env bash\nexit 7\n' >"$root/dies-before-exec-hook"
died_hook_reads_dirty() {
    ! hook_scrubs_clean "$root/dies-before-exec-hook"
}
check "a hook that dies before exec reads as dirty, never clean" \
    died_hook_reads_dirty

for hook in "$repo"/.githooks/*; do
    name=$(basename "$hook")
    case " $hooks_exempt_from_scrub " in *" $name "*) continue ;; esac
    check "$name reaches its exec with no repo-relocating git env" \
        hook_scrubs_clean "$hook"
done

# --- the just lookup: no just on PATH, so the hook asks mise for it ----------

# `want` is what the stub mise must be called with, so the hook neither skips
# the fallback nor runs another recipe; a hook that pushes also gets no
# repo-relocating env.
hook_falls_back_to_mise() {
    local hook="$1" want="$2" scrubbed="$3"
    hook_hands "$hook" mise bare
    [ "$(cat "$hook_run/argv" 2>/dev/null)" = "$want " ] || return 1
    [ "$scrubbed" = no ] || [ -z "$(cat "$hook_run/seen" 2>/dev/null)" ]
}
check "pre-push falls back to mise exec -- just preflight with a scrubbed env" \
    hook_falls_back_to_mise "$repo/.githooks/pre-push" "exec -- just preflight" yes
check "pre-commit falls back to mise exec -- just fmt-check" \
    hook_falls_back_to_mise "$repo/.githooks/pre-commit" "exec -- just fmt-check" no

if [ "$fails" -eq 0 ]; then
    echo "hook-scrub-selftest: OK"
else
    echo "hook-scrub-selftest: $fails FAILED" >&2
    exit 1
fi
