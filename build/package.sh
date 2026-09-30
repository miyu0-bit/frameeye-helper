#!/usr/bin/env bash
# Puts the finished downloads together in dist/ (run build-helper.sh and build-preview.sh first):
#   dist/FrameEyeHelper.exe            the Windows app
#   dist/FrameEyeHelper-Windows.zip    the same, with a short "read me first", for sending
#   dist/linux/                        the same app for Linux, plus the Eye panel preview
#   dist/FrameEyeHelper-Linux.tar.gz   dist/linux/ packed up for the release page
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
dist="$root/dist"
if [[ -f "$dist/linux/FrameEyeHelper" && ! -f "$dist/FrameEyeHelper-linux" ]]; then
    mv "$dist/linux/FrameEyeHelper" "$dist/FrameEyeHelper-linux"   # packaging again
fi
test -f "$dist/FrameEyeHelper.exe" && test -f "$dist/FrameEyeHelper-linux" || { echo "run build/build-helper.sh first" >&2; exit 1; }
test -x "$root/preview/bin/frameeyeosc-panel-preview" || { echo "run build/build-preview.sh first" >&2; exit 1; }

rm -rf "$dist/linux" "$dist/FrameEyeHelper-Windows.zip"
mkdir -p "$dist/linux/FrameEye-Panel-Preview/bin"
mv "$dist/FrameEyeHelper-linux" "$dist/linux/FrameEyeHelper"
cp "$root/preview/run-panel-preview.sh" "$dist/linux/FrameEye-Panel-Preview/"
cp "$root/preview/bin/"* "$dist/linux/FrameEye-Panel-Preview/bin/"
cp "$root/helper/assets/FRAMEEYEOSC_LICENSE.txt" "$dist/linux/FrameEye-Panel-Preview/FRAMEEYEOSC_LICENSE.txt"
cp "$root/upstream/frameeyeosc/THIRD_PARTY_LICENSES.md" "$dist/linux/FrameEye-Panel-Preview/FRAMEEYEOSC_THIRD_PARTY_LICENSES.md"
cp "$root/LICENSE" "$root/THIRD_PARTY_LICENSES.md" "$dist/linux/"

cat > "$dist/linux/README.txt" <<'EOF'
FrameEye Helper for Linux: the same app as FrameEyeHelper.exe, plus a preview of the headset's Eye panel.

  FrameEyeHelper
      Double-click it (or run ./FrameEyeHelper). "Try the demo" on the first page walks through every
      step with a pretend headset. The "Open the Eye panel preview" button (Linux only) opens the
      in-headset Eye panel in a window.

  FrameEye-Panel-Preview/run-panel-preview.sh
      The same preview from a terminal. It runs the real frameeyeosc and its Eye panel, fed by a pretend
      Steam Frame. Space (or the button at the top) closes and opens the pretend dashboard; with it closed
      you see what the headset shows, like the eye fit's dot. --reset starts over with default settings.

Nothing here sends anything to VRChat: the preview's frameeyeosc runs in its own empty network, and its
settings live in a scratch folder ($XDG_RUNTIME_DIR/frameeye-preview) that is gone after logging out.
Needs SDL2, cairo, freetype and the Noto Sans CJK fonts (all standard on Fedora).
EOF

readme="$(mktemp)"
cat > "$readme" <<'EOF'
FrameEye Helper - Steam Frame eye tracking for VRChat
======================================================

1. Double-click FrameEyeHelper.exe.
   If Windows says "Windows protected your PC", click "More info", then "Run anyway".
   (The app isn't signed by a company, so Windows doesn't know it yet. Nothing gets installed on this PC.)

2. Follow the 4 steps in the window:
   Prepare headset -> Connect -> Install -> Check it works

You only need it once. After that, eye tracking starts by itself whenever SteamVR runs on the headset.
EOF
python3 - "$dist/FrameEyeHelper-Windows.zip" "$dist/FrameEyeHelper.exe" "$readme" "$root/LICENSE" "$root/THIRD_PARTY_LICENSES.md" <<'EOF'
import sys, zipfile
out, exe, readme, license_file, third_party = sys.argv[1:6]
crlf = lambda path: open(path, encoding="utf-8").read().replace("\n", "\r\n")
with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
    z.write(exe, "FrameEyeHelper/FrameEyeHelper.exe")
    z.writestr("FrameEyeHelper/READ ME FIRST.txt", crlf(readme))
    z.writestr("FrameEyeHelper/LICENSE.txt", crlf(license_file))
    z.writestr("FrameEyeHelper/THIRD_PARTY_LICENSES.md", crlf(third_party))
EOF
rm -f "$readme"
rm -f "$dist/FrameEyeHelper-Linux.tar.gz"
tar -C "$dist" --transform 's,^linux,FrameEyeHelper-Linux,' -czf "$dist/FrameEyeHelper-Linux.tar.gz" linux
(cd "$dist" && sha256sum FrameEyeHelper-Windows.zip FrameEyeHelper-Linux.tar.gz FrameEyeHelper.exe > SHA256SUMS)
find "$dist" -type f -printf '%10s  %P\n' | sort -k2
