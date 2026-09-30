#!/usr/bin/env bash
# Try the Steam Frame's "Eye" panel (frameeyeosc-panel) on this Linux PC, with a pretend headset:
#   fake-eye-server   plays the Frame's eye tracker (made-up eyes that look around and blink)
#   frameeyeosc       the real program that runs on the headset (built for this PC)
#   the Eye panel     the real panel, in a window instead of the SteamVR dashboard
# Everything lives in a scratch folder (settings, status), so nothing on this PC is changed, and
# frameeyeosc runs in its own empty network, so it can't reach VRChat or anything else.
#
#   ./run-panel-preview.sh           start (close the window or press "Quit app" in the panel to stop)
#   ./run-panel-preview.sh --reset   start over with default settings
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
bin="$here/bin"
for program in fake-eye-server frameeyeosc frameeyeosc-panel-preview; do
    if [[ ! -x "$bin/$program" ]]; then
        echo "Missing $bin/$program. Build it first (see README.md)." >&2
        exit 1
    fi
done

runtime="${XDG_RUNTIME_DIR:-/tmp}"
sandbox="$runtime/frameeye-preview"
if [[ "${1:-}" == "--reset" ]]; then
    rm -rf "$sandbox"
    echo "Settings reset."
fi
mkdir -p "$sandbox/home" "$sandbox/config/frameeyeosc" "$sandbox/run"
chmod 700 "$sandbox" "$sandbox/run"
# One preview at a time: a second pretend eye tracker would replace the first one's shared memory
exec 9>"$sandbox/.lock"
if ! flock -n 9; then
    echo "The Eye panel preview is already open."
    exit 0
fi

# First start: send to "this PC" as a fixed address, as if the headset streamed to it
config="$sandbox/config/frameeyeosc/config.json"
if [[ ! -f "$config" ]]; then
    printf '{\n  "host": "127.0.0.1"\n}\n' > "$config"
fi

# The panel's update button calls this; in the preview it only ever says "up to date"
updater="$sandbox/home/.local/share/frameeyeosc/frame-update.sh"
mkdir -p "$(dirname "$updater")"
cat > "$updater" <<'EOF'
#!/bin/sh
# Preview stand-in for frameeyeosc's updater: never downloads or installs anything.
current=0.5.3
while [ $# -gt 0 ]; do [ "$1" = "--current" ] && current="$2"; shift; done
printf '{"status":"up-to-date","current":"%s","latest":"%s","url":"https://github.com/sasaken1102r/frameeyeosc/releases","cached":false,"checked_at":%s}\n' \
    "$current" "$current" "$(date +%s)"
EOF
chmod 755 "$updater"

env_sandbox=(HOME="$sandbox/home" XDG_CONFIG_HOME="$sandbox/config" XDG_CACHE_HOME="$sandbox/home/.cache")
pids=()
cleanup() {
    for pid in "${pids[@]}"; do kill "$pid" 2>/dev/null || true; done
    wait 2>/dev/null || true
    echo "Preview stopped."
}
trap cleanup EXIT INT TERM

echo "Starting the pretend eye tracker..."
env "${env_sandbox[@]}" XDG_RUNTIME_DIR="$sandbox/run" "$bin/fake-eye-server" &
pids+=($!)
sleep 0.3

echo "Starting frameeyeosc (in its own empty network, so nothing reaches VRChat)..."
isolate=(unshare --user --map-root-user --net sh -c 'ip link set lo up 2>/dev/null; exec "$0" "$@"')
if ! unshare --user --map-root-user --net true 2>/dev/null; then
    # No user namespaces: pin the destination to a port nothing uses, and lock it from the panel instead
    echo "  (can't isolate its network here; sending to 127.0.0.1:9019 instead, locked)"
    isolate=()
    daemon_args=(--target 127.0.0.1:9019)
else
    daemon_args=()
fi
env "${env_sandbox[@]}" XDG_RUNTIME_DIR="$sandbox/run" "${isolate[@]}" "$bin/frameeyeosc" "${daemon_args[@]}" &
pids+=($!)
sleep 0.5

echo "Opening the Eye panel. Space closes/opens the pretend dashboard."
# The panel keeps the real runtime dir for the window (Wayland) and sounds (PipeWire); its settings
# and status come from the scratch folder
env "${env_sandbox[@]}" "$bin/frameeyeosc-panel-preview" --status "$sandbox/run/frameeyeosc/status.json" || true
