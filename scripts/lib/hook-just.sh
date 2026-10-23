#!/usr/bin/env bash
# Sourced by the git hooks. A hook runs before any justfile, so the justfile's
# mise PATH can't supply just: `resolve_just <hook>` sets `just` to the command
# that runs it, or exits 1 with an install hint.

resolve_just() {
    # shellcheck disable=SC2034  # read by the sourcing hook
    just=(just)
    command -v just &>/dev/null && return 0
    if command -v mise &>/dev/null; then
        # shellcheck disable=SC2034  # read by the sourcing hook
        just=(mise exec -- just)
        return 0
    fi
    # shellcheck disable=SC2016  # backticks are literal text, not expansion
    printf '\033[31m[%s] `just` not found — brew install mise, then `mise install`\033[0m\n' "$1" >&2
    exit 1
}
