#!/usr/bin/env bash
# Runs every contract in contracts.yml against each of its files as committed,
# where it must pass, and against each of its breaks, where it must fail.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
contracts_path=${1:-policy/ci-observability/contracts.yml}
contracts=$(yq -o=json '.' "$contracts_path")
# A second document here would reach the loop's arithmetic as two counts, which
# bash rejects without tripping `set -e`: every contract would go unrun.
if ! jq -s -e 'length == 1 and (.[0] | length > 0)' <<<"$contracts" >/dev/null; then
    echo "error: $contracts_path must be one non-empty YAML document" >&2
    exit 1
fi

# A misspelled key (`expect:`) is read as absent, so its contract would pass
# without the value it was written to carry; a break YAML reads as a map (an
# unquoted `: `) fires on its own text, proving nothing.
shape=$(jq -r '
    def text: type == "string" and length > 0;
    .[] | (.id // "?") as $id
    | ((keys - ["id", "file", "why", "assert", "break", "expected", "covers", "against", "parse-runs"])[] | "\($id): unknown key `\(.)`"),
      (select(.break | text or (type == "array" and length > 0 and all(.[]; text)) | not)
          | "\($id): break must be a jq edit or a non-empty list of them")' <<<"$contracts")
if [[ -n $shape ]]; then
    printf 'error: %s: %s\n' "$contracts_path" "$shape" >&2
    exit 1
fi
count=$(jq 'length' <<<"$contracts")

# The document a `parse-runs` contract judges: each `run` beside its bash AST
# from shfmt, or null where shfmt refuses it, which an assert must judge red
# itself.
with_run_asts() {
    local document=$1 path ast
    command -v shfmt >/dev/null || {
        echo "error: a parse-runs contract needs shfmt (mise.toml)" >&2
        exit 1
    }
    while IFS= read -r path; do
        ast=$(jq -r --argjson p "$path" 'getpath($p).run' <<<"$document" | shfmt --to-json 2>/dev/null) || ast=null
        document=$(jq -c --argjson p "$path" --argjson ast "$ast" 'setpath($p + ["run_ast"]; $ast)' <<<"$document")
    done < <(jq -c 'paths(objects | .run | type == "string")' <<<"$document")
    printf '%s\n' "$document"
}

# The current contract's verdict on a document, committed or broken, so both
# sides are judged on the same input.
judge() {
    local document=$1
    [[ $parse_runs != true ]] || document=$(with_run_asts "$document") || return
    jq -e --arg expected "$expected" --argjson against "$against" "$assertion" <<<"$document"
}

failed=0
for ((i = 0; i < count; i++)); do
    id=$(jq -r ".[$i].id" <<<"$contracts")
    # Exactly one truthy result: jq -e alone judges only the last of several.
    assertion="[$(jq -r ".[$i].assert" <<<"$contracts")] | length == 1 and .[0] != false and .[0] != null"
    # One JSON string per line: a break written as a YAML block spans lines.
    breaks=$(jq -c ".[$i].break | [.] | flatten | .[]" <<<"$contracts")
    expected=$(jq -r ".[$i].expected // \"\"" <<<"$contracts")
    parse_runs=$(jq -r ".[$i][\"parse-runs\"] // false" <<<"$contracts")
    against_path=$(jq -r ".[$i].against // \"\"" <<<"$contracts")
    against=null
    if [[ -n $against_path ]] && { ! against=$(yq -o=json '.' "$against_path") ||
        [[ $(jq -s 'length' <<<"$against") != 1 ]]; }; then
        echo "error: $id cannot read $against_path as one document" >&2
        failed=1
        continue
    fi
    files=$(jq -r ".[$i].file | [.] | flatten | .[]" <<<"$contracts")
    # A contract over every file holding `covers` must list each: a new file
    # holding it would otherwise go unchecked with the contract still green.
    covers=$(jq -r ".[$i].covers // \"\"" <<<"$contracts")
    if [[ -n $covers ]]; then
        unlisted=$(comm -23 <(git grep -l --untracked -F -e "$covers" -- .github | sort) <(sort <<<"$files"))
        if [[ -n $unlisted ]]; then
            printf 'error: %s does not list these files holding %s:\n%s\n' "$id" "$covers" "$unlisted" >&2
            failed=1
        fi
    fi
    while IFS= read -r file; do
        # yq's `==` is false for any two maps or arrays, identical ones included,
        # so yq only turns the file into JSON and jq makes every comparison.
        if ! document=$(yq -o=json '.' "$file") || [[ $(jq -s 'length' <<<"$document") != 1 ]]; then
            echo "error: $id cannot read $file as one document" >&2
            failed=1
            continue
        fi
        if ! judge "$document" >/dev/null; then
            printf 'error: %s breaks contract %s\n  %s\n' "$file" "$id" "$(jq -r ".[$i].why" <<<"$contracts")" >&2
            failed=1
            continue
        fi
        while IFS= read -r encoded; do
            breakage=$(jq -r '.' <<<"$encoded")
            if ! broken=$(jq -c "$breakage" <<<"$document") ||
                [[ $(jq -s 'length == 1 and (.[0] | type) == "object"' <<<"$broken") != true ]]; then
                printf "error: %s's break does not yield one document from %s:\n%s\n" "$id" "$file" "$breakage" >&2
                failed=1
                continue
            fi
            if judge "$broken" >/dev/null 2>&1; then
                printf 'error: %s still passes on %s after this break, so that clause cannot fire:\n%s\n' "$id" "$file" "$breakage" >&2
                failed=1
            fi
        done <<<"$breaks"
    done <<<"$files"
done

if ((failed)); then
    exit 1
fi
echo "$count CI contracts hold, and each one fires on every break"
