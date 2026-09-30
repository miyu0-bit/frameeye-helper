//! A background thread that talks to the headset over SSH (or pretends to, in demo mode), so the
//! window never freezes. The UI sends `Cmd`s and gets `Event`s back.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use russh::client::{self, Handle, KeyboardInteractiveAuthResponse};
use russh::keys::{HashAlg, PublicKeyOrCertificate};
use russh::{ChannelMsg, Disconnect, MethodKind};
use tokio::net::TcpStream;
use tokio::sync::mpsc as tmpsc;
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::bundle;
use crate::net::{self, ScanHit};
use crate::remote::{self, HeadsetInfo};
use crate::status::{self, LiveStatus, StatusJson};

pub enum Cmd {
    Connect { host: String, port: u16, user: String, password: String, known_fingerprint: Option<String> },
    ConnectDemo,
    Disconnect,
    RefreshInfo,
    Install { with_panel: bool },
    Uninstall { purge: bool },
    StartStatus,
    StopStatus,
    FetchLogs,
    RestartService,
    ShowPanel,
    /// Change keys in frameeyeosc's config.json (kept: every other key).
    SetConfig { changes: Vec<(String, serde_json::Value)>, label: String },
    Scan { port: u16 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Problem {
    BadAddress,
    Unreachable,
    Refused,
    Timeout,
    Handshake,
    HostKeyChanged,
    WrongPassword,
    NoPasswordLogin,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskKind {
    Install,
    Uninstall,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TaskStep {
    Uploading,
    Verifying,
    Running,
}

pub enum Event {
    Connected { info: HeadsetInfo, fingerprint: String, demo: bool },
    ConnectFailed { problem: Problem, detail: String, fingerprint: Option<String> },
    Info(HeadsetInfo),
    TaskStep(TaskStep),
    TaskLine(String),
    UploadProgress(f32),
    TaskDone { kind: TaskKind, ok: bool, message: String },
    Status(LiveStatus),
    StatusProblem(String),
    Logs(String),
    ActionDone { label: String, ok: bool, message: String },
    ScanProgress(f32),
    ScanDone { hits: Vec<ScanHit>, networks: usize },
    Disconnected { reason: String },
}

pub struct Worker {
    commands: tmpsc::UnboundedSender<Cmd>,
    events: mpsc::Receiver<Event>,
}

impl Worker {
    pub fn start(ctx: egui::Context) -> Self {
        let (commands, command_rx) = tmpsc::unbounded_channel();
        let (event_tx, events) = mpsc::channel();
        let out = Emitter { tx: event_tx, ctx };
        std::thread::Builder::new()
            .name("headset".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .expect("could not start the async runtime");
                runtime.block_on(run(command_rx, out));
            })
            .expect("could not start the worker thread");
        Self { commands, events }
    }

    pub fn send(&self, cmd: Cmd) {
        let _ = self.commands.send(cmd);
    }

    pub fn poll(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }
}

#[derive(Clone)]
struct Emitter {
    tx: mpsc::Sender<Event>,
    ctx: egui::Context,
}

impl Emitter {
    fn emit(&self, event: Event) {
        let _ = self.tx.send(event);
        self.ctx.request_repaint();
    }
}

// ---------------------------------------------------------------------------------------------
// SSH

/// Remembers the host key it was shown, and accepts it if it is the one trusted before (or if
/// there is none yet: trust on first use, like `ssh -o StrictHostKeyChecking=accept-new`).
struct Client {
    expected: Option<String>,
    seen: Arc<Mutex<Option<String>>>,
}

impl client::Handler for Client {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let fingerprint = match key {
            PublicKeyOrCertificate::PublicKey { key, .. } => key.fingerprint(HashAlg::Sha256).to_string(),
            PublicKeyOrCertificate::Certificate(certificate) => {
                certificate.public_key().fingerprint(HashAlg::Sha256).to_string()
            }
        };
        let accepted = self.expected.as_ref().is_none_or(|expected| *expected == fingerprint);
        *self.seen.lock().unwrap() = Some(fingerprint);
        Ok(accepted)
    }
}

struct Conn {
    handle: Arc<Handle<Client>>,
    host: String,
    user: String,
}

struct Output {
    stdout: String,
    code: Option<u32>,
}

/// Run one command. `stdin` is sent first (with `on_upload` progress); each stdout line goes to
/// `on_line` as it arrives.
async fn exec(
    handle: &Handle<Client>,
    command: &str,
    stdin: Option<&[u8]>,
    mut on_upload: impl FnMut(f32),
    mut on_line: impl FnMut(&str),
) -> Result<Output, russh::Error> {
    let mut channel = handle.channel_open_session().await?;
    channel.exec(true, command).await?;
    if let Some(bytes) = stdin {
        let mut sent = 0usize;
        for chunk in bytes.chunks(64 * 1024) {
            channel.data(chunk).await?;
            sent += chunk.len();
            on_upload(sent as f32 / bytes.len().max(1) as f32);
        }
    }
    channel.eof().await?;
    let mut stdout = Vec::new();
    let mut line_start = 0usize;
    let mut code = None;
    while let Some(message) = channel.wait().await {
        match message {
            ChannelMsg::Data { data } => {
                stdout.extend_from_slice(&data);
                while let Some(end) = stdout[line_start..].iter().position(|byte| *byte == b'\n') {
                    on_line(String::from_utf8_lossy(&stdout[line_start..line_start + end]).trim_end());
                    line_start += end + 1;
                }
            }
            // stderr: the scripts send everything that matters to stdout
            ChannelMsg::ExtendedData { .. } => {}
            ChannelMsg::ExitStatus { exit_status } => code = Some(exit_status),
            ChannelMsg::Close => break,
            _ => {}
        }
    }
    if line_start < stdout.len() {
        on_line(String::from_utf8_lossy(&stdout[line_start..]).trim_end());
    }
    Ok(Output {
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        code,
    })
}

async fn exec_simple(handle: &Handle<Client>, command: &str, limit: Duration) -> Result<Output, String> {
    match timeout(limit, exec(handle, command, None, |_| {}, |_| {})).await {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => Err(format!("the headset didn't answer within {} seconds", limit.as_secs())),
    }
}

fn io_problem(error: &std::io::Error, addr: SocketAddr) -> (Problem, String) {
    use std::io::ErrorKind::*;
    match error.kind() {
        ConnectionRefused => (Problem::Refused, format!("{addr} refused the connection ({error}).")),
        TimedOut => (Problem::Timeout, format!("No answer from {addr} ({error}).")),
        HostUnreachable | NetworkUnreachable | AddrNotAvailable => {
            (Problem::Unreachable, format!("{addr} can't be reached ({error})."))
        }
        _ => (Problem::Other, format!("Connecting to {addr} failed: {error}")),
    }
}

type ConnectError = (Problem, String, Option<String>);

async fn connect(
    host: &str,
    port: u16,
    user: &str,
    password: &str,
    known: Option<String>,
) -> Result<(Conn, HeadsetInfo, String), ConnectError> {
    let host = host.trim();
    if host.is_empty() {
        return Err((Problem::BadAddress, "Type the headset's IP address first.".into(), None));
    }
    let addrs: Vec<SocketAddr> = match timeout(Duration::from_secs(6), tokio::net::lookup_host((host, port))).await {
        Ok(Ok(addrs)) => addrs.collect(),
        Ok(Err(error)) => return Err((Problem::BadAddress, format!("\"{host}\" isn't an address this PC can find ({error})."), None)),
        Err(_) => return Err((Problem::BadAddress, format!("Looking up \"{host}\" took too long."), None)),
    };
    let mut stream = None;
    let mut last = (Problem::BadAddress, format!("\"{host}\" didn't give any address."));
    for addr in addrs {
        match timeout(Duration::from_secs(6), TcpStream::connect(addr)).await {
            Ok(Ok(connected)) => {
                stream = Some(connected);
                break;
            }
            Ok(Err(error)) => last = io_problem(&error, addr),
            Err(_) => last = (Problem::Timeout, format!("No answer from {addr} within 6 seconds.")),
        }
    }
    let Some(stream) = stream else {
        return Err((last.0, last.1, None));
    };
    let _ = stream.set_nodelay(true);

    let seen = Arc::new(Mutex::new(None));
    let handler = Client {
        expected: known.clone(),
        seen: seen.clone(),
    };
    let config = Arc::new(client::Config {
        keepalive_interval: Some(Duration::from_secs(10)),
        keepalive_max: 3,
        nodelay: true,
        ..Default::default()
    });
    let seen_key = || seen.lock().unwrap().clone();
    let mut handle = match timeout(Duration::from_secs(15), client::connect_stream(config, stream, handler)).await {
        Ok(Ok(handle)) => handle,
        Ok(Err(error)) => {
            let fingerprint = seen_key();
            if let (Some(expected), Some(shown)) = (&known, &fingerprint) {
                if expected != shown {
                    return Err((
                        Problem::HostKeyChanged,
                        format!("Expected {expected}, but the headset showed {shown}."),
                        fingerprint,
                    ));
                }
            }
            return Err((Problem::Handshake, format!("The secure connection couldn't be set up: {error}"), fingerprint));
        }
        Err(_) => return Err((Problem::Timeout, "The headset didn't finish setting up the secure connection.".into(), seen_key())),
    };
    let fingerprint = seen_key().unwrap_or_default();

    authenticate(&mut handle, user, password)
        .await
        .map_err(|(problem, detail)| (problem, detail, Some(fingerprint.clone())))?;

    let handle = Arc::new(handle);
    let info = fetch_info(&handle, host, user)
        .await
        .map_err(|detail| (Problem::Other, format!("Logged in, but reading the headset's state failed: {detail}"), Some(fingerprint.clone())))?;
    Ok((
        Conn {
            handle,
            host: host.to_string(),
            user: user.to_string(),
        },
        info,
        fingerprint,
    ))
}

/// Password login, falling back to keyboard-interactive (the same password) where the server
/// only offers that.
async fn authenticate(handle: &mut Handle<Client>, user: &str, password: &str) -> Result<(), (Problem, String)> {
    let other = |error: russh::Error| (Problem::Other, format!("Logging in failed: {error}"));
    let remaining = match handle.authenticate_password(user, password).await.map_err(other)? {
        client::AuthResult::Success => return Ok(()),
        client::AuthResult::Failure { remaining_methods, .. } => remaining_methods,
    };
    if remaining.contains(&MethodKind::KeyboardInteractive) {
        let mut response = handle
            .authenticate_keyboard_interactive_start(user, None)
            .await
            .map_err(other)?;
        for _ in 0..6 {
            match response {
                KeyboardInteractiveAuthResponse::Success => return Ok(()),
                KeyboardInteractiveAuthResponse::Failure { .. } => break,
                KeyboardInteractiveAuthResponse::InfoRequest { prompts, .. } => {
                    let answers = prompts
                        .iter()
                        .map(|prompt| if prompt.echo { String::new() } else { password.to_string() })
                        .collect();
                    response = handle
                        .authenticate_keyboard_interactive_respond(answers)
                        .await
                        .map_err(other)?;
                }
            }
        }
    }
    if remaining.contains(&MethodKind::Password) || remaining.contains(&MethodKind::KeyboardInteractive) {
        Err((Problem::WrongPassword, format!("The headset didn't accept the password for user \"{user}\".")))
    } else {
        let methods: Vec<&str> = remaining.iter().map(<&str>::from).collect();
        Err((
            Problem::NoPasswordLogin,
            format!("The headset doesn't allow password login (it offers: {}).", methods.join(", ")),
        ))
    }
}

async fn fetch_info(handle: &Handle<Client>, host: &str, user: &str) -> Result<HeadsetInfo, String> {
    let output = exec_simple(handle, &remote::info(), Duration::from_secs(20)).await?;
    Ok(HeadsetInfo::parse(&output.stdout, host, user))
}

/// Upload the bundled release, check it arrived intact, then run install.sh (or its uninstall).
async fn run_task(conn: &Conn, kind: TaskKind, with_panel: bool, purge: bool, out: &Emitter) -> Result<String, String> {
    if !bundle::verify() {
        return Err("The copy of frameeyeosc inside this program is damaged. Download the helper again.".into());
    }
    out.emit(Event::TaskStep(TaskStep::Uploading));
    let upload = timeout(
        Duration::from_secs(120),
        exec(&conn.handle, &remote::upload(), Some(bundle::TARBALL), |fraction| out.emit(Event::UploadProgress(fraction)), |_| {}),
    )
    .await
    .map_err(|_| "Sending the files to the headset took too long.".to_string())?
    .map_err(|error| format!("Sending the files to the headset failed: {error}"))?;
    out.emit(Event::TaskStep(TaskStep::Verifying));
    let arrived = upload.stdout.trim();
    if upload.code != Some(0) || arrived != bundle::TARBALL_SHA256 {
        return Err(format!(
            "The files didn't arrive intact (checksum {}). Try again.",
            if arrived.is_empty() { "missing" } else { arrived }
        ));
    }
    out.emit(Event::TaskLine(format!("Upload verified (SHA-256 {}…)", &arrived[..16])));
    out.emit(Event::TaskStep(TaskStep::Running));
    let script = match kind {
        TaskKind::Install => remote::install(with_panel),
        TaskKind::Uninstall => remote::uninstall(purge),
    };
    let script = format!("exec 2>&1\n{script}");
    let result = timeout(
        Duration::from_secs(180),
        exec(&conn.handle, &script, None, |_| {}, |line| out.emit(Event::TaskLine(line.to_string()))),
    )
    .await
    .map_err(|_| "install.sh didn't finish within 3 minutes.".to_string())?
    .map_err(|error| format!("Running install.sh failed: {error}"))?;
    match result.code {
        Some(0) => Ok(match kind {
            TaskKind::Install => "Installed.".into(),
            TaskKind::Uninstall => "Removed.".into(),
        }),
        Some(code) => Err(format!("install.sh stopped with exit code {code}. The details above say why.")),
        None => Err("install.sh ended without an exit code (the connection may have dropped).".into()),
    }
}

async fn set_config(conn: &Conn, changes: &[(String, serde_json::Value)]) -> Result<(), String> {
    let current = exec_simple(&conn.handle, &remote::read_config(), Duration::from_secs(15)).await?;
    let text = current.stdout.trim();
    let mut config: serde_json::Value = if text.is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(text).map_err(|_| {
            "The headset's settings file is damaged. Open the Eye panel in the headset and use \"Reset all\" on the Basic tab."
                .to_string()
        })?
    };
    let Some(object) = config.as_object_mut() else {
        return Err("The headset's settings file isn't a JSON object.".into());
    };
    for (key, value) in changes {
        object.insert(key.clone(), value.clone());
    }
    let json = serde_json::to_vec_pretty(&config).map_err(|error| error.to_string())?;
    let written = timeout(
        Duration::from_secs(15),
        exec(&conn.handle, &remote::write_config(), Some(&json), |_| {}, |_| {}),
    )
    .await
    .map_err(|_| "Writing the settings took too long.".to_string())?
    .map_err(|error| error.to_string())?;
    if written.code == Some(0) { Ok(()) } else { Err("Writing the settings on the headset failed.".into()) }
}

/// Stream status.json until the connection goes away; restarts the stream if only it fails.
async fn stream_status(handle: Arc<Handle<Client>>, out: Emitter) {
    let mut last = LiveStatus::default();
    loop {
        let result: Result<(), russh::Error> = async {
            let mut channel = handle.channel_open_session().await?;
            channel.exec(true, remote::status_stream()).await?;
            channel.eof().await?;
            let mut buffer: Vec<u8> = Vec::new();
            let mut frame: Vec<String> = Vec::new();
            while let Some(message) = channel.wait().await {
                let ChannelMsg::Data { data } = message else { continue };
                buffer.extend_from_slice(&data);
                while let Some(end) = buffer.iter().position(|byte| *byte == b'\n') {
                    let line: Vec<u8> = buffer.drain(..=end).collect();
                    let line = String::from_utf8_lossy(&line).trim_end().to_string();
                    if line == "--end--" {
                        last = status::parse_frame(&frame, &last);
                        frame.clear();
                        out.emit(Event::Status(last.clone()));
                    } else {
                        frame.push(line);
                    }
                }
            }
            Ok(())
        }
        .await;
        if handle.is_closed() {
            out.emit(Event::Disconnected {
                reason: "The connection to the headset was lost (was it turned off or did it leave the network?).".into(),
            });
            return;
        }
        if let Err(error) = result {
            out.emit(Event::StatusProblem(error.to_string()));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

// ---------------------------------------------------------------------------------------------
// Demo mode: a pretend headset, so the whole flow can be tried (and shown) without a Steam Frame.

struct Demo {
    installed: bool,
    with_panel: bool,
    shared: Arc<Mutex<DemoState>>,
}

#[derive(Clone)]
struct DemoState {
    sending: bool,
    host: String,
}

const DEMO_PC: &str = "192.168.1.20";

fn demo_info(demo: &Demo) -> HeadsetInfo {
    HeadsetInfo {
        host: "192.168.1.42".into(),
        user: "steamos".into(),
        arch: "aarch64".into(),
        eye_server: true,
        daemon_installed: demo.installed,
        daemon_sha: if demo.installed { bundle::DAEMON_SHA256.into() } else { String::new() },
        panel_installed: demo.installed && demo.with_panel,
        panel_version: (demo.installed && demo.with_panel).then(|| bundle::VERSION.to_string()),
        service_active: if demo.installed { "active" } else { "inactive" }.into(),
        service_enabled: if demo.installed { "enabled" } else { "not-found" }.into(),
        panel_active: if demo.installed && demo.with_panel { "active" } else { "inactive" }.into(),
        steamvr: "active".into(),
        os: "SteamOS (demo)".into(),
        client_ip: DEMO_PC.parse().ok(),
        missing_tools: String::new(),
    }
}

/// Eyes that look around, blink every few seconds and now and then look far to one side.
fn demo_status(t: f64, state: &DemoState) -> LiveStatus {
    let fixation = (t / 1.4).floor();
    let hash = |n: f64| ((n * 12.9898).sin() * 43758.5453).fract().abs() as f32;
    let settle = (((t / 1.4).fract() as f32) * 8.0).min(1.0);
    let target = |n: f64| [hash(n) * 1.1 - 0.55, hash(n + 7.0) * 0.7 - 0.35];
    let (from, to) = (target(fixation - 1.0), target(fixation));
    let gaze = [from[0] + (to[0] - from[0]) * settle, from[1] + (to[1] - from[1]) * settle];
    let blink_phase = (t % 4.3) as f32;
    let lid = if blink_phase < 0.12 { 0.0 } else if blink_phase < 0.25 { (blink_phase - 0.12) / 0.13 * 0.75 } else { 0.75 };
    let wink = (t % 17.0) > 15.0 && (t % 17.0) < 15.5;
    let lids = [lid, if wink { 0.0 } else { lid }];
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64());
    let target = if state.host == "auto" { format!("{DEMO_PC}:9000") } else { format!("{}:9000", state.host) };
    LiveStatus {
        service: "active".into(),
        panel_service: "active".into(),
        steamvr: "active".into(),
        eye_server: true,
        headset_now: now,
        status: Some(StatusJson {
            pid: 4242,
            time: now,
            started: now - 600.0,
            sending: state.sending,
            output: "vrchat".into(),
            target_mode: if state.host == "auto" { "auto" } else { "fixed" }.into(),
            target: Some(target),
            rate: if state.sending { 118.0 } else { 0.0 },
            tracker_rate: Some(118.0 + (t * 3.0).sin() as f32 * 2.0),
            tracking: true,
            raw: Some(status::RawValues {
                openness: [0.3 + lids[0] * 0.66, 0.3 + lids[1] * 0.66],
                gaze: [gaze[0] + 0.02, gaze[1] - 0.01],
            }),
            sent: Some(status::SentValues {
                lids_vrcft: lids,
                gaze,
                gaze_left: gaze,
                gaze_right: gaze,
            }),
            config_error: None,
            effective: Some(status::Effective {
                host: state.host.clone(),
                port: None,
                prefix: "/FT".into(),
                independent_eyes: false,
            }),
        }),
    }
}

const DEMO_INSTALL_OUTPUT: &[&str] = &[
    "Created symlink '/home/steamos/.config/systemd/user/steamvr.service.wants/frameeyeosc.service' → '/home/steamos/.config/systemd/user/frameeyeosc.service'.",
    "Created symlink '/home/steamos/.config/systemd/user/steamvr.service.wants/frameeyeosc-panel.service' → '/home/steamos/.config/systemd/user/frameeyeosc-panel.service'.",
    "● frameeyeosc.service - Send Steam Frame eye tracking to VRChat over OSC",
    "     Loaded: loaded (/home/steamos/.config/systemd/user/frameeyeosc.service; enabled; preset: enabled)",
    "     Active: active (running)",
    "",
    "frameeyeosc is installed and starts together with SteamVR.",
    "  Settings: /home/steamos/.config/frameeyeosc/config.json",
    "  Logs:     journalctl --user -u frameeyeosc -f",
    "",
    "The panel starts with SteamVR from the next SteamVR start (\"Eye\" on the dashboard).",
    "",
    "On your PC, turn off Steam Link's own OSC output (SteamVR settings > Steam Link > OSC),",
    "otherwise it drives the avatar's eyes too with unsmoothed data.",
];

// ---------------------------------------------------------------------------------------------

enum Link {
    None,
    Ssh(Conn),
    Demo(Demo),
}

async fn run(mut commands: tmpsc::UnboundedReceiver<Cmd>, out: Emitter) {
    let mut link = Link::None;
    let mut status_task: Option<JoinHandle<()>> = None;
    let stop_status = |task: &mut Option<JoinHandle<()>>| {
        if let Some(task) = task.take() {
            task.abort();
        }
    };
    while let Some(cmd) = commands.recv().await {
        match cmd {
            Cmd::Scan { port } => {
                let out = out.clone();
                tokio::spawn(async move {
                    let progress = out.clone();
                    let (hits, networks) = net::scan(port, move |fraction| progress.emit(Event::ScanProgress(fraction))).await;
                    out.emit(Event::ScanDone { hits, networks });
                });
            }
            Cmd::Connect { host, port, user, password, known_fingerprint } => {
                stop_status(&mut status_task);
                if let Link::Ssh(conn) = std::mem::replace(&mut link, Link::None) {
                    let _ = conn.handle.disconnect(Disconnect::ByApplication, "", "en").await;
                }
                match connect(&host, port, &user, &password, known_fingerprint).await {
                    Ok((conn, info, fingerprint)) => {
                        link = Link::Ssh(conn);
                        out.emit(Event::Connected { info, fingerprint, demo: false });
                    }
                    Err((problem, detail, fingerprint)) => out.emit(Event::ConnectFailed { problem, detail, fingerprint }),
                }
            }
            Cmd::ConnectDemo => {
                stop_status(&mut status_task);
                tokio::time::sleep(Duration::from_millis(700)).await;
                let demo = Demo {
                    installed: false,
                    with_panel: true,
                    shared: Arc::new(Mutex::new(DemoState { sending: true, host: "auto".into() })),
                };
                out.emit(Event::Connected {
                    info: demo_info(&demo),
                    fingerprint: "SHA256:demo".into(),
                    demo: true,
                });
                link = Link::Demo(demo);
            }
            Cmd::Disconnect => {
                stop_status(&mut status_task);
                if let Link::Ssh(conn) = std::mem::replace(&mut link, Link::None) {
                    let _ = conn.handle.disconnect(Disconnect::ByApplication, "", "en").await;
                }
            }
            Cmd::RefreshInfo => match &link {
                Link::Ssh(conn) => match fetch_info(&conn.handle, &conn.host, &conn.user).await {
                    Ok(info) => out.emit(Event::Info(info)),
                    Err(detail) => lost_or(&conn.handle, &out, detail),
                },
                Link::Demo(demo) => out.emit(Event::Info(demo_info(demo))),
                Link::None => {}
            },
            Cmd::Install { with_panel } => match &mut link {
                Link::Ssh(conn) => {
                    let result = run_task(conn, TaskKind::Install, with_panel, false, &out).await;
                    let (ok, message) = flatten(result);
                    out.emit(Event::TaskDone { kind: TaskKind::Install, ok, message });
                    if let Ok(info) = fetch_info(&conn.handle, &conn.host, &conn.user).await {
                        out.emit(Event::Info(info));
                    }
                }
                Link::Demo(demo) => {
                    out.emit(Event::TaskStep(TaskStep::Uploading));
                    for step in 1..=10 {
                        tokio::time::sleep(Duration::from_millis(90)).await;
                        out.emit(Event::UploadProgress(step as f32 / 10.0));
                    }
                    out.emit(Event::TaskStep(TaskStep::Verifying));
                    out.emit(Event::TaskLine(format!("Upload verified (SHA-256 {}…)", &bundle::TARBALL_SHA256[..16])));
                    tokio::time::sleep(Duration::from_millis(400)).await;
                    out.emit(Event::TaskStep(TaskStep::Running));
                    for line in DEMO_INSTALL_OUTPUT.iter().filter(|line| with_panel || !line.contains("panel")) {
                        tokio::time::sleep(Duration::from_millis(120)).await;
                        out.emit(Event::TaskLine((*line).to_string()));
                    }
                    demo.installed = true;
                    demo.with_panel = with_panel;
                    out.emit(Event::TaskDone { kind: TaskKind::Install, ok: true, message: "Installed.".into() });
                    out.emit(Event::Info(demo_info(demo)));
                }
                Link::None => out.emit(Event::TaskDone { kind: TaskKind::Install, ok: false, message: "Not connected to the headset.".into() }),
            },
            Cmd::Uninstall { purge } => match &mut link {
                Link::Ssh(conn) => {
                    stop_status(&mut status_task);
                    let (ok, message) = flatten(run_task(conn, TaskKind::Uninstall, false, purge, &out).await);
                    out.emit(Event::TaskDone { kind: TaskKind::Uninstall, ok, message });
                    if let Ok(info) = fetch_info(&conn.handle, &conn.host, &conn.user).await {
                        out.emit(Event::Info(info));
                    }
                }
                Link::Demo(demo) => {
                    stop_status(&mut status_task);
                    out.emit(Event::TaskStep(TaskStep::Running));
                    tokio::time::sleep(Duration::from_millis(600)).await;
                    out.emit(Event::TaskLine("Removed frameeyeosc and the panel.".into()));
                    demo.installed = false;
                    out.emit(Event::TaskDone { kind: TaskKind::Uninstall, ok: true, message: "Removed.".into() });
                    out.emit(Event::Info(demo_info(demo)));
                }
                Link::None => out.emit(Event::TaskDone { kind: TaskKind::Uninstall, ok: false, message: "Not connected to the headset.".into() }),
            },
            Cmd::StartStatus => {
                stop_status(&mut status_task);
                match &link {
                    Link::Ssh(conn) => status_task = Some(tokio::spawn(stream_status(conn.handle.clone(), out.clone()))),
                    Link::Demo(demo) => {
                        let shared = demo.shared.clone();
                        let installed = demo.installed;
                        let out = out.clone();
                        status_task = Some(tokio::spawn(async move {
                            let start = std::time::Instant::now();
                            loop {
                                let state = shared.lock().unwrap().clone();
                                let mut live = demo_status(start.elapsed().as_secs_f64(), &state);
                                if !installed {
                                    live.service = "inactive".into();
                                    live.status = None;
                                }
                                out.emit(Event::Status(live));
                                tokio::time::sleep(Duration::from_millis(200)).await;
                            }
                        }));
                    }
                    Link::None => {}
                }
            }
            Cmd::StopStatus => stop_status(&mut status_task),
            Cmd::FetchLogs => match &link {
                Link::Ssh(conn) => match exec_simple(&conn.handle, &remote::logs(), Duration::from_secs(20)).await {
                    Ok(output) => out.emit(Event::Logs(output.stdout)),
                    Err(detail) => lost_or(&conn.handle, &out, detail),
                },
                Link::Demo(_) => out.emit(Event::Logs(
                    "== frameeyeosc (newest last) ==\nReading /dev/shm/eye-server.mmap\nSending OSC to 192.168.1.20:9000\nEye tracking started\n(demo)\n".into(),
                )),
                Link::None => {}
            },
            Cmd::RestartService => {
                let label = "Start frameeyeosc".to_string();
                match &link {
                    Link::Ssh(conn) => match exec_simple(&conn.handle, &remote::restart_service(), Duration::from_secs(30)).await {
                        Ok(output) if output.code == Some(0) => out.emit(Event::ActionDone { label, ok: true, message: "frameeyeosc was (re)started.".into() }),
                        Ok(output) => out.emit(Event::ActionDone { label, ok: false, message: output.stdout }),
                        Err(detail) => lost_or(&conn.handle, &out, detail),
                    },
                    Link::Demo(_) => out.emit(Event::ActionDone { label, ok: true, message: "frameeyeosc was (re)started.".into() }),
                    Link::None => {}
                }
            }
            Cmd::ShowPanel => {
                let label = "Show the Eye panel".to_string();
                match &link {
                    Link::Ssh(conn) => match exec_simple(&conn.handle, &remote::show_panel(), Duration::from_secs(30)).await {
                        Ok(_) => out.emit(Event::ActionDone {
                            label,
                            ok: true,
                            message: "Asked the headset to open the Eye panel on the SteamVR dashboard (SteamVR must be running).".into(),
                        }),
                        Err(detail) => lost_or(&conn.handle, &out, detail),
                    },
                    Link::Demo(_) => out.emit(Event::ActionDone { label, ok: true, message: "The Eye panel would open in the headset now (demo).".into() }),
                    Link::None => {}
                }
            }
            Cmd::SetConfig { changes, label } => match &link {
                Link::Ssh(conn) => match set_config(conn, &changes).await {
                    Ok(()) => out.emit(Event::ActionDone { label, ok: true, message: "Saved on the headset; frameeyeosc uses it right away.".into() }),
                    Err(message) => {
                        if conn.handle.is_closed() {
                            lost_or(&conn.handle, &out, message);
                        } else {
                            out.emit(Event::ActionDone { label, ok: false, message });
                        }
                    }
                },
                Link::Demo(demo) => {
                    let mut state = demo.shared.lock().unwrap();
                    for (key, value) in &changes {
                        match (key.as_str(), value) {
                            ("sending", serde_json::Value::Bool(sending)) => state.sending = *sending,
                            ("host", serde_json::Value::String(host)) => state.host = host.clone(),
                            _ => {}
                        }
                    }
                    out.emit(Event::ActionDone { label, ok: true, message: "Saved (demo).".into() });
                }
                Link::None => {}
            },
        }
    }
}

fn flatten(result: Result<String, String>) -> (bool, String) {
    match result {
        Ok(message) => (true, message),
        Err(message) => (false, message),
    }
}

/// Report a failed request: as a lost connection if that's what it was.
fn lost_or(handle: &Handle<Client>, out: &Emitter, detail: String) {
    if handle.is_closed() {
        out.emit(Event::Disconnected { reason: format!("The connection to the headset was lost ({detail}).") });
    } else {
        out.emit(Event::ActionDone { label: "Headset".into(), ok: false, message: detail });
    }
}
