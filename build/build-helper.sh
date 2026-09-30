#!/usr/bin/env bash
# Builds FrameEye Helper for Windows (x86_64, .exe) and Linux inside the frameeye-build container,
# runs the unit tests, and copies the results to dist/.
#   build/build-helper.sh            (from the project root, or anywhere)
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
image=frameeye-build
if ! podman image exists "$image"; then
    podman build -t "$image" -f "$root/build/Containerfile" "$root/build"
fi
mkdir -p "$root/build/cargo-home" "$root/dist"
podman run --rm --userns=keep-id \
    -v "$root/helper:/src:Z" \
    -v "$root/build/cargo-home:/cargo-home:Z" \
    -e CARGO_TARGET_DIR=/src/target-container \
    "$image" bash -c '
        set -euo pipefail
        cargo test --release --locked
        cargo build --release --locked --target x86_64-unknown-linux-gnu
        cargo build --release --locked --target x86_64-pc-windows-gnu
    '
cp "$root/helper/target-container/x86_64-pc-windows-gnu/release/FrameEyeHelper.exe" "$root/dist/FrameEyeHelper.exe"
cp "$root/helper/target-container/x86_64-unknown-linux-gnu/release/FrameEyeHelper" "$root/dist/FrameEyeHelper-linux"
ls -la "$root/dist"
