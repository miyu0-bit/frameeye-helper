#!/usr/bin/env bash
# Builds the pretend Steam Frame image. Needs the preview binaries (build/build-preview.sh) first.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
bin="$here/../../preview/bin"
mkdir -p "$here/opt"
cp "$bin/frameeyeosc" "$bin/fake-eye-server" "$here/opt/"
podman build -t fakeframe "$here"
