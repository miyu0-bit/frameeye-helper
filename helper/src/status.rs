//! frameeyeosc's status.json (written ten times a second on the headset) and what the helper makes of it.
//! Only the fields the helper shows are read; everything is optional so newer versions still parse.

use serde::Deserialize;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct StatusJson {
    pub pid: u32,
    /// Unix seconds when the file was written (headset clock).
    pub time: f64,
    pub started: f64,
    pub sending: bool,
    /// "vrchat" or "etvr"
    pub output: String,
    /// "auto" or "fixed"
    pub target_mode: String,
    /// "192.168.1.20:9000" once a destination is known.
    pub target: Option<String>,
    /// OSC samples sent in the last second.
    pub rate: f32,
    /// Samples the eye tracker delivered in the last second.
    pub tracker_rate: Option<f32>,
    pub tracking: bool,
    pub raw: Option<RawValues>,
    pub sent: Option<SentValues>,
    pub config_error: Option<String>,
    pub effective: Option<Effective>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct RawValues {
    pub openness: [f32; 2],
    pub gaze: [f32; 2],
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct SentValues {
    /// Eyelids on the VRCFT scale: 0 closed, 0.75 relaxed, 1 widened.
    pub lids_vrcft: [f32; 2],
    pub gaze: [f32; 2],
    pub gaze_left: [f32; 2],
    pub gaze_right: [f32; 2],
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Effective {
    pub host: String,
    pub port: Option<u16>,
    pub prefix: String,
    pub independent_eyes: bool,
}

/// One reading of the headset's state, as the status stream reports it.
#[derive(Clone, Debug, Default)]
pub struct LiveStatus {
    /// `systemctl --user is-active` of frameeyeosc: "active", "inactive", "failed", "activating", ...
    pub service: String,
    pub panel_service: String,
    pub steamvr: String,
    pub eye_server: bool,
    /// The headset's clock when this was read, to judge how fresh status.json is.
    pub headset_now: f64,
    pub status: Option<StatusJson>,
}

/// What the "Check it works" page shows as the headline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    /// Nothing heard from the headset yet.
    Unknown,
    /// The service isn't running (or its status file is stale).
    NotRunning,
    /// Running, but eye tracking isn't delivering (headset not worn, SteamVR not running).
    NoEyeData,
    /// Tracking, but auto mode hasn't found the Steam Link PC yet.
    NoTarget,
    /// Sending was switched off in the panel.
    Paused,
    /// Sending eye data.
    Sending,
}

impl LiveStatus {
    /// The status file counts as current if it was written in the last 3 seconds by a running service.
    pub fn fresh_status(&self) -> Option<&StatusJson> {
        let status = self.status.as_ref()?;
        let age = self.headset_now - status.time;
        (self.service == "active" && (-2.0..3.0).contains(&age)).then_some(status)
    }

    pub fn health(&self) -> Health {
        if self.service.is_empty() {
            return Health::Unknown;
        }
        let Some(status) = self.fresh_status() else {
            return Health::NotRunning;
        };
        if !status.sending {
            Health::Paused
        } else if !status.tracking {
            Health::NoEyeData
        } else if status.target.is_none() {
            Health::NoTarget
        } else {
            Health::Sending
        }
    }
}

/// Parse one frame of the status stream: `key=value` lines, the status JSON after `status=`.
pub fn parse_frame(lines: &[String], previous: &LiveStatus) -> LiveStatus {
    let mut live = previous.clone();
    for line in lines {
        let Some((key, value)) = line.split_once('=') else { continue };
        match key {
            "active" => live.service = value.trim().to_string(),
            "panel_active" => live.panel_service = value.trim().to_string(),
            "steamvr" => live.steamvr = value.trim().to_string(),
            "eyeserver" => live.eye_server = value.trim() == "yes",
            "now" => live.headset_now = value.trim().parse().unwrap_or(live.headset_now),
            "status" => {
                live.status = if value.trim().is_empty() {
                    None
                } else {
                    // A half-read file is simply skipped; the next frame comes 0.2 s later.
                    serde_json::from_str(value).ok().or(live.status)
                }
            }
            _ => {}
        }
    }
    live
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"version":1,"pid":4242,"time":1000.5,"started":900.0,"sending":true,"output":"vrchat","target_mode":"auto","target":"192.168.1.20:9000","rate":90.0,"tracker_rate":120.0,"tracking":true,"raw":{"openness":[0.8,0.79],"openness_scaled":[0.8,0.8],"gaze":[0.1,-0.05],"gaze_left":[0.1,-0.05],"gaze_right":[0.1,-0.05]},"sent":{"lids":[0.75,0.75],"lids_vrcft":[0.75,0.74],"gaze":[0.09,-0.04],"gaze_left":[0.09,-0.04],"gaze_right":[0.09,-0.04]},"calibration":{"enabled":true,"relaxed":[0.8,0.8],"scales":[1.0,1.0],"fitted":[false,false],"learning":false},"config_path":"/home/steamos/.config/frameeyeosc/config.json","calibration_path":null,"config_error":null,"locked":[],"effective":{"sending":true,"output":"vrchat","host":"auto","port":null,"prefix":"/FT","independent_eyes":false},"gaze_capture":null}"#;

    fn frame(status: &str) -> Vec<String> {
        vec![
            "active=active".into(),
            "panel_active=inactive".into(),
            "eyeserver=yes".into(),
            "now=1001.0".into(),
            format!("status={status}"),
        ]
    }

    #[test]
    fn a_running_sender_is_healthy() {
        let live = parse_frame(&frame(SAMPLE), &LiveStatus::default());
        assert_eq!(live.health(), Health::Sending);
        let status = live.fresh_status().unwrap();
        assert_eq!(status.target.as_deref(), Some("192.168.1.20:9000"));
        assert_eq!(status.sent.as_ref().unwrap().lids_vrcft, [0.75, 0.74]);
        assert_eq!(status.effective.as_ref().unwrap().prefix, "/FT");
    }

    #[test]
    fn a_stale_status_file_means_not_running() {
        let mut lines = frame(SAMPLE);
        lines[3] = "now=1010.0".into();
        assert_eq!(parse_frame(&lines, &LiveStatus::default()).health(), Health::NotRunning);
    }

    #[test]
    fn states_follow_the_flags() {
        let paused = SAMPLE.replace(r#""sending":true,"output""#, r#""sending":false,"output""#);
        assert_eq!(parse_frame(&frame(&paused), &LiveStatus::default()).health(), Health::Paused);
        let untracked = SAMPLE.replace(r#""tracking":true"#, r#""tracking":false"#);
        assert_eq!(parse_frame(&frame(&untracked), &LiveStatus::default()).health(), Health::NoEyeData);
        let no_target = SAMPLE.replace(r#""target":"192.168.1.20:9000""#, r#""target":null"#);
        assert_eq!(parse_frame(&frame(&no_target), &LiveStatus::default()).health(), Health::NoTarget);
    }

    #[test]
    fn a_broken_line_keeps_the_last_good_status() {
        let good = parse_frame(&frame(SAMPLE), &LiveStatus::default());
        let broken = parse_frame(&frame("{\"pid\":"), &good);
        assert_eq!(broken.status.unwrap().pid, 4242);
    }
}
