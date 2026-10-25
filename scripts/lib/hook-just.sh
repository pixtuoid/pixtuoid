#!/usr/bin/env bash
# Sourced by the git hooks. A hook runs before any justfile, so the justfile's
# mise PATH can't supply just: `resolve_just` sets `just` to the command that
# runs it.

resolve_just() {
    # shellcheck disable=SC2034  # read by the sourcing hook
    just=(just)
    command -v just &>/dev/null && return 0
    # shellcheck disable=SC2034  # read by the sourcing hook
    just=(mise exec -- just)
}
