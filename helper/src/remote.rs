//! Every command the helper runs on the headset, in one place so they are easy to review.
//! They only touch the user's own home folder and user services (no sudo), and only through
//! frameeyeosc's own install.sh.

use std::net::IpAddr;

use crate::bundle;

/// Where the release is unpacked while installing (removed afterwards).
const STAGE: &str = r#"d="$HOME/.cache/frameeye-helper""#;
/// `systemctl --user` needs the user's runtime dir, which some SSH setups don't export.
const RUNTIME: &str = r#"export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}""#;

pub fn info() -> String {
    format!(
        r#"{RUNTIME}
b="$HOME/.local/bin"
echo "arch=$(uname -m)"
echo "user=$(id -un)"
if [ -e /dev/shm/eye-server.mmap ]; then echo "eyeserver=yes"; else echo "eyeserver=no"; fi
if [ -x "$b/frameeyeosc" ]; then echo "daemon=yes"; echo "daemon_sha=$(sha256sum "$b/frameeyeosc" 2>/dev/null | cut -d' ' -f1)"; else echo "daemon=no"; fi
if [ -x "$b/frameeyeosc-panel" ]; then echo "panel=yes"; echo "panel_version=$("$b/frameeyeosc-panel" --version 2>/dev/null | head -n 1)"; else echo "panel=no"; fi
echo "active=$(systemctl --user is-active frameeyeosc.service 2>/dev/null)"
echo "enabled=$(systemctl --user is-enabled frameeyeosc.service 2>/dev/null)"
echo "panel_active=$(systemctl --user is-active frameeyeosc-panel.service 2>/dev/null)"
echo "steamvr=$(systemctl --user is-active steamvr.service 2>/dev/null)"
if [ -r /etc/os-release ]; then . /etc/os-release; echo "os=${{PRETTY_NAME:-${{NAME:-}}}} ${{VERSION_ID:-}}"; fi
echo "ssh=${{SSH_CONNECTION:-}}"
missing=""; for tool in tar sha256sum systemctl; do command -v "$tool" >/dev/null 2>&1 || missing="$missing $tool"; done
echo "missing=$missing"
"#
    )
}

/// Reads the release from stdin into the staging folder and prints its SHA-256.
pub fn upload() -> String {
    format!(
        r#"set -e
{STAGE}
mkdir -p "$d"
cat > "$d/package.tar.gz.part"
mv -f "$d/package.tar.gz.part" "$d/package.tar.gz"
sha256sum "$d/package.tar.gz" | cut -d' ' -f1
"#
    )
}

/// Unpacks the uploaded release and runs its install.sh; with the panel, also starts it right away
/// (install.sh would otherwise only start it with the next SteamVR start).
pub fn install(with_panel: bool) -> String {
    let (args, start_panel) = if with_panel {
        ("--with-panel", "systemctl --user start frameeyeosc-panel.service >/dev/null 2>&1 || true")
    } else {
        ("", "")
    };
    format!(
        r#"set -e
{RUNTIME}
{STAGE}
rm -rf "$d/frameeyeosc"
tar xzf "$d/package.tar.gz" -C "$d"
cd "$d/frameeyeosc"
./install.sh {args}
{start_panel}
cd "$HOME"
rm -rf "$d"
"#
    )
}

/// Removes frameeyeosc and the panel with the release's own install.sh (settings stay unless `purge`).
pub fn uninstall(purge: bool) -> String {
    let purge = if purge { " --purge" } else { "" };
    format!(
        r#"set -e
{RUNTIME}
{STAGE}
rm -rf "$d/frameeyeosc"
tar xzf "$d/package.tar.gz" -C "$d"
cd "$d/frameeyeosc"
./install.sh --uninstall{purge}
cd "$HOME"
rm -rf "$d"
"#
    )
}

/// Prints the service state and status.json five times a second until the channel closes. Each
/// frame ends with a `--end--` line.
pub fn status_stream() -> String {
    format!(
        r#"{RUNTIME}
f="$XDG_RUNTIME_DIR/frameeyeosc/status.json"
i=0
while :; do
  if [ $((i % 5)) -eq 0 ]; then
    echo "active=$(systemctl --user is-active frameeyeosc.service 2>/dev/null)"
    echo "panel_active=$(systemctl --user is-active frameeyeosc-panel.service 2>/dev/null)"
    echo "steamvr=$(systemctl --user is-active steamvr.service 2>/dev/null)"
    if [ -e /dev/shm/eye-server.mmap ]; then echo "eyeserver=yes"; else echo "eyeserver=no"; fi
  fi
  echo "now=$(date +%s.%N)"
  if [ -r "$f" ]; then printf 'status='; tr -d '\n' < "$f"; echo; else echo "status="; fi
  echo "--end--"
  i=$((i + 1))
  sleep 0.2
done
"#
    )
}

pub fn logs() -> String {
    format!(
        r#"{RUNTIME}
echo "== frameeyeosc (newest last) =="
journalctl --user -u frameeyeosc.service -n 60 --no-pager -o short 2>&1 || true
echo
echo "== frameeyeosc-panel =="
journalctl --user -u frameeyeosc-panel.service -n 25 --no-pager -o short 2>&1 || true
"#
    )
}

pub fn restart_service() -> String {
    format!(
        r#"{RUNTIME}
systemctl --user enable frameeyeosc.service 2>&1
systemctl --user restart frameeyeosc.service 2>&1
"#
    )
}

/// Starts the panel service if needed, then asks the running panel to open itself on the dashboard
/// (a second launch signals the resident one and exits at once).
pub fn show_panel() -> String {
    format!(
        r#"{RUNTIME}
systemctl --user start frameeyeosc-panel.service 2>&1 || true
sleep 2
"$HOME/.local/bin/frameeyeosc-panel" 2>&1 | tail -n 1
"#
    )
}

pub fn read_config() -> String {
    r#"cat "${XDG_CONFIG_HOME:-$HOME/.config}/frameeyeosc/config.json" 2>/dev/null || true"#.to_string()
}

/// Replaces config.json with stdin (atomically, like the panel does). frameeyeosc picks it up within 0.1 s.
pub fn write_config() -> String {
    r#"set -e
c="${XDG_CONFIG_HOME:-$HOME/.config}/frameeyeosc"
mkdir -p "$c"
cat > "$c/config.json.tmp"
mv -f "$c/config.json.tmp" "$c/config.json"
"#
    .to_string()
}

/// What `info()` found on the headset.
#[derive(Clone, Debug, Default)]
pub struct HeadsetInfo {
    pub host: String,
    pub user: String,
    pub arch: String,
    pub eye_server: bool,
    pub daemon_installed: bool,
    pub daemon_sha: String,
    pub panel_installed: bool,
    /// "0.5.3", from `frameeyeosc-panel --version`.
    pub panel_version: Option<String>,
    pub service_active: String,
    pub service_enabled: String,
    pub panel_active: String,
    pub steamvr: String,
    pub os: String,
    /// This PC's address as the headset sees it (from SSH_CONNECTION).
    pub client_ip: Option<IpAddr>,
    pub missing_tools: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Installed {
    No,
    Version(String),
    /// Installed, but the version can't be told (a daemon-only install of another version).
    Unknown,
}

impl HeadsetInfo {
    pub fn parse(text: &str, host: &str, user: &str) -> Self {
        let mut info = HeadsetInfo {
            host: host.to_string(),
            user: user.to_string(),
            ..Default::default()
        };
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            let value = value.trim();
            match key.trim() {
                "arch" => info.arch = value.into(),
                "eyeserver" => info.eye_server = value == "yes",
                "daemon" => info.daemon_installed = value == "yes",
                "daemon_sha" => info.daemon_sha = value.into(),
                "panel" => info.panel_installed = value == "yes",
                "panel_version" => {
                    info.panel_version = value
                        .split_whitespace()
                        .last()
                        .filter(|version| version.chars().next().is_some_and(|c| c.is_ascii_digit()))
                        .map(str::to_string)
                }
                "active" => info.service_active = value.into(),
                "enabled" => info.service_enabled = value.into(),
                "panel_active" => info.panel_active = value.into(),
                "steamvr" => info.steamvr = value.into(),
                "os" => info.os = value.into(),
                "ssh" => info.client_ip = value.split_whitespace().next().and_then(|ip| ip.parse().ok()),
                "missing" => info.missing_tools = value.into(),
                _ => {}
            }
        }
        info
    }

    pub fn is_frame(&self) -> bool {
        self.arch == "aarch64"
    }

    pub fn installed(&self) -> Installed {
        if !self.daemon_installed {
            Installed::No
        } else if let Some(version) = &self.panel_version {
            Installed::Version(version.clone())
        } else if self.daemon_sha == bundle::DAEMON_SHA256 {
            Installed::Version(bundle::VERSION.into())
        } else {
            Installed::Unknown
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_output_is_parsed() {
        let text = "arch=aarch64\nuser=steamos\neyeserver=yes\ndaemon=yes\ndaemon_sha=abc\npanel=yes\n\
                    panel_version=frameeyeosc-panel 0.5.3\nactive=active\nenabled=enabled\npanel_active=active\n\
                    steamvr=active\nos=SteamOS 3.8\nssh=192.168.1.20 51234 192.168.1.42 22\nmissing=\n";
        let info = HeadsetInfo::parse(text, "192.168.1.42", "steamos");
        assert!(info.is_frame());
        assert!(info.eye_server);
        assert_eq!(info.installed(), Installed::Version("0.5.3".into()));
        assert_eq!(info.client_ip, Some("192.168.1.20".parse().unwrap()));
        assert_eq!(info.os, "SteamOS 3.8");
    }

    #[test]
    fn daemon_only_installs_are_recognised_by_hash() {
        let text = format!("arch=aarch64\ndaemon=yes\ndaemon_sha={}\npanel=no\n", bundle::DAEMON_SHA256);
        let info = HeadsetInfo::parse(&text, "h", "u");
        assert_eq!(info.installed(), Installed::Version(bundle::VERSION.into()));
        let other = HeadsetInfo::parse("daemon=yes\ndaemon_sha=ffff\npanel=no\n", "h", "u");
        assert_eq!(other.installed(), Installed::Unknown);
        assert_eq!(HeadsetInfo::parse("daemon=no\n", "h", "u").installed(), Installed::No);
    }

    #[test]
    fn commands_are_plain_posix_sh() {
        // Braces meant for the shell must survive format!().
        assert!(info().contains("${PRETTY_NAME:-${NAME:-}}"));
        assert!(install(true).contains("./install.sh --with-panel"));
        assert!(!install(false).contains("--with-panel"));
        assert!(uninstall(true).contains("--uninstall --purge"));
    }
}
