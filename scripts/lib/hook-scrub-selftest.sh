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

# git's own list must relocate the repo and nothing else: stripping a transport
# or identity var would break a caller that set it deliberately.
scrub_list_is_narrow() {
    local list
    list=$(git rev-parse --local-env-vars)
    ! grep -qx 'GIT_SSH_COMMAND' <<<"$list" &&
        ! grep -qx 'GIT_COMMITTER_NAME' <<<"$list"
}

check "git's own list carries no transport/identity vars to over-strip" \
    scrub_list_is_narrow

# --- the pre-push hook's scrub -----------------------------------------------
# Enumerated rather than named, so a hook added later fails HERE instead of
# inheriting the gap silently. pre-commit is exempt while it only runs
# `cargo fmt`; drop it from this list the moment it fans out to anything else.
hooks_exempt_from_scrub="pre-commit"

# What the hook hands `just` at its `exec`, given the GIT_DIR a LINKED
# worktree's push exports. Stubbing `just` asserts the env contract alone — no
# toolchain, no network, no push, which is why CI can run this at all.
hook_hands_just() {
    local hook="$1" s
    s=$(mktemp -d "$root/hookXXXX")
    mkdir -p "$s/bin" "$s/repo"
    git init -q "$s/repo"
    # Reports EVERY local-env-var still set, not just GIT_DIR: a scrub narrowed
    # to GIT_DIR alone leaves GIT_INDEX_FILE relocating the index, which is half
    # of what outranks `git -C <dir>` in #893, and would otherwise pass here.
    # shellcheck disable=SC2016  # the stub's own script text, not expansion
    printf '#!/usr/bin/env bash\nfor v in $(git rev-parse --local-env-vars); do [ -n "${!v:-}" ] && printf "%%s " "$v"; done >"%s/seen"\nexit 0\n' "$s" >"$s/bin/just"
    chmod +x "$s/bin/just"
    (cd "$s/repo" && PATH="$s/bin:$PATH" GIT_DIR="$s/repo/.git" \
        GIT_INDEX_FILE="$s/repo/.git/index" GIT_WORK_TREE="$s/repo" \
        bash "$hook" origin y </dev/null) >/dev/null 2>&1
    # Two channels: stdout is the leak list; the exit status (cat's own) says
    # whether the hook reached `exec` at all. Folding "died before exec" into
    # stdout as a sentinel would satisfy the control's `-n` and flip its
    # diagnosis when the harness itself is broken.
    cat "$s/seen" 2>/dev/null
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

if [ "$fails" -eq 0 ]; then
    echo "hook-scrub-selftest: OK"
else
    echo "hook-scrub-selftest: $fails FAILED" >&2
    exit 1
fi
