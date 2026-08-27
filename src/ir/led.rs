//! WS2812 status LED driven by direct GPIO register access.
//!
//! The WS2812 protocol needs sub-microsecond timing, which the chardev GPIO
//! interface cannot deliver; the driver bit-bangs the data line via mapped
//! GPIO output registers with a calibrated busy-wait. An occasional glitched
//! frame (scheduler preemption mid-frame) self-heals on the next refresh.
//!
//! Register defaults target the Amlogic SM1/G12A AO bank (GPIOAO_8):
//! OEN `0xff800024` bit 8 (0 = output), OUT `0xff800034` bit 8, verified
//! against `drivers/pinctrl/meson/pinctrl-meson-g12a.c`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::watch;
use tracing::{debug, warn};

use crate::config::IrConfig;

const FRAME_PERIOD_US: u64 = 1250; // WS2812 bit period
const T0H_NS: u64 = 400;
const T1H_NS: u64 = 800;
const RESET_US: u64 = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LedColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl LedColor {
    pub const GREEN: Self = Self { r: 0, g: 255, b: 0 };
    pub const BLUE: Self = Self { r: 0, g: 0, b: 255 };
    pub const RED: Self = Self { r: 255, g: 0, b: 0 };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedPattern {
    Off,
    Solid { color: LedColor, hold_ms: u64 },
    Blink { color: LedColor },
}

impl LedPattern {
    pub fn solid(color: LedColor, hold_ms: u64) -> Self {
        LedPattern::Solid { color, hold_ms }
    }

    pub fn blink(color: LedColor) -> Self {
        LedPattern::Blink { color }
    }
}

struct LedBackend {
    regs: MmapRegs,
    /// nanoseconds per calibrated spin iteration
    ns_per_iter: f64,
}

struct MmapRegs {
    map: *mut libc::c_void,
    oen_ptr: *mut u32,
    out_ptr: *mut u32,
    mask: u32,
}

unsafe impl Send for MmapRegs {}

/// WS2812 status LED controller: owns the hardware, reacts to pattern changes
/// via a watch channel. Cheap to construct even when the hardware is absent.
pub struct Ws2812Led {
    pattern_tx: watch::Sender<LedPattern>,
    ready: AtomicBool,
}

impl Ws2812Led {
    pub fn new(config: &IrConfig) -> Self {
        let (pattern_tx, pattern_rx) = watch::channel(LedPattern::Off);
        let ready = AtomicBool::new(false);

        if !config.led_enabled {
            debug!("STA WS2812 LED disabled in config");
            return Self { pattern_tx, ready };
        }

        match open_backend(config) {
            Ok(backend) => {
                ready.store(true, Ordering::Relaxed);
                let brightness = config.led_brightness;
                std::thread::Builder::new()
                    .name("ws2812-led".to_string())
                    .spawn(move || led_task(backend, pattern_rx, brightness))
                    .expect("failed to spawn LED thread");
                debug!("STA WS2812 LED initialized");
            }
            Err(e) => {
                warn!("STA WS2812 LED unavailable: {}", e);
            }
        }

        Self { pattern_tx, ready }
    }

    pub fn apply_config(&self, config: &IrConfig) {
        if !config.led_enabled && self.ready.load(Ordering::Relaxed) {
            let _ = self.pattern_tx.send(LedPattern::Off);
        }
    }

    pub fn set(&self, pattern: LedPattern) {
        let _ = self.pattern_tx.send(pattern);
    }

    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Relaxed)
    }
}

fn open_backend(config: &IrConfig) -> Result<LedBackend, String> {
    if config.led_mmap_base == 0 {
        return Err("LED mmap base not configured".to_string());
    }

    let page = config.led_mmap_base & !0xfff;
    let offset = (config.led_mmap_base & 0xfff) as usize;
    let path = std::ffi::CString::new("/dev/mem").unwrap();
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDWR | libc::O_SYNC) };
    if fd < 0 {
        return Err(format!(
            "open /dev/mem failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    let map = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            0x1000,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd,
            page as libc::off_t,
        )
    };
    unsafe { libc::close(fd) };
    if map == libc::MAP_FAILED {
        return Err(format!(
            "mmap GPIO registers failed: {}",
            std::io::Error::last_os_error()
        ));
    }

    let base_ptr = map as *mut u32;
    let regs = MmapRegs {
        map,
        oen_ptr: unsafe { base_ptr.add((offset + config.led_oen_offset as usize) / 4) },
        out_ptr: unsafe { base_ptr.add((offset + config.led_out_offset as usize) / 4) },
        mask: 1u32 << config.led_bit.min(31),
    };

    let mut backend = LedBackend {
        regs,
        ns_per_iter: 0.6, // rough initial guess for ~1.5 GHz A55
    };
    backend.calibrate();
    Ok(backend)
}

impl LedBackend {
    fn calibrate(&mut self) {
        let iters = 2_000_000u64;
        let start = Instant::now();
        let mut x = 1u64;
        for _ in 0..iters {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
            std::hint::black_box(x);
        }
        let elapsed = start.elapsed().as_secs_f64();
        if elapsed > 0.0 {
            self.ns_per_iter = (elapsed * 1e9) / iters as f64;
        }
        std::hint::black_box(x);
        debug!("LED spin loop calibrated: {:.2} ns/iter", self.ns_per_iter);
    }

    #[inline]
    fn spin_ns(&self, ns: u64) {
        if self.ns_per_iter <= 0.0 {
            return;
        }
        let iters = (ns as f64 / self.ns_per_iter) as u64;
        let mut x = 1u64;
        for _ in 0..iters {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
        }
        std::hint::black_box(x);
    }

    #[inline]
    fn write(&self, high: bool) {
        unsafe {
            let v = self.regs.out_ptr.read_volatile();
            self.regs
                .out_ptr
                .write_volatile(if high { v | self.regs.mask } else { v & !self.regs.mask });
        }
    }

    /// Send one 24-bit GRB frame.
    fn send_frame(&self, color: LedColor) {
        let (g, r, b) = (color.g as u32, color.r as u32, color.b as u32);
        let value = (g << 16) | (r << 8) | b;

        self.write(false);
        self.spin_ns(RESET_US * 1000);

        for i in (0..24).rev() {
            let one = value >> i & 1 != 0;
            let high_ns = if one { T1H_NS } else { T0H_NS };
            self.write(true);
            self.spin_ns(high_ns);
            self.write(false);
            self.spin_ns(FRAME_PERIOD_US * 1000 - high_ns);
        }
        self.write(false);
    }
}

fn scale(color: LedColor, brightness: u32) -> LedColor {
    let f = brightness.min(100) as f64 / 100.0;
    LedColor {
        r: (color.r as f64 * f).round() as u8,
        g: (color.g as f64 * f).round() as u8,
        b: (color.b as f64 * f).round() as u8,
    }
}

fn led_task(mut backend: LedBackend, mut rx: watch::Receiver<LedPattern>, brightness: u32) {
    let mut current = *rx.borrow_and_update();

    loop {
        if rx.has_changed().unwrap_or(true) {
            current = *rx.borrow_and_update();
        }

        match current {
            LedPattern::Off => {
                backend.send_frame(LedColor { r: 0, g: 0, b: 0 });
                // Long idle: wait for a change (fall back to polling).
                for _ in 0..100 {
                    if rx.has_changed().unwrap_or(false) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            LedPattern::Solid { color, hold_ms } => {
                backend.send_frame(scale(color, brightness));
                let deadline = Instant::now() + Duration::from_millis(hold_ms.max(100));
                while Instant::now() < deadline {
                    if rx.has_changed().unwrap_or(false) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                if !rx.has_changed().unwrap_or(false) {
                    // Hold elapsed: switch to off unless a new pattern arrived.
                    current = LedPattern::Off;
                    backend.send_frame(LedColor { r: 0, g: 0, b: 0 });
                    let _ = rx.borrow_and_update();
                }
            }
            LedPattern::Blink { color } => {
                let color = scale(color, brightness);
                let period = 500u64;
                for phase in [true, false] {
                    if rx.has_changed().unwrap_or(false) {
                        break;
                    }
                    if phase {
                        backend.send_frame(color);
                    } else {
                        backend.send_frame(LedColor { r: 0, g: 0, b: 0 });
                    }
                    std::thread::sleep(Duration::from_millis(period / 2));
                }
                if rx.has_changed().unwrap_or(false) {
                    current = *rx.borrow_and_update();
                }
            }
        }
    }
}

impl Drop for MmapRegs {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.map, 0x1000) };
    }
}
