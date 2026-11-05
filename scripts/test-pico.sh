#!/bin/zsh
# Full check of the files on the Pico (explorer, no BOOTSEL), on a plugged-in board.
#   scripts/test-pico.sh [pico2|pico]      (default: pico2)
# 1. unit tests of the file system (power cuts, sizes, names…)
# 2. builds the app (debug)
# 3. plays the self-test scenario in the real app
# 4. reads the Pico with picotool only (no Gauthier) and compares, byte for byte
set -e
setopt null_glob
BOARD=${1:-pico2}
ROOT=${0:A:h:h}
OUT=${TMPDIR:-/tmp}/gauthier-test-$BOARD
export PATH="$HOME/.cargo/bin:$PATH"
cd $ROOT

echo "== 1. unit tests"
# On a copy: cargo would leave a Cargo.lock in the bundled resources.
rm -rf $OUT-fs-src && cp -R src-tauri/resources/picorust-runtime/fs-core $OUT-fs-src
(cd $OUT-fs-src && CARGO_TARGET_DIR=$OUT-fs-target cargo test --quiet 2>&1 | grep -E "test result|FAILED|panicked" | head -1)

echo "== 2. build"
npx tauri build --debug --bundles app 2>&1 | grep -E "^error|Finished 1" || true

echo "== 3. self-test in the app ($BOARD)"
rm -rf $OUT.txt $OUT.txt.*
APP=src-tauri/target/debug/bundle/macos/Gauthier.app/Contents/MacOS/gauthier
env -u PATH PATH=/usr/bin:/bin GAUTHIER_SELFTEST=$BOARD-files GAUTHIER_SELFTEST_OUT=$OUT.txt $APP >$OUT.log 2>&1 || true
cat $OUT.txt
grep -q FAIL $OUT.txt && { echo "self-test FAILED"; exit 1; }

echo "== 4. independent check with picotool"
python3 -I scripts/pico_files.py verify main.rs $OUT.txt.expected.rs
echo "ALL CHECKS PASSED"
