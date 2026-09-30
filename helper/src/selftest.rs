//! `--selftest HOST PORT USER PASSWORD` (only with the "selftest" feature): drives the real worker, i.e.
//! the same SSH code the buttons use, through the whole flow against a test headset (tests/fakeframe),
//! checking each step. Also listens on UDP 9011 to see the eye data actually arrive. (Never VRChat's
//! 9000: a VRChat running on the test PC would get the made-up eye data.)

use std::net::UdpSocket;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::remote::Installed;
use crate::status::Health;
use crate::worker::{Cmd, Event, Problem, TaskKind, Worker};

const TEST_PORT: u16 = 9011;

struct Run {
    worker: Worker,
    failures: usize,
}

impl Run {
    /// Wait for an event `want` accepts (logging the others), up to `seconds`.
    fn expect(&mut self, what: &str, seconds: u64, mut want: impl FnMut(&Event) -> bool) -> Option<Event> {
        let deadline = Instant::now() + Duration::from_secs(seconds);
        while Instant::now() < deadline {
            for event in self.worker.poll() {
                if want(&event) {
                    println!("  PASS  {what}");
                    return Some(event);
                }
                match &event {
                    Event::TaskLine(line) => println!("        | {line}"),
                    Event::ConnectFailed { problem, detail, .. } => println!("        (connect failed: {problem:?}: {detail})"),
                    Event::TaskDone { ok, message, .. } => println!("        (task done ok={ok}: {message})"),
                    Event::ActionDone { label, ok, message } => println!("        ({label} ok={ok}: {message})"),
                    Event::Disconnected { reason } => println!("        (disconnected: {reason})"),
                    _ => {}
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        println!("  FAIL  {what} (nothing within {seconds} s)");
        self.failures += 1;
        None
    }

    fn check(&mut self, what: &str, ok: bool) {
        println!("  {}  {what}", if ok { "PASS" } else { "FAIL" });
        if !ok {
            self.failures += 1;
        }
    }
}

pub fn run(args: &[String]) -> i32 {
    let [host, port, user, password] = args else {
        eprintln!("usage: FrameEyeHelper --selftest HOST PORT USER PASSWORD");
        return 2;
    };
    let port: u16 = port.parse().unwrap_or(22);
    let connect = |password: &str, known: Option<String>| Cmd::Connect {
        host: host.clone(),
        port,
        user: user.clone(),
        password: password.to_string(),
        known_fingerprint: known,
    };
    // Count OSC packets that reach this PC, on a test port instead of VRChat's 9000
    let received = Arc::new(AtomicUsize::new(0));
    match UdpSocket::bind(format!("0.0.0.0:{TEST_PORT}")) {
        Ok(socket) => {
            let received = received.clone();
            std::thread::spawn(move || {
                let mut buffer = [0u8; 1024];
                while let Ok(n) = socket.recv(&mut buffer) {
                    if buffer[..n].starts_with(b"/avatar/parameters/") {
                        received.fetch_add(1, Ordering::Relaxed);
                    }
                }
            });
        }
        Err(error) => println!("  (UDP {TEST_PORT} is busy, so OSC arrival isn't checked: {error})"),
    }

    let mut run = Run {
        worker: Worker::start(egui::Context::default()),
        failures: 0,
    };

    println!("1. Wrong password is reported as such");
    run.worker.send(connect("not-the-password", None));
    run.expect("WrongPassword", 20, |e| matches!(e, Event::ConnectFailed { problem: Problem::WrongPassword, .. }));

    println!("2. A changed host key is caught");
    run.worker.send(connect(password, Some("SHA256:somethingElse".into())));
    run.expect("HostKeyChanged", 20, |e| {
        matches!(e, Event::ConnectFailed { problem: Problem::HostKeyChanged, fingerprint: Some(_), .. })
    });

    println!("3. Connect and read the headset");
    run.worker.send(connect(password, None));
    let Some(Event::Connected { info, fingerprint, .. }) = run.expect("Connected", 30, |e| matches!(e, Event::Connected { .. })) else {
        println!("cannot continue without a connection");
        return 1;
    };
    println!("        fingerprint {fingerprint}, arch {}, os {}, this PC as seen by the headset: {:?}", info.arch, info.os, info.client_ip);
    run.check("looks like a Steam Frame (aarch64)", info.is_frame());
    run.check("eye tracker present", info.eye_server);
    let fresh = info.installed() == Installed::No;
    println!("        installed before: {:?}", info.installed());

    println!("4. Install with the panel");
    run.worker.send(Cmd::Install { with_panel: true });
    run.expect("install finished OK", 120, |e| matches!(e, Event::TaskDone { kind: TaskKind::Install, ok: true, .. }));
    if let Some(Event::Info(info)) = run.expect("fresh headset info", 30, |e| matches!(e, Event::Info(_))) {
        run.check(&format!("installed version is 0.5.3 ({:?})", info.installed()), info.installed() == Installed::Version("0.5.3".into()));
        run.check("service active", info.service_active == "active");
    }

    println!("5. Live status");
    run.worker.send(Cmd::StartStatus);
    let mut last = None;
    run.expect("status with a running frameeyeosc and eye data", 20, |e| match e {
        Event::Status(live) if live.health() != Health::Unknown && live.health() != Health::NotRunning => {
            last = Some(live.clone());
            live.fresh_status().is_some_and(|status| status.tracking)
        }
        _ => false,
    });
    if let Some(live) = &last {
        let status = live.fresh_status();
        println!(
            "        health {:?}, target {:?}, rate {:?}, tracker {:?}",
            live.health(),
            status.and_then(|s| s.target.clone()),
            status.map(|s| s.rate),
            status.and_then(|s| s.tracker_rate)
        );
    }

    println!("6. Send to this PC");
    // A real headset reaches the PC at the address it sees the SSH login come from. Rootless podman's
    // default networking gives the test container the PC's own address, so the test may name another.
    let this_pc = std::env::var("SELFTEST_PC_ADDRESS")
        .ok()
        .or_else(|| info.client_ip.map(|ip| ip.to_string()))
        .unwrap_or_else(|| "127.0.0.1".into());
    run.worker.send(Cmd::SetConfig {
        changes: vec![
            ("host".into(), serde_json::Value::String(this_pc.clone())),
            ("port".into(), serde_json::Value::from(TEST_PORT)),
        ],
        label: "host".into(),
    });
    run.expect("config written", 20, |e| matches!(e, Event::ActionDone { ok: true, .. }));
    let expected = format!("{this_pc}:{TEST_PORT}");
    run.expect(&format!("status now sends to {expected}"), 20, |e| match e {
        Event::Status(live) => live.health() == Health::Sending && live.fresh_status().and_then(|s| s.target.as_deref()) == Some(expected.as_str()),
        _ => false,
    });
    std::thread::sleep(Duration::from_secs(2));
    let packets = received.load(Ordering::Relaxed);
    println!("        OSC packets that reached this PC's UDP {TEST_PORT}: {packets}");
    run.check("eye data arrived at this PC", packets > 100);

    println!("7. Pause and resume");
    run.worker.send(Cmd::SetConfig { changes: vec![("sending".into(), false.into())], label: "pause".into() });
    run.expect("paused", 20, |e| matches!(e, Event::Status(live) if live.health() == Health::Paused));
    run.worker.send(Cmd::SetConfig { changes: vec![("sending".into(), true.into())], label: "resume".into() });
    run.expect("sending again", 20, |e| matches!(e, Event::Status(live) if live.health() == Health::Sending));
    run.worker.send(Cmd::StopStatus);

    println!("8. Logs and restart");
    run.worker.send(Cmd::FetchLogs);
    if let Some(Event::Logs(text)) = run.expect("logs", 20, |e| matches!(e, Event::Logs(_))) {
        run.check("log mentions sending", text.contains("Sending OSC to"));
    }
    run.worker.send(Cmd::RestartService);
    run.expect("restart OK", 30, |e| matches!(e, Event::ActionDone { ok: true, .. }));

    println!("9. Uninstall (with --purge)");
    run.worker.send(Cmd::Uninstall { purge: true });
    run.expect("uninstall finished OK", 120, |e| matches!(e, Event::TaskDone { kind: TaskKind::Uninstall, ok: true, .. }));
    if let Some(Event::Info(info)) = run.expect("headset info after", 30, |e| matches!(e, Event::Info(_))) {
        run.check("nothing installed any more", info.installed() == Installed::No);
    }
    run.worker.send(Cmd::Disconnect);
    std::thread::sleep(Duration::from_millis(300));

    println!();
    println!(
        "{} (fresh headset: {fresh}, OSC packets received: {packets})",
        if run.failures == 0 { "ALL PASSED".to_string() } else { format!("{} FAILED", run.failures) }
    );
    i32::from(run.failures > 0)
}
