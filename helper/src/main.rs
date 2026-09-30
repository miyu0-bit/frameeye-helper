//! FrameEye Helper: installs frameeyeosc (Steam Frame eye tracking → VRChat over OSC) on the headset
//! with a few clicks, and shows whether it's working.

// No console window behind the app on Windows (release builds)
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod bundle;
mod firewall;
mod net;
mod remote;
#[cfg(feature = "selftest")]
mod selftest;
mod settings;
mod status;
mod worker;

/// The Eye icon (the same one the headset's dashboard shows), as RGBA.
pub fn icon_rgba() -> Option<(Vec<u8>, u32, u32)> {
    let image = image::load_from_memory(include_bytes!("../assets/icon-256.png")).ok()?.into_rgba8();
    let (width, height) = image.dimensions();
    Some((image.into_raw(), width, height))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|arg| arg == "--version") {
        println!("FrameEye Helper {} (frameeyeosc {} inside)", env!("CARGO_PKG_VERSION"), bundle::VERSION);
        return;
    }
    #[cfg(feature = "selftest")]
    if args.get(1).map(String::as_str) == Some("--selftest") {
        std::process::exit(selftest::run(&args[2..]));
    }
    let flags = app::Flags::parse(&args);

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("FrameEye Helper")
        .with_app_id("frameeye-helper")
        .with_inner_size([960.0, 760.0])
        .with_min_inner_size([760.0, 560.0]);
    if let Some((rgba, width, height)) = icon_rgba() {
        viewport = viewport.with_icon(egui::IconData { rgba, width, height });
    }
    let options = |renderer| eframe::NativeOptions {
        viewport: viewport.clone(),
        renderer,
        centered: true,
        ..Default::default()
    };
    let run = |renderer| {
        let flags = flags.clone();
        eframe::run_native(
            "FrameEye Helper",
            options(renderer),
            Box::new(move |cc| Ok(Box::new(app::App::new(cc, flags)))),
        )
    };

    // wgpu (DirectX 12 / Vulkan) first, then OpenGL for PCs where that fails; "--opengl" skips wgpu.
    let first = if args.iter().any(|arg| arg == "--opengl") {
        Err(eframe::Error::AppCreation("OpenGL requested".into()))
    } else {
        run(eframe::Renderer::Wgpu)
    };
    if let Err(error) = first {
        eprintln!("Could not start with wgpu ({error}); trying OpenGL");
        if let Err(error) = run(eframe::Renderer::Glow) {
            report_fatal(&format!("FrameEye Helper couldn't open its window.\n\n{error}"));
        }
    }
}

/// Last resort when no window could be opened: a message box on Windows, stderr elsewhere.
fn report_fatal(message: &str) {
    eprintln!("{message}");
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
        let wide = |text: &str| -> Vec<u16> { std::ffi::OsStr::new(text).encode_wide().chain(Some(0)).collect() };
        let (text, title) = (wide(message), wide("FrameEye Helper"));
        unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), MB_OK | MB_ICONERROR) };
    }
}
