//! The window: four steps (prepare the headset, connect, install, check it works).

use std::net::IpAddr;
use std::path::PathBuf;
use std::time::Duration;

use egui::{Align, Color32, CornerRadius, FontId, Layout, Margin, RichText, Sense, Stroke, Ui, pos2, vec2};

use crate::bundle;
use crate::firewall;
use crate::net::{self, ScanHit};
use crate::remote::{HeadsetInfo, Installed};
use crate::settings::Settings;
use crate::status::{Health, LiveStatus};
use crate::worker::{Cmd, Event, Problem, TaskKind, TaskStep, Worker};

// The in-headset Eye panel's colours (panel/src/theme.h), so the two look like one product.
const BG: Color32 = Color32::from_rgb(0x0d, 0x11, 0x17);
const CARD: Color32 = Color32::from_rgb(0x16, 0x1b, 0x22);
const CONTROL: Color32 = Color32::from_rgb(0x21, 0x26, 0x2d);
const CONTROL_HOVER: Color32 = Color32::from_rgb(0x30, 0x36, 0x3d);
const DIVIDER: Color32 = Color32::from_rgb(0x30, 0x36, 0x3d);
const TEXT: Color32 = Color32::from_rgb(0xe6, 0xed, 0xf3);
const MUTED: Color32 = Color32::from_rgb(0x91, 0x98, 0xa1);
const ACCENT: Color32 = Color32::from_rgb(0xe2, 0x7d, 0xfd);
const ACCENT_PRESSED: Color32 = Color32::from_rgb(0xc4, 0x5f, 0xe0);
const ON_ACCENT: Color32 = BG;
const GREEN: Color32 = Color32::from_rgb(0x3f, 0xb9, 0x50);
const YELLOW: Color32 = Color32::from_rgb(0xd2, 0x99, 0x22);
const RED: Color32 = Color32::from_rgb(0xf8, 0x51, 0x49);
const BLUE: Color32 = Color32::from_rgb(0x58, 0xa6, 0xff);
const CONTENT_WIDTH: f32 = 700.0;

#[derive(Clone, Default)]
pub struct Flags {
    /// Start straight in demo mode.
    pub demo: bool,
    /// Walk through every page in demo mode, save a screenshot of each here, then quit.
    pub screenshot_dir: Option<PathBuf>,
}

impl Flags {
    pub fn parse(args: &[String]) -> Self {
        let mut flags = Flags::default();
        let mut i = 1;
        while i < args.len() {
            match args[i].as_str() {
                "--demo" => flags.demo = true,
                "--screenshots" if i + 1 < args.len() => {
                    flags.screenshot_dir = Some(PathBuf::from(&args[i + 1]));
                    i += 1;
                }
                _ => {}
            }
            i += 1;
        }
        flags
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Page {
    Prepare,
    Connect,
    Install,
    Check,
}

enum ScanState {
    Idle,
    Running(f32),
    Done { hits: Vec<ScanHit>, networks: usize },
}

struct Session {
    info: HeadsetInfo,
    demo: bool,
}

#[derive(Default)]
struct Task {
    kind: Option<TaskKind>,
    running: bool,
    step: Option<TaskStep>,
    upload: f32,
    lines: Vec<String>,
    result: Option<(bool, String)>,
}

struct Notice {
    ok: bool,
    text: String,
}

/// Smoothed eye values for the drawing (the headset reports five times a second).
#[derive(Clone, Copy, Default)]
struct EyeAnim {
    lids: [f32; 2],
    gaze: [[f32; 2]; 2],
}

struct Shooter {
    dir: PathBuf,
    step: usize,
    wait_until: f64,
    pending: bool,
}

pub struct App {
    worker: Worker,
    settings: Settings,
    page: Page,
    furthest: Page,
    local_ips: Vec<IpAddr>,
    logo: Option<egui::TextureHandle>,

    password: String,
    show_password: bool,
    connecting: bool,
    connect_error: Option<(Problem, String)>,
    disconnect_reason: Option<String>,
    host_key_prompt: Option<String>,
    scan: ScanState,

    session: Option<Session>,
    task: Task,
    show_details: bool,
    confirm_uninstall: bool,
    purge: bool,

    streaming: bool,
    live: Option<LiveStatus>,
    status_problem: Option<String>,
    anim: Option<EyeAnim>,
    logs: Option<String>,
    notice: Option<Notice>,
    firewall_result: Option<(bool, String)>,
    about_open: bool,

    shooter: Option<Shooter>,
    preview_result: Option<(bool, String)>,
    /// Scroll the install/uninstall result into view on the next frame.
    reveal_result: bool,
    /// False for --screenshots runs, which must neither read nor write the user's settings.
    persist: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, flags: Flags) -> Self {
        setup_style(&cc.egui_ctx);
        let logo = crate::icon_rgba().map(|(rgba, width, height)| {
            cc.egui_ctx.load_texture(
                "logo",
                egui::ColorImage::from_rgba_unmultiplied([width as usize, height as usize], &rgba),
                egui::TextureOptions::LINEAR,
            )
        });
        let worker = Worker::start(cc.egui_ctx.clone());
        let mut app = Self {
            worker,
            settings: Settings::load(),
            page: Page::Prepare,
            furthest: Page::Prepare,
            local_ips: net::local_ips(),
            logo,
            password: String::new(),
            show_password: false,
            connecting: false,
            connect_error: None,
            disconnect_reason: None,
            host_key_prompt: None,
            scan: ScanState::Idle,
            session: None,
            task: Task::default(),
            show_details: false,
            confirm_uninstall: false,
            purge: false,
            streaming: false,
            live: None,
            status_problem: None,
            anim: None,
            logs: None,
            notice: None,
            firewall_result: None,
            about_open: false,
            shooter: None,
            reveal_result: false,
            preview_result: None,
            persist: flags.screenshot_dir.is_none(),
        };
        if let Some(dir) = flags.screenshot_dir {
            app.settings = Settings::default();
            let _ = std::fs::create_dir_all(&dir);
            app.shooter = Some(Shooter { dir, step: 0, wait_until: 1.5, pending: false });
        } else if flags.demo {
            app.start_demo();
        }
        app
    }

    fn save_settings(&self) {
        if self.persist {
            self.settings.save();
        }
    }

    fn start_demo(&mut self) {
        self.connecting = true;
        self.connect_error = None;
        self.go(Page::Connect);
        self.worker.send(Cmd::ConnectDemo);
    }

    fn connect(&mut self) {
        let host = self.settings.host.trim().to_string();
        self.settings.host = host.clone();
        if host.is_empty() {
            self.connect_error = Some((Problem::BadAddress, String::new()));
            return;
        }
        let known = self.settings.known_hosts.get(&Settings::host_key_id(&host, self.settings.port)).cloned();
        self.connecting = true;
        self.connect_error = None;
        self.disconnect_reason = None;
        self.save_settings();
        self.local_ips = net::local_ips();
        self.worker.send(Cmd::Connect {
            host,
            port: self.settings.port,
            user: self.settings.user.trim().to_string(),
            password: self.password.clone(),
            known_fingerprint: known,
        });
    }

    fn disconnect(&mut self) {
        self.worker.send(Cmd::Disconnect);
        self.session = None;
        self.streaming = false;
        self.live = None;
        self.anim = None;
        self.logs = None;
        self.task = Task::default();
        self.go(Page::Connect);
    }

    fn can_visit(&self, page: Page) -> bool {
        match page {
            Page::Prepare | Page::Connect => true,
            Page::Install => self.session.is_some(),
            Page::Check => self.session.as_ref().is_some_and(|session| session.info.installed() != Installed::No),
        }
    }

    fn go(&mut self, page: Page) {
        self.page = page;
        self.furthest = self.furthest.max(page);
        self.notice = None;
    }

    fn handle_events(&mut self) {
        for event in self.worker.poll() {
            match event {
                Event::Connected { info, fingerprint, demo } => {
                    self.connecting = false;
                    self.connect_error = None;
                    if !demo {
                        let id = Settings::host_key_id(&self.settings.host, self.settings.port);
                        self.settings.known_hosts.entry(id).or_insert(fingerprint);
                        self.save_settings();
                        self.password.clear();
                    }
                    self.session = Some(Session { info, demo });
                    self.task = Task::default();
                    self.logs = None;
                    self.go(Page::Install);
                }
                Event::ConnectFailed { problem, detail, fingerprint } => {
                    self.connecting = false;
                    if problem == Problem::HostKeyChanged {
                        self.host_key_prompt = fingerprint;
                    }
                    self.connect_error = Some((problem, detail));
                }
                Event::Info(info) => {
                    if let Some(session) = &mut self.session {
                        session.info = info;
                    }
                }
                Event::TaskStep(step) => self.task.step = Some(step),
                Event::UploadProgress(fraction) => self.task.upload = fraction,
                Event::TaskLine(line) => self.task.lines.push(line),
                Event::TaskDone { kind, ok, message } => {
                    self.task.kind = Some(kind);
                    self.task.running = false;
                    self.task.result = Some((ok, message));
                    self.reveal_result = true;
                    if !ok {
                        self.show_details = true;
                    }
                }
                Event::Status(live) => {
                    self.status_problem = None;
                    self.live = Some(live);
                }
                Event::StatusProblem(problem) => self.status_problem = Some(problem),
                Event::Logs(text) => self.logs = Some(text),
                Event::ActionDone { label, ok, message } => {
                    self.notice = Some(Notice { ok, text: format!("{label}: {message}") });
                }
                Event::ScanProgress(fraction) => self.scan = ScanState::Running(fraction),
                Event::ScanDone { hits, networks } => {
                    if hits.len() == 1 && self.settings.host.trim().is_empty() {
                        self.settings.host = hits[0].ip.to_string();
                    }
                    self.scan = ScanState::Done { hits, networks };
                }
                Event::Disconnected { reason } => {
                    let was_connected = self.session.is_some();
                    self.disconnect();
                    self.connecting = false;
                    if was_connected {
                        self.disconnect_reason = Some(reason);
                    }
                }
            }
        }
    }

    /// Stream the headset's status only while the "Check it works" page is open.
    fn sync_status_stream(&mut self) {
        let want = self.page == Page::Check && self.session.is_some();
        if want != self.streaming {
            self.worker.send(if want { Cmd::StartStatus } else { Cmd::StopStatus });
            self.streaming = want;
            if !want {
                self.live = None;
                self.anim = None;
            }
        }
    }

    fn installed(&self) -> Installed {
        self.session.as_ref().map_or(Installed::No, |session| session.info.installed())
    }

    // -----------------------------------------------------------------------------------------
    // Header, stepper and footer

    fn header(&mut self, ui: &mut Ui) {
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if let Some(logo) = &self.logo {
                ui.add(egui::Image::new(egui::load::SizedTexture::new(logo.id(), vec2(40.0, 40.0))));
            }
            ui.vertical(|ui| {
                ui.label(RichText::new("FrameEye Helper").size(21.0).strong().color(TEXT));
                ui.label(RichText::new("Steam Frame eye tracking for VRChat").size(13.5).color(MUTED));
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add(egui::Button::new("About").fill(CONTROL)).clicked() {
                    self.about_open = true;
                }
            });
        });
        ui.add_space(8.0);
        self.stepper(ui);
        ui.add_space(8.0);
    }

    fn stepper(&mut self, ui: &mut Ui) {
        let steps = [
            (Page::Prepare, "Prepare headset"),
            (Page::Connect, "Connect"),
            (Page::Install, "Install"),
            (Page::Check, "Check it works"),
        ];
        let mut clicked = None;
        ui.horizontal(|ui| {
            for (index, (page, label)) in steps.iter().enumerate() {
                if index > 0 {
                    let (rect, _) = ui.allocate_exact_size(vec2(26.0, 28.0), Sense::hover());
                    ui.painter().line_segment(
                        [pos2(rect.left(), rect.center().y), pos2(rect.right(), rect.center().y)],
                        Stroke::new(1.5, DIVIDER),
                    );
                }
                let current = *page == self.page;
                let done = *page < self.page || (*page == Page::Install && self.installed() != Installed::No && self.page != Page::Install);
                let reachable = self.can_visit(*page) && !current;
                let label_color = if current { TEXT } else if reachable || done { MUTED } else { MUTED.gamma_multiply(0.6) };
                let galley = ui.painter().layout_no_wrap(label.to_string(), FontId::proportional(14.5), label_color);
                let size = vec2(28.0 + 8.0 + galley.size().x, 28.0);
                let (rect, response) = ui.allocate_exact_size(size, if reachable { Sense::click() } else { Sense::hover() });
                let center = pos2(rect.left() + 14.0, rect.center().y);
                let (fill, number_color) = if current {
                    (ACCENT, ON_ACCENT)
                } else if done {
                    (GREEN.gamma_multiply(0.25), GREEN)
                } else {
                    (CONTROL, MUTED)
                };
                ui.painter().circle_filled(center, 13.0, fill);
                if response.hovered() {
                    ui.painter().circle_stroke(center, 13.0, Stroke::new(1.5, ACCENT));
                }
                let mark = if done && !current { "✔".to_string() } else { (index + 1).to_string() };
                ui.painter().text(center, egui::Align2::CENTER_CENTER, mark, FontId::proportional(14.0), number_color);
                ui.painter().galley(pos2(rect.left() + 36.0, rect.center().y - galley.size().y / 2.0), galley, label_color);
                if response.clicked() {
                    clicked = Some(*page);
                }
                if reachable {
                    response.on_hover_cursor(egui::CursorIcon::PointingHand);
                }
            }
        });
        if let Some(page) = clicked {
            self.go(page);
        }
    }

    fn footer(&mut self, ui: &mut Ui) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            match &self.session {
                Some(session) if session.demo => {
                    ui.label(RichText::new("●").color(YELLOW));
                    ui.label(RichText::new("Demo mode: a pretend headset, nothing is really installed").color(MUTED));
                }
                Some(session) => {
                    ui.label(RichText::new("●").color(GREEN));
                    ui.label(RichText::new(format!("Connected to {}", session.info.host)).color(MUTED));
                }
                None => {
                    ui.label(RichText::new("●").color(MUTED));
                    ui.label(RichText::new("Not connected to a headset").color(MUTED));
                }
            }
            if self.session.is_some() && ui.add(egui::Button::new("Disconnect").fill(CONTROL)).clicked() {
                self.disconnect();
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let next = match self.page {
                    Page::Prepare => Some(Page::Connect),
                    Page::Connect => Some(Page::Install),
                    Page::Install => Some(Page::Check),
                    Page::Check => None,
                };
                if let Some(next) = next {
                    let enabled = self.can_visit(next) && !self.task.running;
                    if ui.add_enabled(enabled, egui::Button::new("Next  →").fill(CONTROL)).clicked() {
                        self.go(next);
                    }
                }
                let back = match self.page {
                    Page::Prepare => None,
                    Page::Connect => Some(Page::Prepare),
                    Page::Install => Some(Page::Connect),
                    Page::Check => Some(Page::Install),
                };
                if let Some(back) = back {
                    if ui.add_enabled(!self.task.running, egui::Button::new("←  Back").fill(CONTROL)).clicked() {
                        self.go(back);
                    }
                }
            });
        });
        ui.add_space(6.0);
    }

    // -----------------------------------------------------------------------------------------
    // Step 1

    fn page_prepare(&mut self, ui: &mut Ui) {
        heading(ui, "Set up eye tracking for VRChat");
        paragraph(
            ui,
            "Your Steam Frame knows where you look and how open your eyes are. This helper puts a small free program, \
             frameeyeosc, onto the headset. It sends that eye data to VRChat on this PC, so your avatar's eyes move \
             with yours and blink when you blink.",
        );
        ui.add_space(4.0);
        card(ui, |ui| {
            flow_diagram(ui);
        });
        ui.add_space(10.0);
        card(ui, |ui| {
            card_title(ui, "Do this once, on the headset");
            numbered(ui, 1, "Put the headset on and open Settings → System. Turn on Developer Mode.");
            numbered(
                ui,
                2,
                "A Developer section appears. Set a password there (and if there's an SSH switch, turn it on). \
                 Remember the password: you'll type it here in the next step.",
            );
            numbered(ui, 3, "Keep the headset on the same Wi-Fi or router as this PC, and switched on.");
        });
        ui.add_space(10.0);
        card(ui, |ui| {
            card_title(ui, "Good to know");
            bullet(ui, "You only need this helper once. After that, eye tracking starts by itself whenever SteamVR runs on the headset.");
            bullet(ui, "Nothing gets installed on this PC, and it doesn't need admin rights.");
            bullet(ui, "Later updates can be installed from the Eye panel inside the headset.");
        });
        ui.add_space(16.0);
        ui.horizontal(|ui| {
            if primary_button(ui, "My headset is ready  →", true).clicked() {
                self.go(Page::Connect);
            }
            ui.add_space(12.0);
            if ui.link(RichText::new("Just looking? Try the demo (no headset needed)").color(BLUE)).clicked() {
                self.start_demo();
            }
        });
        if let Some(script) = panel_preview_script() {
            ui.add_space(16.0);
            card(ui, |ui| {
                card_title(ui, "Linux only: try the headset's Eye panel on this PC");
                small(
                    ui,
                    "Opens the real Eye panel in a window, fed by a pretend Steam Frame, so you can see and click through what appears inside the headset. Nothing is sent to VRChat.",
                );
                if ui.button("Open the Eye panel preview").clicked() {
                    self.preview_result = Some(match std::process::Command::new(&script).spawn() {
                        Ok(_) => (true, "Starting the preview…".to_string()),
                        Err(error) => (false, format!("Couldn't start {}: {error}", script.display())),
                    });
                }
                if let Some((ok, message)) = &self.preview_result {
                    ui.label(RichText::new(message).color(if *ok { MUTED } else { RED }));
                }
            });
        }
    }

    // -----------------------------------------------------------------------------------------
    // Step 2

    fn page_connect(&mut self, ui: &mut Ui) {
        heading(ui, "Connect to your headset");
        paragraph(ui, "The helper logs in to the headset over your network with the Developer Mode password.");
        if let Some(reason) = &self.disconnect_reason {
            message_box(ui, YELLOW, "Disconnected", reason, None);
            ui.add_space(8.0);
        }
        let mut do_connect = false;
        let mut do_scan = false;
        card(ui, |ui| {
            ui.label(RichText::new("Headset IP address").strong());
            ui.horizontal(|ui| {
                let edit = ui.add(
                    egui::TextEdit::singleline(&mut self.settings.host)
                        .hint_text("for example 192.168.1.42")
                        .desired_width(230.0)
                        .font(FontId::proportional(17.0)),
                );
                if edit.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                    do_connect = true;
                }
                let scanning = matches!(self.scan, ScanState::Running(_));
                if ui.add_enabled(!scanning && !self.connecting, egui::Button::new("🔍  Find my headset").fill(CONTROL)).clicked() {
                    do_scan = true;
                }
                if let ScanState::Running(fraction) = self.scan {
                    ui.add(egui::Spinner::new());
                    ui.label(RichText::new(format!("Looking… {:.0}%", fraction * 100.0)).color(MUTED));
                }
            });
            small(ui, "Where to find it: on the headset, open Settings → Wi-Fi (or Internet) and pick your network. It looks like 192.168.1.42.");
            let mut pick = None;
            if let ScanState::Done { hits, networks } = &self.scan {
                ui.add_space(4.0);
                if *networks == 0 {
                    small(ui, "This PC doesn't seem to be on a home network, so nothing could be searched. Type the address instead.");
                } else if hits.is_empty() {
                    ui.label(RichText::new("No headset found on your network. Check that it's on and awake, or type its address.").color(YELLOW));
                } else {
                    ui.label(RichText::new("Found these devices that accept a login. Pick your headset:").color(MUTED));
                    for hit in hits {
                        let text = format!("{}    {}", hit.ip, hit.banner.trim_start_matches("SSH-2.0-"));
                        let selected = self.settings.host == hit.ip.to_string();
                        if ui.add(egui::Button::selectable(selected, RichText::new(text).monospace())).clicked() {
                            pick = Some(hit.ip.to_string());
                        }
                    }
                }
            }
            if let Some(ip) = pick {
                self.settings.host = ip;
            }
            ui.add_space(8.0);
            ui.label(RichText::new("Developer Mode password").strong());
            ui.horizontal(|ui| {
                let edit = ui.add(
                    egui::TextEdit::singleline(&mut self.password)
                        .password(!self.show_password)
                        .hint_text("the password you set on the headset")
                        .desired_width(230.0)
                        .font(FontId::proportional(17.0)),
                );
                if edit.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                    do_connect = true;
                }
                ui.checkbox(&mut self.show_password, "Show");
            });
            small(ui, "The password is only used to log in. It is never saved.");
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if primary_button(ui, "Connect", !self.connecting).clicked() {
                    do_connect = true;
                }
                if self.connecting {
                    ui.add(egui::Spinner::new());
                    ui.label(RichText::new("Connecting…").color(MUTED));
                }
            });
            if let Some((problem, detail)) = &self.connect_error {
                if *problem != Problem::HostKeyChanged {
                    ui.add_space(8.0);
                    let (title, hint) = explain(*problem, &self.settings.host);
                    message_box(ui, RED, title, hint, (!detail.is_empty()).then_some(detail.as_str()));
                }
            }
            ui.add_space(6.0);
            egui::CollapsingHeader::new(RichText::new("Advanced").color(MUTED)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("User name");
                    ui.add(egui::TextEdit::singleline(&mut self.settings.user).desired_width(120.0));
                    ui.label("SSH port");
                    let mut port = self.settings.port.to_string();
                    if ui.add(egui::TextEdit::singleline(&mut port).desired_width(60.0)).changed() {
                        if let Ok(value) = port.trim().parse() {
                            self.settings.port = value;
                        }
                    }
                });
                small(ui, "The Steam Frame's user is \"steamos\" and SSH uses port 22. Only change these if you know you need to.");
                if !self.settings.known_hosts.is_empty() && ui.button("Forget remembered headsets").clicked() {
                    self.settings.known_hosts.clear();
                    self.save_settings();
                }
            });
        });
        if do_scan {
            self.scan = ScanState::Running(0.0);
            self.worker.send(Cmd::Scan { port: self.settings.port });
        }
        if do_connect && !self.connecting {
            self.connect();
        }
    }

    // -----------------------------------------------------------------------------------------
    // Step 3

    fn page_install(&mut self, ui: &mut Ui) {
        heading(ui, "Install eye tracking on the headset");
        let Some(session) = &self.session else {
            paragraph(ui, "Connect to the headset first.");
            return;
        };
        let info = session.info.clone();
        let installed = info.installed();
        let mut refresh = false;
        card(ui, |ui| {
            ui.horizontal(|ui| {
                card_title(ui, "Your headset");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add_enabled(!self.task.running, egui::Button::new("⟳  Refresh").fill(CONTROL)).clicked() {
                        refresh = true;
                    }
                });
            });
            check_row(ui, Mark::Ok, &format!("Connected to {} as {}", info.host, info.user), (!info.os.is_empty()).then_some(info.os.as_str()));
            if info.is_frame() {
                check_row(ui, Mark::Ok, "It's a Steam Frame (ARM64)", None);
            } else {
                check_row(
                    ui,
                    Mark::Bad,
                    &format!("This device is {}, not a Steam Frame", if info.arch.is_empty() { "unknown" } else { &info.arch }),
                    Some("frameeyeosc only runs on the Steam Frame itself. Check that you connected to the headset's address."),
                );
            }
            if info.eye_server {
                check_row(ui, Mark::Ok, "The eye tracker is running", None);
            } else {
                check_row(ui, Mark::Info, "The eye tracker isn't running right now", Some("It starts with SteamVR. That's fine: frameeyeosc waits for it."));
            }
            match &installed {
                Installed::No => check_row(ui, Mark::Info, "frameeyeosc isn't installed yet", None),
                Installed::Version(version) => {
                    let running = if info.service_active == "active" { " and running" } else { "" };
                    match bundle::compare_versions(version, bundle::VERSION) {
                        std::cmp::Ordering::Less => check_row(
                            ui,
                            Mark::Warn,
                            &format!("frameeyeosc {version} is installed{running}"),
                            Some(&format!("Version {} is newer. Updating is recommended.", bundle::VERSION)),
                        ),
                        _ => check_row(ui, Mark::Ok, &format!("frameeyeosc {version} is installed{running}"), None),
                    }
                }
                Installed::Unknown => check_row(ui, Mark::Warn, "frameeyeosc is installed (unknown version)", None),
            }
            if info.panel_installed {
                check_row(ui, Mark::Ok, "The Eye panel is installed", None);
            }
            if !info.missing_tools.trim().is_empty() {
                check_row(ui, Mark::Bad, &format!("The headset is missing: {}", info.missing_tools.trim()), None);
            }
        });
        ui.add_space(10.0);

        let newer_installed = matches!(&installed, Installed::Version(version) if bundle::compare_versions(version, bundle::VERSION).is_gt());
        let same_installed = matches!(&installed, Installed::Version(version) if bundle::compare_versions(version, bundle::VERSION).is_eq());
        let can_run = info.is_frame() && !self.task.running && info.missing_tools.trim().is_empty();
        let mut start: Option<Cmd> = None;
        card(ui, |ui| {
            if newer_installed {
                card_title(ui, "Nothing to do here");
                paragraph(ui, "The headset already has a newer version than the one inside this helper. Go on to the next step.");
            } else {
                ui.checkbox(
                    &mut self.settings.with_panel,
                    "Also install the Eye panel for the SteamVR dashboard (recommended: it lets you fine-tune from inside VR)",
                );
                ui.add_space(8.0);
                let label = match &installed {
                    Installed::No => "Install eye tracking".to_string(),
                    Installed::Version(_) if same_installed => format!("Reinstall {}", bundle::VERSION),
                    _ => format!("Update to {}", bundle::VERSION),
                };
                ui.horizontal(|ui| {
                    let clicked = if same_installed {
                        ui.add_enabled(can_run, egui::Button::new(label).fill(CONTROL)).clicked()
                    } else {
                        primary_button(ui, &label, can_run).clicked()
                    };
                    if clicked {
                        start = Some(Cmd::Install { with_panel: self.settings.with_panel });
                    }
                    if same_installed && self.task.result.is_none() {
                        ui.label(RichText::new("Already installed, so you can skip this.").color(GREEN));
                    }
                });
                if same_installed && self.settings.with_panel && !info.panel_installed && self.task.result.is_none() {
                    small(ui, "The Eye panel isn't installed yet. Reinstall with the box ticked to add it.");
                }
            }

            if self.task.kind.is_some() || self.task.running {
                ui.add_space(10.0);
                ui.separator();
                let step = self.task.step;
                let done = |this: TaskStep| step.is_some_and(|step| step > this) || self.task.result.as_ref().is_some_and(|(ok, _)| *ok);
                let active = |this: TaskStep| self.task.running && step == Some(this);
                if self.task.kind != Some(TaskKind::Uninstall) || self.task.running {
                    progress_row(ui, done(TaskStep::Uploading), active(TaskStep::Uploading), "Sending the files to the headset");
                    if active(TaskStep::Uploading) {
                        ui.add(egui::ProgressBar::new(self.task.upload).desired_width(320.0).fill(ACCENT));
                    }
                    progress_row(ui, done(TaskStep::Verifying), active(TaskStep::Verifying), "Checking they arrived intact");
                    let running_label = if self.task.kind == Some(TaskKind::Uninstall) { "Removing" } else { "Installing and starting" };
                    progress_row(ui, done(TaskStep::Running), active(TaskStep::Running), running_label);
                }
                if let Some((ok, message)) = &self.task.result {
                    ui.add_space(6.0);
                    match (ok, self.task.kind) {
                        (true, Some(TaskKind::Install)) => {
                            message_box(
                                ui,
                                GREEN,
                                "Installed!",
                                "Eye tracking now starts by itself whenever SteamVR runs on the headset. Next, check that it works.",
                                None,
                            );
                            ui.add_space(6.0);
                            if primary_button(ui, "Next: check it works  →", true).clicked() {
                                self.page = Page::Check;
                                self.furthest = Page::Check;
                            }
                        }
                        (true, _) => message_box(ui, GREEN, "Removed from the headset", message, None),
                        (false, _) => message_box(ui, RED, "That didn't work", message, None),
                    }
                    if std::mem::take(&mut self.reveal_result) {
                        ui.scroll_to_cursor(Some(Align::Center));
                    }
                }
                if !self.task.lines.is_empty() {
                    ui.add_space(4.0);
                    let header = egui::CollapsingHeader::new(RichText::new("Details").color(MUTED))
                        .open(if self.show_details { Some(true) } else { None })
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical().max_height(180.0).stick_to_bottom(true).show(ui, |ui| {
                                for line in &self.task.lines {
                                    ui.label(RichText::new(line).monospace().size(12.5).color(MUTED));
                                }
                            });
                        });
                    if header.header_response.clicked() {
                        self.show_details = false;
                    }
                }
            }
        });
        if !matches!(installed, Installed::No) && !self.task.running {
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new(RichText::new("Uninstall…").color(MUTED)).fill(CONTROL)).clicked() {
                    self.confirm_uninstall = true;
                    self.purge = false;
                }
            });
        }
        if refresh {
            self.worker.send(Cmd::RefreshInfo);
        }
        if let Some(cmd) = start {
            self.task = Task {
                kind: Some(TaskKind::Install),
                running: true,
                ..Task::default()
            };
            self.show_details = false;
            self.save_settings();
            self.worker.send(cmd);
        }
    }

    // -----------------------------------------------------------------------------------------
    // Step 4

    fn page_check(&mut self, ui: &mut Ui) {
        heading(ui, "Check that it works");
        let Some(session) = &self.session else {
            paragraph(ui, "Connect to the headset first.");
            return;
        };
        let info = session.info.clone();
        let live = self.live.clone().unwrap_or_default();
        let health = live.health();
        let status = live.fresh_status().cloned();
        let mut action: Option<Cmd> = None;

        card(ui, |ui| {
            ui.horizontal(|ui| {
                card_title(ui, "Live from the headset");
                if !live.service.is_empty() {
                    ui.label(RichText::new("updating 5× a second").size(12.0).color(MUTED));
                }
            });
            let (color, title, hint) = match health {
                Health::Unknown => (MUTED, "Asking the headset…".to_string(), String::new()),
                Health::NotRunning if info.installed() == Installed::No => (
                    RED,
                    "frameeyeosc isn't installed".into(),
                    "Go back to the Install step.".into(),
                ),
                Health::NotRunning if live.steamvr != "active" && !live.eye_server => (
                    YELLOW,
                    "Waiting for SteamVR on the headset".into(),
                    "frameeyeosc starts together with SteamVR. Put the headset on and start playing through Steam Link from this PC."
                        .into(),
                ),
                Health::NotRunning => (
                    RED,
                    "frameeyeosc isn't running".into(),
                    if live.service == "failed" {
                        "It stopped with an error. \"Show headset log\" below says why; \"Start it\" tries again.".into()
                    } else {
                        "Press \"Start it\". If it stops again, the headset log below says why.".into()
                    },
                ),
                Health::NoEyeData => (
                    YELLOW,
                    "Running, waiting for eye data".into(),
                    if live.eye_server {
                        "Put the headset on. The Frame only tracks your eyes while you're wearing it.".into()
                    } else {
                        "The eye tracker isn't running. It starts with SteamVR on the headset.".into()
                    },
                ),
                Health::NoTarget => (
                    YELLOW,
                    "Tracking, but it doesn't know where to send yet".into(),
                    "It sends to the PC that Steam Link is streaming from. Start streaming from this PC, or send to this PC directly."
                        .into(),
                ),
                Health::Paused => (YELLOW, "Sending is paused".into(), "Someone switched sending off in the Eye panel.".into()),
                Health::Sending => (GREEN, "Sending your eye tracking to VRChat".into(), String::new()),
            };
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(vec2(16.0, 24.0), Sense::hover());
                ui.painter().circle_filled(rect.center(), 7.0, color);
                if health == Health::Unknown {
                    ui.add(egui::Spinner::new());
                }
                ui.label(RichText::new(&title).size(19.0).strong().color(TEXT));
            });
            if !hint.is_empty() {
                paragraph(ui, &hint);
            }

            // What frameeyeosc sends, and to where
            let this_pc_ip = info.client_ip.map(|ip| ip.to_string());
            if let Some(status) = &status {
                ui.add_space(4.0);
                egui::Grid::new("live-grid").num_columns(2).spacing([18.0, 6.0]).show(ui, |ui| {
                    ui.label(RichText::new("Sending to").color(MUTED));
                    ui.horizontal(|ui| match &status.target {
                        Some(target) => {
                            let kind = if status.output == "etvr" { "VRCFaceTracking (ETVR)" } else { "VRChat" };
                            ui.label(RichText::new(format!("{target}  ({kind})")).monospace());
                            match net::is_this_pc(target, info.client_ip, &self.local_ips) {
                                Some(true) => badge(ui, GREEN, "this PC ✔"),
                                Some(false) => badge(ui, YELLOW, "not this PC"),
                                None => {}
                            }
                        }
                        None => {
                            ui.label(RichText::new("nobody yet").color(MUTED));
                        }
                    });
                    ui.end_row();
                    ui.label(RichText::new("Messages").color(MUTED));
                    ui.label(format!("{:.0} per second", status.rate));
                    ui.end_row();
                    ui.label(RichText::new("Eye tracker").color(MUTED));
                    ui.horizontal(|ui| match status.tracker_rate {
                        Some(rate) => {
                            ui.label(format!("{rate:.0} samples per second"));
                            if rate < 60.0 {
                                badge(ui, RED, "low");
                            }
                        }
                        None => {
                            ui.label(RichText::new("no samples").color(MUTED));
                        }
                    });
                    ui.end_row();
                    if let Some(error) = &status.config_error {
                        ui.label(RichText::new("Settings").color(MUTED));
                        ui.label(RichText::new(error).color(RED));
                        ui.end_row();
                    }
                });
            }

            // Buttons that fix what's wrong
            let can_start = health == Health::NotRunning && info.installed() != Installed::No;
            let paused = health == Health::Paused;
            let not_here = status
                .as_ref()
                .and_then(|status| status.target.as_deref())
                .and_then(|target| net::is_this_pc(target, info.client_ip, &self.local_ips))
                == Some(false);
            let redirect = this_pc_ip.clone().filter(|_| health == Health::NoTarget || not_here);
            let fixed = status.as_ref().is_some_and(|status| status.target_mode == "fixed");
            if can_start || paused || redirect.is_some() || fixed {
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    if can_start && ui.button("Start it").clicked() {
                        action = Some(Cmd::RestartService);
                    }
                    if paused && ui.button("Resume sending").clicked() {
                        action = Some(Cmd::SetConfig {
                            changes: vec![("sending".into(), serde_json::Value::Bool(true))],
                            label: "Resume sending".into(),
                        });
                    }
                    if let Some(ip) = &redirect {
                        if ui.button(format!("Send to this PC ({ip}) instead")).clicked() {
                            action = Some(Cmd::SetConfig {
                                changes: vec![("host".into(), serde_json::Value::String(ip.clone()))],
                                label: "Send to this PC".into(),
                            });
                        }
                    }
                    if fixed && ui.button("Back to automatic (Steam Link PC)").clicked() {
                        action = Some(Cmd::SetConfig {
                            changes: vec![("host".into(), serde_json::Value::String("auto".into()))],
                            label: "Automatic target".into(),
                        });
                    }
                });
            }
            if let Some(problem) = &self.status_problem {
                small(ui, &format!("(Reading the status failed, retrying: {problem})"));
            }

            // The eyes, as sent
            ui.add_space(6.0);
            let target = status.as_ref().and_then(|status| status.sent.clone()).map(|sent| EyeAnim {
                lids: sent.lids_vrcft,
                gaze: if status.as_ref().and_then(|s| s.effective.as_ref()).is_some_and(|e| e.independent_eyes) {
                    [sent.gaze_left, sent.gaze_right]
                } else {
                    [sent.gaze, sent.gaze]
                },
            });
            let live_eyes = health == Health::Sending || health == Health::Paused;
            let dt = ui.input(|input| input.stable_dt).min(0.1);
            let shown = match (target, live_eyes) {
                (Some(target), true) => {
                    let mut anim = self.anim.unwrap_or(target);
                    let k = 1.0 - (-dt * 14.0).exp();
                    for eye in 0..2 {
                        anim.lids[eye] += (target.lids[eye] - anim.lids[eye]) * k;
                        for axis in 0..2 {
                            anim.gaze[eye][axis] += (target.gaze[eye][axis] - anim.gaze[eye][axis]) * k;
                        }
                    }
                    self.anim = Some(anim);
                    ui.ctx().request_repaint_after(Duration::from_millis(16));
                    Some(anim)
                }
                _ => {
                    self.anim = None;
                    None
                }
            };
            eyes(ui, shown);
            ui.label(
                RichText::new("Your avatar's eyes, as they are being sent (seen from you: left eye on the left)")
                    .size(12.5)
                    .color(MUTED),
            );
        });

        if let Some(notice) = &self.notice {
            ui.add_space(8.0);
            message_box(ui, if notice.ok { GREEN } else { RED }, if notice.ok { "Done" } else { "Didn't work" }, &notice.text, None);
        }

        ui.add_space(10.0);
        card(ui, |ui| {
            card_title(ui, "On this PC");
            checklist_item(
                ui,
                &mut self.settings.check_steamlink_osc_off,
                "Turn off Steam Link's own OSC",
                "In SteamVR open Settings → Steam Link and switch OSC off. Otherwise Steam Link sends its own, jittery eye data and the two fight over your avatar's eyes.",
            );
            checklist_item(
                ui,
                &mut self.settings.check_vrchat_osc_on,
                "Turn on OSC in VRChat",
                "In VRChat open the Action Menu → Options → OSC and make sure it's Enabled.",
            );
            checklist_item(
                ui,
                &mut self.settings.check_avatar,
                "Wear an avatar with eye tracking",
                "The avatar needs VRCFaceTracking eye parameters (FT/v2/EyeLeftX, EyeLidLeft, …). Many face-tracking avatars have them. If the eyes don't move, try another avatar.",
            );
            ui.add_space(6.0);
            ui.separator();
            ui.label(RichText::new("Avatar's eyes still not moving?").strong());
            if cfg!(windows) {
                small(
                    ui,
                    "Windows Firewall may be dropping the eye data. This adds a narrow rule: only VRChat, only UDP port 9000, only from your local network. Windows asks for permission first.",
                );
                if ui.button("🛡  Allow VRChat through Windows Firewall").clicked() {
                    self.firewall_result = Some(match firewall::allow_vrchat_osc() {
                        Ok(message) => (true, message),
                        Err(message) => (false, message),
                    });
                }
            } else {
                small(ui, "On Windows, a button here fixes the Windows Firewall rule for VRChat. (Not needed on Linux.)");
            }
            if let Some((ok, message)) = &self.firewall_result {
                ui.label(RichText::new(message).color(if *ok { GREEN } else { RED }));
            }
        });

        ui.add_space(10.0);
        card(ui, |ui| {
            card_title(ui, "Inside the headset");
            paragraph(
                ui,
                "Open the SteamVR dashboard and choose Eye. On the Eye fit tab, press Fit my eyes, close the dashboard and follow the dot with your eyes (about 20 seconds). This lines your avatar's eyes up with yours.",
            );
            if info.panel_installed || session.demo {
                if ui.button("Open the Eye panel in the headset").clicked() {
                    action = Some(Cmd::ShowPanel);
                }
            }
        });

        ui.add_space(10.0);
        let header = egui::CollapsingHeader::new(RichText::new("Show headset log").color(MUTED)).show(ui, |ui| {
            if ui.button("Refresh").clicked() {
                action = Some(Cmd::FetchLogs);
            }
            match &self.logs {
                Some(text) => {
                    egui::ScrollArea::vertical().max_height(240.0).stick_to_bottom(true).show(ui, |ui| {
                        ui.add(egui::Label::new(RichText::new(text).monospace().size(12.0).color(MUTED)).wrap());
                    });
                }
                None => {
                    ui.add(egui::Spinner::new());
                }
            }
        });
        if header.header_response.clicked() && self.logs.is_none() {
            action = Some(Cmd::FetchLogs);
        }

        if let Some(cmd) = action {
            self.notice = None;
            self.worker.send(cmd);
        }
    }

    // -----------------------------------------------------------------------------------------
    // Dialogs

    fn modals(&mut self, ctx: &egui::Context) {
        if let Some(fingerprint) = self.host_key_prompt.clone() {
            let mut close = false;
            let mut trust = false;
            egui::Modal::new(egui::Id::new("host-key")).show(ctx, |ui| {
                ui.set_max_width(460.0);
                ui.label(RichText::new("This headset introduced itself differently").size(18.0).strong());
                ui.add_space(6.0);
                paragraph(
                    ui,
                    &format!(
                        "The device at {} has a different identity than last time. That's normal after a factory reset or a SteamOS reinstall, or if a different device now has this address. If you just reset your headset, trust it; otherwise cancel.",
                        self.settings.host
                    ),
                );
                small(ui, &format!("New identity: {fingerprint}"));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if primary_button(ui, "Trust it and connect", true).clicked() {
                        trust = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });
            if trust {
                self.settings.known_hosts.remove(&Settings::host_key_id(&self.settings.host, self.settings.port));
                self.save_settings();
                self.host_key_prompt = None;
                self.connect();
            } else if close {
                self.host_key_prompt = None;
            }
        }

        if self.confirm_uninstall {
            let mut close = false;
            let mut go = false;
            egui::Modal::new(egui::Id::new("uninstall")).show(ctx, |ui| {
                ui.set_max_width(420.0);
                ui.label(RichText::new("Remove eye tracking from the headset?").size(18.0).strong());
                ui.add_space(6.0);
                paragraph(ui, "This removes frameeyeosc and the Eye panel. You can install them again at any time.");
                ui.checkbox(&mut self.purge, "Also delete its settings and learned eyelid calibration");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.add(egui::Button::new(RichText::new("Remove").color(TEXT)).fill(RED.gamma_multiply(0.55))).clicked() {
                        go = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });
            if go {
                self.confirm_uninstall = false;
                self.task = Task {
                    kind: Some(TaskKind::Uninstall),
                    running: true,
                    ..Task::default()
                };
                self.worker.send(Cmd::Uninstall { purge: self.purge });
            } else if close {
                self.confirm_uninstall = false;
            }
        }

        if self.about_open {
            let response = egui::Modal::new(egui::Id::new("about")).show(ctx, |ui| {
                ui.set_max_width(520.0);
                ui.label(RichText::new("About FrameEye Helper").size(18.0).strong());
                ui.add_space(4.0);
                paragraph(
                    ui,
                    &format!(
                        "FrameEye Helper {} installs frameeyeosc {} on a Steam Frame. frameeyeosc was made by konsti219, who found where the Frame keeps its eye data, and extended by sasaken1102r (smoothing, eyelid calibration, the Eye panel, the eye fit). This helper only copies their release to the headset and runs its own install.sh.",
                        env!("CARGO_PKG_VERSION"),
                        bundle::VERSION
                    ),
                );
                ui.hyperlink_to("FrameEye Helper on GitHub (updates, questions)", bundle::PROJECT_URL);
                ui.hyperlink_to("frameeyeosc (sasaken1102r's version, the one installed)", bundle::UPSTREAM_URL);
                ui.hyperlink_to("frameeyeosc (konsti219's original)", bundle::ORIGINAL_URL);
                ui.add_space(4.0);
                small(
                    ui,
                    "Unofficial: not made or endorsed by Valve, VRChat, VRCFaceTracking or the frameeyeosc authors. frameeyeosc reads the Frame's private eye-tracking data, which a SteamOS update may change; if it stops working, update it from the Eye panel.",
                );
                egui::CollapsingHeader::new("frameeyeosc license (MIT)").show(ui, |ui| {
                    egui::ScrollArea::vertical().max_height(160.0).show(ui, |ui| {
                        ui.label(RichText::new(bundle::LICENSE).monospace().size(11.5).color(MUTED));
                    });
                });
                small(ui, "Built with egui, russh and tokio (MIT / Apache-2.0).");
                ui.add_space(6.0);
                if ui.button("Close").clicked() {
                    ui.close();
                }
            });
            if response.should_close() {
                self.about_open = false;
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // --screenshots: walk through the demo and save each page (used for the tutorial and as a smoke test)

    fn drive_shooter(&mut self, ctx: &egui::Context) {
        let Some(mut shooter) = self.shooter.take() else { return };
        self.shoot_step(ctx, &mut shooter);
        self.shooter = Some(shooter);
    }

    fn shoot_step(&mut self, ctx: &egui::Context, shooter: &mut Shooter) {
        ctx.request_repaint_after(Duration::from_millis(50));
        let now = ctx.input(|input| input.time);
        // Save any screenshot that came back
        let shots: Vec<(String, std::sync::Arc<egui::ColorImage>)> = ctx.input(|input| {
            input
                .raw
                .events
                .iter()
                .filter_map(|event| match event {
                    egui::Event::Screenshot { image, user_data, .. } => user_data
                        .data
                        .as_ref()
                        .and_then(|data| data.downcast_ref::<String>())
                        .map(|name| (name.clone(), image.clone())),
                    _ => None,
                })
                .collect()
        });
        for (name, image) in shots {
            let bytes: Vec<u8> = image.pixels.iter().flat_map(|pixel| pixel.to_array()).collect();
            let path = shooter.dir.join(format!("{name}.png"));
            match image::RgbaImage::from_raw(image.size[0] as u32, image.size[1] as u32, bytes) {
                Some(buffer) => match buffer.save(&path) {
                    Ok(()) => println!("saved {}", path.display()),
                    Err(error) => eprintln!("could not save {}: {error}", path.display()),
                },
                None => eprintln!("screenshot {name} had an unexpected size"),
            }
            shooter.pending = false;
            shooter.step += 1;
            shooter.wait_until = now + 0.4;
        }
        if shooter.pending || now < shooter.wait_until {
            return;
        }
        let shoot = |name: &str| {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(name.to_string())));
        };
        match shooter.step {
            0 => {
                shooter.pending = true;
                shoot("1-prepare");
            }
            1 => {
                if self.page != Page::Connect {
                    self.page = Page::Connect;
                    self.settings.host = "192.168.1.42".into();
                    self.password = "hunter22".into();
                    self.scan = ScanState::Done {
                        hits: vec![
                            ScanHit { ip: "192.168.1.42".parse().unwrap(), banner: "SSH-2.0-OpenSSH_9.9".into() },
                            ScanHit { ip: "192.168.1.60".parse().unwrap(), banner: "SSH-2.0-dropbear_2024.86".into() },
                        ],
                        networks: 1,
                    };
                    shooter.wait_until = now + 0.8;
                } else {
                    shooter.pending = true;
                    shoot("2-connect");
                }
            }
            2 => {
                if self.session.is_none() {
                    if !self.connecting {
                        self.scan = ScanState::Idle;
                        self.connecting = true;
                        self.worker.send(Cmd::ConnectDemo);
                    }
                } else {
                    shooter.pending = true;
                    shoot("3-install");
                }
            }
            3 => {
                if self.task.kind.is_none() {
                    self.task = Task { kind: Some(TaskKind::Install), running: true, ..Task::default() };
                    self.worker.send(Cmd::Install { with_panel: true });
                } else if self.task.result.is_some() && !self.task.lines.is_empty() {
                    // one more round so the result has scrolled into view
                    self.task.lines.clear();
                    shooter.wait_until = now + 1.0;
                } else if self.task.result.is_some() {
                    shooter.pending = true;
                    shoot("4-installed");
                }
            }
            4 => {
                if self.page != Page::Check {
                    self.go(Page::Check);
                    self.settings.check_steamlink_osc_off = true;
                    self.settings.check_vrchat_osc_on = true;
                    shooter.wait_until = now + 3.0;
                } else {
                    shooter.pending = true;
                    shoot("5-check");
                }
            }
            _ => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.handle_events();
        self.sync_status_stream();
        let ctx = ui.ctx().clone();
        self.drive_shooter(&ctx);

        let edge = egui::Frame::new().fill(BG).inner_margin(Margin::symmetric(20, 0));
        egui::Panel::top("header").frame(edge).show(ui, |ui| self.header(ui));
        egui::Panel::bottom("footer").frame(edge.fill(CARD)).show(ui, |ui| self.footer(ui));
        egui::CentralPanel::default().frame(egui::Frame::new().fill(BG)).show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt(("page", self.page as u8)).auto_shrink([false, false]).show(ui, |ui| {
                let side = ((ui.available_width() - CONTENT_WIDTH) / 2.0).max(20.0);
                ui.horizontal(|ui| {
                    ui.add_space(side);
                    ui.vertical(|ui| {
                        ui.set_max_width(CONTENT_WIDTH.min(ui.available_width() - 20.0));
                        ui.add_space(12.0);
                        match self.page {
                            Page::Prepare => self.page_prepare(ui),
                            Page::Connect => self.page_connect(ui),
                            Page::Install => self.page_install(ui),
                            Page::Check => self.page_check(ui),
                        }
                        ui.add_space(24.0);
                    });
                });
            });
        });
        self.modals(&ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_settings();
    }
}

// ---------------------------------------------------------------------------------------------
// Look and small widgets

fn setup_style(ctx: &egui::Context) {
    // egui's proportional font has no arrows (→) or geometric shapes (●); its bundled monospace font
    // Hack does, so it goes into the fallback chain ahead of the icon font.
    let mut fonts = egui::FontDefinitions::default();
    if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        let at = family.iter().position(|name| name == "emoji-icon-font").unwrap_or(family.len());
        family.insert(at, "Hack".to_owned());
    }
    ctx.set_fonts(fonts);
    ctx.set_theme(egui::Theme::Dark);
    ctx.all_styles_mut(|style| {
        use egui::TextStyle;
        style.text_styles = [
            (TextStyle::Heading, FontId::proportional(25.0)),
            (TextStyle::Body, FontId::proportional(15.5)),
            (TextStyle::Button, FontId::proportional(15.5)),
            (TextStyle::Small, FontId::proportional(13.0)),
            (TextStyle::Monospace, FontId::monospace(13.5)),
        ]
        .into();
        style.spacing.item_spacing = vec2(8.0, 7.0);
        style.spacing.button_padding = vec2(12.0, 6.0);
        style.spacing.interact_size.y = 30.0;
        let visuals = &mut style.visuals;
        visuals.panel_fill = BG;
        visuals.window_fill = CARD;
        visuals.window_stroke = Stroke::new(1.0, DIVIDER);
        visuals.extreme_bg_color = Color32::from_rgb(0x01, 0x04, 0x09);
        visuals.hyperlink_color = BLUE;
        visuals.selection.bg_fill = ACCENT.gamma_multiply(0.45);
        visuals.selection.stroke = Stroke::new(1.5, ACCENT);
        let radius = CornerRadius::same(6);
        let widgets = &mut visuals.widgets;
        widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        widgets.noninteractive.bg_stroke = Stroke::new(1.0, DIVIDER);
        widgets.noninteractive.corner_radius = radius;
        for (state, fill) in [
            (&mut widgets.inactive, CONTROL),
            (&mut widgets.hovered, CONTROL_HOVER),
            (&mut widgets.active, ACCENT_PRESSED.gamma_multiply(0.6)),
            (&mut widgets.open, CONTROL_HOVER),
        ] {
            state.bg_fill = fill;
            state.weak_bg_fill = fill;
            state.fg_stroke = Stroke::new(1.5, TEXT);
            state.corner_radius = radius;
        }
        widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT.gamma_multiply(0.8));
        widgets.inactive.bg_stroke = Stroke::new(1.0, DIVIDER);
    });
}

fn heading(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).size(25.0).strong().color(TEXT));
    ui.add_space(2.0);
}

fn paragraph(ui: &mut Ui, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).color(MUTED).size(15.0)).wrap());
}

fn small(ui: &mut Ui, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).color(MUTED).size(13.0)).wrap());
}

fn card_title(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).size(16.5).strong().color(TEXT));
}

fn card<R>(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new()
        .fill(CARD)
        .stroke(Stroke::new(1.0, DIVIDER))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add_contents(ui)
        })
        .inner
}

fn primary_button(ui: &mut Ui, text: &str, enabled: bool) -> egui::Response {
    let button = egui::Button::new(RichText::new(text).size(16.5).strong().color(if enabled { ON_ACCENT } else { MUTED }))
        .fill(if enabled { ACCENT } else { CONTROL })
        .corner_radius(CornerRadius::same(8))
        .min_size(vec2(170.0, 40.0));
    ui.add_enabled(enabled, button)
}

fn numbered(ui: &mut Ui, number: usize, text: &str) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(26.0, 24.0), Sense::hover());
        ui.painter().circle_filled(rect.center(), 11.0, ACCENT.gamma_multiply(0.25));
        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, number.to_string(), FontId::proportional(13.5), ACCENT);
        ui.add(egui::Label::new(RichText::new(text).color(TEXT)).wrap());
    });
}

fn bullet(ui: &mut Ui, text: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("•").color(ACCENT));
        ui.add(egui::Label::new(RichText::new(text).color(TEXT)).wrap());
    });
}

#[derive(Clone, Copy)]
enum Mark {
    Ok,
    Info,
    Warn,
    Bad,
}

fn check_row(ui: &mut Ui, mark: Mark, text: &str, detail: Option<&str>) {
    let (symbol, color) = match mark {
        Mark::Ok => ("✔", GREEN),
        Mark::Info => ("ℹ", BLUE),
        Mark::Warn => ("⚠", YELLOW),
        Mark::Bad => ("✖", RED),
    };
    ui.horizontal(|ui| {
        ui.add_sized(vec2(20.0, 22.0), egui::Label::new(RichText::new(symbol).color(color).size(15.0)));
        ui.vertical(|ui| {
            ui.add(egui::Label::new(RichText::new(text).color(TEXT)).wrap());
            if let Some(detail) = detail {
                small(ui, detail);
            }
        });
    });
}

fn progress_row(ui: &mut Ui, done: bool, active: bool, text: &str) {
    ui.horizontal(|ui| {
        if done {
            ui.add_sized(vec2(20.0, 22.0), egui::Label::new(RichText::new("✔").color(GREEN)));
        } else if active {
            ui.add_sized(vec2(20.0, 22.0), egui::Spinner::new());
        } else {
            ui.add_sized(vec2(20.0, 22.0), egui::Label::new(RichText::new("○").color(MUTED)));
        }
        ui.label(RichText::new(text).color(if done || active { TEXT } else { MUTED }));
    });
}

fn message_box(ui: &mut Ui, color: Color32, title: &str, text: &str, detail: Option<&str>) {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.12))
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.7)))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title).strong().color(color));
            ui.add(egui::Label::new(RichText::new(text).color(TEXT)).wrap());
            if let Some(detail) = detail {
                small(ui, detail);
            }
        });
}

fn badge(ui: &mut Ui, color: Color32, text: &str) {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.18))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(8, 1))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(13.0).color(color));
        });
}

fn checklist_item(ui: &mut Ui, checked: &mut bool, title: &str, text: &str) {
    ui.horizontal(|ui| {
        ui.checkbox(checked, "");
        ui.vertical(|ui| {
            ui.label(RichText::new(title).strong().color(if *checked { GREEN } else { TEXT }));
            small(ui, text);
        });
    });
    ui.add_space(2.0);
}

/// The Linux Eye panel preview's launcher, if it sits next to this program (release folder or source tree).
fn panel_preview_script() -> Option<PathBuf> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    [
        dir.join("FrameEye-Panel-Preview").join("run-panel-preview.sh"),
        dir.join("../../../preview/run-panel-preview.sh"),
    ]
    .into_iter()
    .find(|path| path.is_file())
}

/// Friendly wording for why connecting failed.
fn explain(problem: Problem, host: &str) -> (&'static str, &'static str) {
    let _ = host;
    match problem {
        Problem::BadAddress => ("Check the address", "Type the headset's IP address, like 192.168.1.42, or use Find my headset."),
        Problem::Unreachable | Problem::Timeout => (
            "Couldn't reach the headset",
            "Check that the headset is switched on and awake, that it's on the same Wi-Fi or router as this PC, and that the address is right. Find my headset can look for it.",
        ),
        Problem::Refused => (
            "The headset isn't accepting logins yet",
            "On the headset, turn on Developer Mode (and SSH, if there's a switch) and set a password under Developer.",
        ),
        Problem::WrongPassword => (
            "Wrong password",
            "Use the password you set under Developer in the headset's settings. The user name is steamos (see Advanced).",
        ),
        Problem::NoPasswordLogin => (
            "The headset doesn't accept password logins",
            "Set a password under Developer in the headset's settings; that switches password login on.",
        ),
        Problem::Handshake => ("The secure connection failed", "Try again. If it keeps happening, restart the headset."),
        Problem::HostKeyChanged => ("The headset's identity changed", ""),
        Problem::Other => ("Something went wrong", "The details below say what. Try again, or restart the headset."),
    }
}

/// Headset → Wi-Fi → PC, drawn with shapes.
fn flow_diagram(ui: &mut Ui) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, 96.0), Sense::hover());
    let painter = ui.painter_at(rect);
    let box_w = (width - 140.0) / 2.0;
    let left = egui::Rect::from_min_size(rect.left_top() + vec2(0.0, 8.0), vec2(box_w, 80.0));
    let right = egui::Rect::from_min_size(rect.right_top() + vec2(-box_w, 8.0), vec2(box_w, 80.0));
    for (area, title, sub) in [
        (left, "Steam Frame", "frameeyeosc reads your eyes"),
        (right, "This PC", "VRChat moves your avatar's eyes"),
    ] {
        painter.rect_filled(area, CornerRadius::same(10), CONTROL);
        painter.rect_stroke(area, CornerRadius::same(10), Stroke::new(1.0, DIVIDER), egui::StrokeKind::Inside);
        painter.text(area.center() - vec2(0.0, 12.0), egui::Align2::CENTER_CENTER, title, FontId::proportional(17.0), TEXT);
        painter.text(area.center() + vec2(0.0, 14.0), egui::Align2::CENTER_CENTER, sub, FontId::proportional(13.0), MUTED);
    }
    let y = rect.center().y;
    let (start, end) = (pos2(left.right() + 14.0, y), pos2(right.left() - 14.0, y));
    painter.line_segment([start, end], Stroke::new(2.5, ACCENT));
    painter.add(egui::Shape::convex_polygon(
        vec![end, end + vec2(-10.0, -6.0), end + vec2(-10.0, 6.0)],
        ACCENT,
        Stroke::NONE,
    ));
    painter.text(pos2((start.x + end.x) / 2.0, y - 14.0), egui::Align2::CENTER_CENTER, "eye data", FontId::proportional(13.0), ACCENT);
    painter.text(pos2((start.x + end.x) / 2.0, y + 14.0), egui::Align2::CENTER_CENTER, "over Wi-Fi (OSC)", FontId::proportional(12.0), MUTED);
}

/// Two cartoon eyes: `None` draws them resting and grey (no data).
fn eyes(ui: &mut Ui, anim: Option<EyeAnim>) {
    let width = ui.available_width().min(520.0);
    let (rect, _) = ui.allocate_exact_size(vec2(width, 130.0), Sense::hover());
    let painter = ui.painter_at(rect);
    let eye_w = (width * 0.34).min(170.0);
    let eye_h = eye_w * 0.56;
    let live = anim.is_some();
    let anim = anim.unwrap_or(EyeAnim { lids: [0.75; 2], gaze: [[0.0; 2]; 2] });
    for (eye, label) in [(0usize, "left"), (1, "right")] {
        let center = rect.center() + vec2(if eye == 0 { -1.0 } else { 1.0 } * width * 0.22, -8.0);
        draw_eye(&painter, center, eye_w, eye_h, anim.lids[eye], anim.gaze[eye], live);
        let text = if live {
            format!("{label}  open {:.0}%", (anim.lids[eye] / 0.75 * 100.0).clamp(0.0, 133.0))
        } else {
            format!("{label}  (no data)")
        };
        painter.text(center + vec2(0.0, eye_h * 0.5 + 16.0), egui::Align2::CENTER_CENTER, text, FontId::proportional(12.5), MUTED);
    }
}

fn draw_eye(painter: &egui::Painter, center: egui::Pos2, w: f32, h: f32, lid: f32, gaze: [f32; 2], live: bool) {
    use std::f32::consts::PI;
    // Nothing (the iris looking far to one side) may spill out of this eye's own box
    let bounds = egui::Rect::from_center_size(center, vec2(w + 4.0, h * 2.0));
    let painter = &painter.with_clip_rect(bounds.intersect(painter.clip_rect()));
    let open = (lid / 0.75).clamp(0.0, 1.34);
    let half = h / 2.0;
    let lower = |x: f32| center.y + half * 0.62 * (PI * x / w).cos();
    let upper_open = |x: f32| center.y - half * 0.78 * (PI * x / w).cos();
    let upper = |x: f32| lower(x) + (upper_open(x) - lower(x)) * open;
    let slices = 60;
    let xs: Vec<f32> = (0..=slices).map(|i| -w / 2.0 + w * i as f32 / slices as f32).collect();
    let white = if live { Color32::from_rgb(0xf4, 0xf1, 0xf6) } else { Color32::from_gray(0x55) };
    for pair in xs.windows(2) {
        let mid = (pair[0] + pair[1]) / 2.0;
        let (top, bottom) = (upper(mid), lower(mid));
        if bottom > top {
            painter.rect_filled(
                egui::Rect::from_min_max(pos2(center.x + pair[0], top), pos2(center.x + pair[1] + 0.5, bottom)),
                CornerRadius::ZERO,
                white,
            );
        }
    }
    // Iris and pupil: +x is right, +y is up
    let iris = h * 0.34;
    let iris_center = center + vec2(gaze[0].clamp(-1.0, 1.0) * w * 0.34, -gaze[1].clamp(-1.0, 1.0) * h * 0.5);
    painter.circle_filled(iris_center, iris, if live { ACCENT } else { Color32::from_gray(0x77) });
    painter.circle_filled(iris_center, iris * 0.45, Color32::from_rgb(0x12, 0x0a, 0x16));
    painter.circle_filled(iris_center + vec2(iris * 0.3, -iris * 0.32), iris * 0.14, Color32::WHITE);
    // The lids: paint the card colour over everything outside the opening
    let top_edge = center.y - h;
    let bottom_edge = center.y + h;
    for pair in xs.windows(2) {
        let mid = (pair[0] + pair[1]) / 2.0;
        let (x0, x1) = (center.x + pair[0], center.x + pair[1] + 0.5);
        painter.rect_filled(egui::Rect::from_min_max(pos2(x0, top_edge), pos2(x1, upper(mid))), CornerRadius::ZERO, CARD);
        painter.rect_filled(egui::Rect::from_min_max(pos2(x0, lower(mid)), pos2(x1, bottom_edge)), CornerRadius::ZERO, CARD);
    }
    // Outlines: the upper lid a little heavier
    let line = |f: &dyn Fn(f32) -> f32| -> Vec<egui::Pos2> { xs.iter().map(|x| pos2(center.x + x, f(*x))).collect() };
    let outline = if live { TEXT } else { Color32::from_gray(0x77) };
    painter.add(egui::Shape::line(line(&upper), Stroke::new(3.0, outline)));
    painter.add(egui::Shape::line(line(&lower), Stroke::new(1.6, outline)));
}
