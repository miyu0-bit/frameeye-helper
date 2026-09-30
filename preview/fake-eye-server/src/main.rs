//! Pretends to be the Steam Frame's eye tracker, so the real frameeyeosc (and its Eye panel) can run on
//! an ordinary Linux PC. It creates /dev/shm/eye-server.mmap with the private version-4 layout that
//! frameeyeosc reads (see frameeyeosc's src/main.rs) and publishes made-up samples about 120 times a second:
//! eyes that look around, blink and now and then wink.
//!
//! During the Eye panel's eye fit it plays along: it reads the capture requests the panel writes into
//! config.json and looks at each dot (or closes its eyes), like a person following the fit would.
//! Its "sensor" is deliberately a little off (reads low and a bit narrow vertically), so the fit has
//! something to correct.
//!
//!   fake-eye-server [--rate HZ] [--no-bias] [--config PATH]
//! Stop with Ctrl+C; the shared-memory file is removed on exit.

use std::fs::OpenOptions;
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const PATH: &str = "/dev/shm/eye-server.mmap";
const SHM_SIZE: usize = 0x4f21a;
const VERSION: u32 = 4;
const OFF_INITIALIZED: usize = 0x04;
const OFF_MUTEX: usize = 0x08;
const OFF_SEQUENCE: usize = 0x38;
const OFF_REQUESTED: usize = 0x3c;
const OFF_RECORD: usize = 0x152;
// Inside the packed record
const REC_STATE: usize = 0x00;
const REC_FLAG: usize = 0x04;
const REC_TIME: usize = 0x05;
const REC_GAZE: usize = 0x0d;
const REC_GAZE_COV: usize = 0x25;
const REC_FIXATION: usize = 0x3d;
const REC_PRE_GAZE: usize = 0x49;
const REC_PRE_COV: usize = 0x61;
const REC_OPENNESS: usize = 0x79;

const _: () = assert!(std::mem::size_of::<libc::pthread_mutex_t>() <= 0x30);

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

struct Shm {
    base: *mut u8,
}

impl Shm {
    fn create() -> io::Result<Self> {
        let _ = std::fs::remove_file(PATH);
        let file = OpenOptions::new().read(true).write(true).create_new(true).mode(0o666).open(PATH)?;
        file.set_len(SHM_SIZE as u64)?;
        use std::os::fd::AsRawFd;
        let base = unsafe {
            libc::mmap(ptr::null_mut(), SHM_SIZE, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, file.as_raw_fd(), 0)
        };
        if base == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let shm = Shm { base: base.cast() };
        unsafe {
            // A robust, process-shared mutex, like the real eye server's
            let mut attr: libc::pthread_mutexattr_t = std::mem::zeroed();
            libc::pthread_mutexattr_init(&mut attr);
            libc::pthread_mutexattr_setpshared(&mut attr, libc::PTHREAD_PROCESS_SHARED);
            libc::pthread_mutexattr_setrobust(&mut attr, libc::PTHREAD_MUTEX_ROBUST);
            libc::pthread_mutex_init(shm.mutex(), &attr);
            libc::pthread_mutexattr_destroy(&mut attr);
            shm.write_u32(0, VERSION);
            shm.write_u32(OFF_INITIALIZED, 1);
        }
        Ok(shm)
    }

    fn mutex(&self) -> *mut libc::pthread_mutex_t {
        unsafe { self.base.add(OFF_MUTEX).cast() }
    }

    unsafe fn write_u32(&self, offset: usize, value: u32) {
        unsafe { ptr::write_volatile(self.base.add(offset).cast::<u32>(), value) };
    }

    unsafe fn write_bytes(&self, offset: usize, bytes: &[u8]) {
        unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), self.base.add(offset), bytes.len()) };
    }

    fn publish(&self, sample: &Sample) {
        let record = OFF_RECORD;
        let floats = |values: &[f32]| -> Vec<u8> { values.iter().flat_map(|value| value.to_le_bytes()).collect() };
        unsafe {
            let code = libc::pthread_mutex_lock(self.mutex());
            if code == libc::EOWNERDEAD {
                libc::pthread_mutex_consistent(self.mutex());
            }
            self.write_bytes(record + REC_STATE, &1u32.to_le_bytes());
            self.write_bytes(record + REC_FLAG, &[1]);
            self.write_bytes(record + REC_TIME, &sample.time.to_le_bytes());
            let gaze = [sample.left[0], sample.left[1], sample.left[2], sample.right[0], sample.right[1], sample.right[2]];
            self.write_bytes(record + REC_GAZE, &floats(&gaze));
            self.write_bytes(record + REC_GAZE_COV, &floats(&[0.005; 6]));
            self.write_bytes(record + REC_FIXATION, &floats(&sample.fixation));
            self.write_bytes(record + REC_PRE_GAZE, &floats(&gaze));
            self.write_bytes(record + REC_PRE_COV, &floats(&[0.005; 6]));
            self.write_bytes(record + REC_OPENNESS, &floats(&sample.openness));
            let sequence = self.base.add(OFF_SEQUENCE).cast::<u32>();
            ptr::write_volatile(sequence, ptr::read_volatile(sequence).wrapping_add(1));
            self.write_u32(OFF_REQUESTED, 0);
            libc::pthread_mutex_unlock(self.mutex());
            // Shared (not private) futex: frameeyeosc waits on this word from another process
            libc::syscall(libc::SYS_futex, sequence, libc::FUTEX_WAKE, i32::MAX, ptr::null::<libc::timespec>());
        }
    }
}

impl Drop for Shm {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.base.cast(), SHM_SIZE) };
        let _ = std::fs::remove_file(PATH);
    }
}

struct Sample {
    time: f64,
    left: [f32; 3],
    right: [f32; 3],
    fixation: [f32; 3],
    openness: [f32; 2],
}

/// A small deterministic pseudo-random generator (xorshift), so no extra crate is needed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }

    fn noise(&mut self, size: f32) -> f32 {
        (self.next() + self.next() + self.next() - 1.5) * size
    }
}

/// What the pretend person is doing.
#[derive(Clone, Copy, PartialEq)]
enum Look {
    Wander,
    /// Following an eye-fit dot: degrees right, degrees up.
    Target(f32, f32),
    Closed,
}

/// Reads the eye-fit capture the panel asked for (config.json `gaze_capture`) and whether frameeyeosc
/// finished it (status.json).
struct FitWatcher {
    config: Option<PathBuf>,
    status: Option<PathBuf>,
    last_check: Instant,
    look: Look,
    /// Until when to keep holding the look after a capture finished (the dot is still up, gliding on).
    hold_until: Option<Instant>,
    active_id: i64,
}

impl FitWatcher {
    fn new(config: Option<PathBuf>) -> Self {
        let config = config.or_else(|| {
            let base = std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
            Some(base.join("frameeyeosc").join("config.json"))
        });
        let status = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .or_else(|| Some(PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() }))))
            .map(|dir| dir.join("frameeyeosc").join("status.json"));
        Self {
            config,
            status,
            last_check: Instant::now() - Duration::from_secs(1),
            look: Look::Wander,
            hold_until: None,
            active_id: 0,
        }
    }

    fn read_json(path: &Option<PathBuf>) -> Option<serde_json::Value> {
        let text = std::fs::read_to_string(path.as_ref()?).ok()?;
        serde_json::from_str(&text).ok()
    }

    fn update(&mut self) -> Look {
        if self.last_check.elapsed() < Duration::from_millis(50) {
            return self.current();
        }
        self.last_check = Instant::now();
        let request = Self::read_json(&self.config).and_then(|config| config.get("gaze_capture").cloned());
        let id = request.as_ref().and_then(|request| request.get("id")).and_then(|id| id.as_i64()).unwrap_or(0);
        let target = request.as_ref().and_then(|request| request.get("target")).and_then(|t| t.as_str()).unwrap_or("");
        let done_id = Self::read_json(&self.status)
            .and_then(|status| status.get("gaze_capture").cloned())
            .filter(|capture| capture.get("state").and_then(|state| state.as_str()) == Some("done"))
            .and_then(|capture| capture.get("id").and_then(|id| id.as_i64()))
            .unwrap_or(0);
        if id != 0 && id != done_id {
            // A capture is running (or about to): look where it wants
            if id != self.active_id {
                self.active_id = id;
                eprintln!("[fake-eye-server] eye fit asks for \"{target}\" (capture {id}); looking there");
            }
            self.look = match target {
                "center" => Look::Target(0.0, 0.0),
                "up" => Look::Target(0.0, 15.0),
                "down" => Look::Target(0.0, -15.0),
                "left" => Look::Target(-20.0, 0.0),
                "right" => Look::Target(20.0, 0.0),
                "closed" => Look::Closed,
                _ => self.look,
            };
            self.hold_until = Some(Instant::now() + Duration::from_secs(3));
        } else if self.look == Look::Closed && id == done_id {
            // The eyes-shut step is over: open them again
            self.look = Look::Wander;
            self.hold_until = None;
        }
        self.current()
    }

    fn current(&mut self) -> Look {
        match self.hold_until {
            Some(until) if Instant::now() > until => {
                self.hold_until = None;
                self.look = Look::Wander;
                Look::Wander
            }
            _ => self.look,
        }
    }
}

fn direction(yaw_deg: f32, pitch_deg: f32) -> [f32; 3] {
    let (x, y) = (yaw_deg.to_radians().tan(), pitch_deg.to_radians().tan());
    let length = (x * x + y * y + 1.0).sqrt();
    [x / length, y / length, -1.0 / length]
}

fn main() {
    let mut rate = 120.0f64;
    let mut bias = true;
    let mut config = None;
    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--rate" if i + 1 < args.len() => {
                rate = args[i + 1].parse().unwrap_or(rate).clamp(5.0, 500.0);
                i += 1;
            }
            "--no-bias" => bias = false,
            "--config" if i + 1 < args.len() => {
                config = Some(PathBuf::from(&args[i + 1]));
                i += 1;
            }
            "-h" | "--help" => {
                println!("fake-eye-server [--rate HZ] [--no-bias] [--config PATH]\nPretends to be the Steam Frame's eye tracker at {PATH}.");
                return;
            }
            other => {
                eprintln!("unknown option {other}");
                std::process::exit(2);
            }
        }
        i += 1;
    }
    unsafe {
        libc::signal(libc::SIGINT, on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t);
    }
    let shm = match Shm::create() {
        Ok(shm) => shm,
        Err(error) => {
            eprintln!("[fake-eye-server] can't create {PATH}: {error}");
            std::process::exit(1);
        }
    };
    eprintln!("[fake-eye-server] publishing made-up eye samples at {rate:.0} Hz to {PATH} (Ctrl+C to stop)");

    let mut rng = Rng(0x9e3779b97f4a7c15);
    let mut fit = FitWatcher::new(config);
    let start = Instant::now();
    let period = Duration::from_secs_f64(1.0 / rate);
    let mut next = Instant::now();
    // Wandering: fixations at random points, with quick saccades between them
    let (mut yaw, mut pitch) = (0.0f32, 0.0f32);
    let (mut goal_yaw, mut goal_pitch) = (0.0f32, 0.0f32);
    let mut next_saccade = 0.8f64;
    let mut next_blink = 2.5f64;
    let mut blink_start = -10.0f64;
    let mut wink_start = -10.0f64;
    let mut next_wink = 17.0f64;
    let (mut open_left, mut open_right) = (0.84f32, 0.84f32);

    while !STOP.load(Ordering::SeqCst) {
        let t = start.elapsed().as_secs_f64();
        let look = fit.update();
        let (target_yaw, target_pitch, shut) = match look {
            Look::Wander => {
                if t > next_saccade {
                    goal_yaw = rng.noise(34.0);
                    goal_pitch = rng.noise(20.0) - 2.0;
                    next_saccade = t + 0.6 + rng.next() as f64 * 1.6;
                }
                (goal_yaw, goal_pitch, false)
            }
            Look::Target(y, p) => (y, p, false),
            Look::Closed => (0.0, 0.0, true),
        };
        // Saccade: cover most of the way within ~50 ms
        let k = 1.0 - (-(1.0 / rate as f32) * 45.0).exp();
        yaw += (target_yaw - yaw) * k;
        pitch += (target_pitch - pitch) * k;

        // Blinks (not while following a fit dot) and a wink now and then
        if look == Look::Wander && t > next_blink {
            blink_start = t;
            next_blink = t + 2.5 + rng.next() as f64 * 3.5;
        }
        if look == Look::Wander && t > next_wink {
            wink_start = t;
            next_wink = t + 15.0 + rng.next() as f64 * 10.0;
        }
        let blink = |since: f64| -> f32 {
            let s = (t - since) as f32;
            if s < 0.0 || s > 0.26 { 1.0 } else if s < 0.08 { 1.0 - s / 0.08 } else if s < 0.14 { 0.0 } else { (s - 0.14) / 0.12 }
        };
        let blinking = blink(blink_start);
        let winking = if (t - wink_start) < 0.6 { 0.0 } else { 1.0 };
        // The Frame reads eyes as less open when looking down
        let relaxed = 0.84 - 0.25 * (-pitch / 20.0).max(0.0);
        let (goal_left, goal_right) = if shut {
            (0.18, 0.19)
        } else {
            (0.2 + (relaxed - 0.2) * blinking, 0.2 + (relaxed * 1.02 - 0.2) * blinking * winking)
        };
        let lid_k = 1.0 - (-(1.0 / rate as f32) * 60.0).exp();
        open_left += (goal_left - open_left) * lid_k;
        open_right += (goal_right - open_right) * lid_k;

        // What the "sensor" reports: a little noise, and (unless --no-bias) a bit low and narrow
        let (sensor_yaw, sensor_pitch) = if bias { (yaw * 0.95 + 1.0, pitch * 0.85 - 2.5) } else { (yaw, pitch) };
        let (n1, n2, n3, n4) = (rng.noise(0.5), rng.noise(0.5), rng.noise(0.5), rng.noise(0.5));
        // Each eye turns in a little toward a point about 2 m away
        let left = direction(sensor_yaw + 0.9 + n1, sensor_pitch + n2);
        let right = direction(sensor_yaw - 0.9 + n3, sensor_pitch + n4);
        let center = direction(sensor_yaw + (n1 + n3) / 2.0, sensor_pitch + (n2 + n4) / 2.0);
        let sample = Sample {
            time: 1000.0 + t,
            left,
            right,
            fixation: center.map(|c| c * 0.7),
            openness: [(open_left + rng.noise(0.01)).clamp(0.0, 1.2), (open_right + rng.noise(0.01)).clamp(0.0, 1.2)],
        };
        shm.publish(&sample);

        next += period;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            next = now;
        }
    }
    eprintln!("[fake-eye-server] stopped");
}
