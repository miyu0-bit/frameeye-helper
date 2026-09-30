# Testing without a headset

There's a fake Steam Frame here, so the app can be tested without a headset.

## The fake Frame

`tests/fakeframe` builds a container that acts like the headset as far as the app can tell: a real
OpenSSH server with a `steamos` user, the fake eye tracker from the preview, and frameeyeosc built for
x86_64. The bits of SteamOS a container doesn't have are faked with small scripts: `uname -m` says
aarch64 (frameeyeosc's installer checks it) and a tiny `systemctl --user` stand-in starts frameeyeosc.

```sh
build/build-preview.sh                # the fake eye tracker and frameeyeosc for x86_64
tests/fakeframe/build-image.sh
podman run -d --name fakeframe-test -p 127.0.0.1:2222:22 fakeframe
```

Password is `frame-test-pw`. Start it with `-e KBD_INTERACTIVE=1` to test a server that only does
keyboard-interactive logins.

## Self-test

Build the app with the `selftest` feature and point it at the container. It goes through the same code the
buttons use: wrong password, changed host key, connect, install (the real `install.sh`), live status,
sending to this PC, pause and resume, logs, restart and uninstall.

```sh
cd helper
cargo build --release --features selftest --target-dir target-selftest
SELFTEST_PC_ADDRESS=169.254.1.2 ./target-selftest/release/FrameEyeHelper --selftest 127.0.0.1 2222 steamos frame-test-pw
```

`SELFTEST_PC_ADDRESS` is only there because rootless podman gives the container the PC's own IP, so the
test has to use podman's address for the host (`host.containers.internal`) to see the eye data arrive. It
listens on UDP 9011, not 9000, so a VRChat running on the same PC doesn't get fake eyes.

The Windows build can run the same test under Wine.

## Screenshots

`FrameEyeHelper --screenshots DIR` clicks through the demo and saves each page. The panel preview takes a
little script in `PREVIEW_SCRIPT`, for example
`PREVIEW_SCRIPT="wait 3; click 796,52; key space; shot a.png; quit" preview/run-panel-preview.sh`.
