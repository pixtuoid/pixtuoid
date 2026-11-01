#!/usr/bin/env bash
# The fuzz targets into $OUT, each beside its `<target>_seed_corpus.zip`.
set -euo pipefail

# The image's own nightly can predate what rust-toolchain.toml's release admits.
nightly=$(sed -n 's/^API_NIGHTLY := "\(.*\)"$/\1/p' justfile)
rustup toolchain install "$nightly" --profile minimal
export RUSTUP_TOOLCHAIN=$nightly

cp Cargo.lock fuzz/Cargo.lock
cargo fuzz build
for target in fuzz/fuzz_targets/*.rs; do
    cp "fuzz/target/x86_64-unknown-linux-gnu/release/$(basename "$target" .rs)" "$OUT"
done

seeds=$WORK/seeds
mkdir -p "$seeds"/{hook,transcript,remote}

# One seed per recording, whole, so a fold's state survives from line to line.
# A transcript leads with its source and path (`harness::fuzz::transcript`), the
# source spelled by its module directory (`harness::source_of`).
fixtures=crates/pixtuoid-core/tests/sources
while IFS= read -r file; do
    name=$(sha256sum <"$file" | cut -c1-32)
    case $file in
    */hook-payloads.jsonl) cp "$file" "$seeds/hook/$name" ;;
    *)
        module=${file#"$fixtures"/}
        {
            printf '%s\n' "${module%%/*}" | tr _ -
            printf '%s\n' "$file"
            cat "$file"
        } >"$seeds/transcript/$name"
        ;;
    esac
done < <(find "$fixtures" -path '*/fixtures/*' -name '*.jsonl')

# A stream of two lines, the second keeping the first's desks, beside a seed for
# each other parser `harness::fuzz::remote` drives.
cp fuzz/seeds/remote/* "$seeds/remote"
line=$(python3 -c 'import json, sys; print(json.dumps(json.load(sys.stdin)))' \
    <crates/pixtuoid-core/src/remote.v1.golden.json)
printf '%s\n%s\n' "$line" "$line" >"$seeds/remote/stream"

for target in hook transcript remote; do
    python3 -m zipfile -c "$OUT/${target}_seed_corpus.zip" "$seeds/$target"/*
done
