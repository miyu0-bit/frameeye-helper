#!/usr/bin/env bash
# Builds the Linux preview of the headset's Eye panel inside the frameeye-build container:
# frameeyeosc (the headset program, for this PC), the pretend eye tracker, and the panel as a window.
# Results go to preview/bin/.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
image=frameeye-build
if ! podman image exists "$image"; then
    podman build -t "$image" -f "$root/build/Containerfile" "$root/build"
fi
mkdir -p "$root/build/cargo-home" "$root/preview/bin"
podman run --rm --userns=keep-id \
    -v "$root:/w:Z" \
    -v "$root/build/cargo-home:/cargo-home:Z" \
    "$image" bash -c '
        set -euo pipefail
        cd /w/upstream/frameeyeosc && cargo build --release --locked --target-dir /w/build/target-upstream
        cd /w/preview/fake-eye-server && cargo build --release --target-dir /w/build/target-fake
        cmake -G Ninja -S /w/preview/panel-desktop -B /w/build/panel-preview -DCMAKE_BUILD_TYPE=Release >/dev/null
        ninja -C /w/build/panel-preview
    '
cp "$root/build/target-upstream/release/frameeyeosc" "$root/preview/bin/"
cp "$root/build/target-fake/release/fake-eye-server" "$root/preview/bin/"
cp "$root/build/panel-preview/frameeyeosc-panel-preview" "$root/preview/bin/"
chmod +x "$root/preview/run-panel-preview.sh"
ls -la "$root/preview/bin"
