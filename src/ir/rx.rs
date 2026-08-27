//! rc-core LIRC receiver: reads decoded scancodes in `LIRC_MODE_SCANCODE`.

use std::collections::HashMap;
use std::io::Read;
use std::os::unix::io::{AsRawFd, RawFd};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use tracing::debug;

use crate::error::{AppError, Result};

use super::LearnedCode;

// From include/uapi/linux/lirc.h (asm-generic ioctl encoding, arm64).
const LIRC_MODE_SCANCODE: libc::c_uint = 0x0000_0008;
const LIRC_CAN_REC_SCANCODE: libc::c_uint = 0x0000_0800;
const LIRC_CAN_SEND_SCANCODE: libc::c_uint = 0x0000_0008;
const LIRC_GET_FEATURES: libc::c_ulong = 0x4004_6900;
const LIRC_SET_REC_MODE: libc::c_ulong = 0x4004_690a;
const LIRC_SET_SEND_MODE: libc::c_ulong = 0x4004_6909;

const QUIET_PERIOD: Duration = Duration::from_millis(250);

/// `struct lirc_scancode` from `include/uapi/linux/lirc.h`.
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct LircScancode {
    timestamp: u64,
    flags: u16,
    rc_proto: u16,
    keycode: u32,
    scancode: u64,
}

pub fn proto_name(rc_proto: u16) -> String {
    static NAMES: &[(u16, &str)] = &[
        (0, "unknown"),
        (1, "other"),
        (2, "rc5"),
        (3, "rc5x_20"),
        (4, "rc5_sz"),
        (5, "jvc"),
        (6, "sony12"),
        (7, "sony15"),
        (8, "sony20"),
        (9, "nec"),
        (10, "necx"),
        (11, "nec32"),
        (12, "sanyo"),
        (13, "mcir2_kbd"),
        (14, "mcir2_mse"),
        (15, "rc6_0"),
        (16, "rc6_6a_20"),
        (17, "rc6_6a_24"),
        (18, "rc6_6a_32"),
        (19, "rc6_mce"),
        (20, "sharp"),
        (21, "xmp"),
        (22, "cec"),
        (23, "imon"),
        (24, "rcmm12"),
        (25, "rcmm24"),
        (26, "rcmm32"),
        (27, "xbox_dvd"),
    ];
    NAMES
        .iter()
        .find(|(v, _)| *v == rc_proto)
        .map(|(_, n)| n.to_string())
        .unwrap_or_else(|| format!("proto_{rc_proto}"))
}

pub fn proto_from_name(name: &str) -> Option<u16> {
    let names: HashMap<&str, u16> = [
        ("unknown", 0u16),
        ("other", 1),
        ("rc5", 2),
        ("rc5x_20", 3),
        ("rc5_sz", 4),
        ("jvc", 5),
        ("sony12", 6),
        ("sony15", 7),
        ("sony20", 8),
        ("nec", 9),
        ("necx", 10),
        ("nec32", 11),
        ("sanyo", 12),
        ("mcir2_kbd", 13),
        ("mcir2_mse", 14),
        ("rc6_0", 15),
        ("rc6_6a_20", 16),
        ("rc6_6a_24", 17),
        ("rc6_6a_32", 18),
        ("rc6_mce", 19),
        ("sharp", 20),
        ("xmp", 21),
        ("cec", 22),
        ("imon", 23),
        ("rcmm12", 24),
        ("rcmm24", 25),
        ("rcmm32", 26),
        ("xbox_dvd", 27),
    ]
    .into_iter()
    .collect();
    names.get(name).copied()
}

pub struct LircReceiver {
    file: std::fs::File,
}

/// Resolve the receiver device: explicit path, or the first rc-core lirc
/// device that can report scancodes.
pub fn resolve_device(requested: &str) -> Option<String> {
    if requested != "auto" && !requested.trim().is_empty() {
        return Some(requested.trim().to_string());
    }
    scan_rc_devices()
        .into_iter()
        .find(|(_, can_rec)| *can_rec)
        .map(|(dev, _)| dev)
}

/// Enumerate rc-core LIRC devices via sysfs: `($(DEVNAME), can_rec_scancode)`.
pub fn scan_rc_devices() -> Vec<(String, bool)> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir("/sys/class/rc") else {
        return found;
    };
    let mut dirs: Vec<_> = entries.flatten().collect();
    dirs.sort_by_key(|d| d.file_name());
    for entry in dirs {
        for lirc in std::fs::read_dir(entry.path()).into_iter().flatten() {
            let Ok(uevent) = std::fs::read_to_string(lirc.path().join("uevent")) else {
                continue;
            };
            let Some(devname) = uevent.lines().find_map(|l| l.strip_prefix("DEVNAME=")) else {
                continue;
            };
            let path = format!("/dev/{devname}");
            let can_rec = std::fs::File::open(&path)
                .map(|f| {
                    get_features(f.as_raw_fd())
                        .map(|v| v & LIRC_CAN_REC_SCANCODE != 0)
                        .unwrap_or(false)
                })
                .unwrap_or(false);
            found.push((path, can_rec));
        }
    }
    found
}

fn get_features(fd: RawFd) -> Result<u32> {
    let mut value: libc::c_uint = 0;
    let rc = unsafe { libc::ioctl(fd, LIRC_GET_FEATURES, &mut value) };
    if rc < 0 {
        return Err(AppError::Internal(format!(
            "LIRC_GET_FEATURES failed: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(value)
}

impl LircReceiver {
    pub fn open(requested: &str) -> Result<Self> {
        let path = resolve_device(requested).ok_or_else(|| {
            AppError::Internal(
                "no IR receiver found (meson-ir rc-core device with scancode support required)"
                    .to_string(),
            )
        })?;

        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)
            .map_err(|e| AppError::Internal(format!("open {path} failed: {e}")))?;

        let features = get_features(file.as_raw_fd())?;
        if features & LIRC_CAN_REC_SCANCODE == 0 {
            return Err(AppError::Internal(format!(
                "{path} does not support LIRC_MODE_SCANCODE reception"
            )));
        }

        let mut mode: libc::c_uint = LIRC_MODE_SCANCODE;
        let rc = unsafe { libc::ioctl(file.as_raw_fd(), LIRC_SET_REC_MODE, &mut mode) };
        if rc < 0 {
            return Err(AppError::Internal(format!(
                "LIRC_SET_REC_MODE failed on {path}: {}",
                std::io::Error::last_os_error()
            )));
        }
        debug!("IR receiver opened on {path} (features=0x{features:08x})");
        Ok(Self { file })
    }

    /// Blocking read of one decoded scancode with a poll-based timeout.
    pub fn read_scancode(&mut self, timeout: Duration) -> Result<Option<(u16, u64, u16)>> {
        let deadline = Instant::now() + timeout;
        loop {
            let mut buf = [0u8; 24];
            match self.file.read(&mut buf) {
                Ok(0) => return Ok(None),
                Ok(n) if n >= std::mem::size_of::<LircScancode>() => {
                    let sc: LircScancode =
                        unsafe { std::ptr::read_unaligned(buf.as_ptr() as *const LircScancode) };
                    return Ok(Some((sc.rc_proto, sc.scancode, sc.flags)));
                }
                Ok(_) => continue,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::Interrupted =>
                {
                    let now = Instant::now();
                    if now >= deadline {
                        return Ok(None);
                    }
                    let remaining = deadline - now;
                    self.poll_wait(remaining.min(Duration::from_millis(100)))?;
                }
                Err(e) => {
                    return Err(AppError::Internal(format!("IR read failed: {e}")));
                }
            }
        }
    }

    fn poll_wait(&self, timeout: Duration) -> Result<()> {
        let mut fds = [libc::pollfd {
            fd: self.file.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }];
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), 1, timeout.as_millis() as libc::c_int) };
        if rc < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() != std::io::ErrorKind::Interrupted {
                return Err(AppError::Internal(format!("poll failed: {err}")));
            }
        }
        Ok(())
    }

    /// Best-effort probe: does a receiver exist and does it report scancodes?
    pub fn probe(requested: &str) -> Option<String> {
        resolve_device(requested)
    }
}

/// Capture one button press: first decoded scancode, then wait for the
/// repeat stream to go quiet before returning. Runs the blocking LIRC reads
/// on a dedicated thread; `cancelled` aborts promptly.
pub async fn capture(
    device: String,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    timeout: Duration,
) -> Result<LearnedCode> {
    tokio::task::spawn_blocking(move || capture_blocking(&device, cancelled, timeout))
        .await
        .map_err(|e| AppError::Internal(format!("learn task failed: {e}")))?
}

fn capture_blocking(
    device: &str,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    timeout: Duration,
) -> Result<LearnedCode> {
    let mut receiver = LircReceiver::open(device)?;
    let deadline = Instant::now() + timeout;
    let mut captured: Option<(u16, u64, u16)> = None;

    loop {
        if cancelled.load(Ordering::Relaxed) {
            return Err(AppError::Conflict("learn cancelled".to_string()));
        }

        let slice = if captured.is_some() {
            QUIET_PERIOD
        } else {
            Duration::from_millis(50)
        };
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }

        match receiver.read_scancode(slice.min(remaining).max(Duration::from_millis(1)))? {
            Some((proto, scancode, flags)) => {
                // Ignore NEC/SANYO repeats once something is captured; treat a
                // different code as a fresh press (previous remote button).
                if let Some((p, s, _)) = captured {
                    if (proto, scancode) == (p, s) || flags & 2 != 0 {
                        continue;
                    }
                }
                captured = Some((proto, scancode, flags));
                debug!("IR captured: proto={proto} scancode=0x{scancode:x}");
            }
            None => {
                if let Some((proto, scancode, _)) = captured {
                    return Ok(LearnedCode {
                        proto: proto_name(proto),
                        scancode,
                    });
                }
            }
        }
    }

    if let Some((proto, scancode, _)) = captured {
        Ok(LearnedCode {
            proto: proto_name(proto),
            scancode,
        })
    } else {
        Err(AppError::Internal(
            "learn timeout: no IR signal captured".to_string(),
        ))
    }
}

/// Best-effort probe for a kernel transmitter (used by hardware status).
pub fn probe_tx_device() -> Option<String> {
    scan_rc_devices()
        .into_iter()
        .filter_map(|(dev, _)| {
            std::fs::File::open(&dev).ok().and_then(|f| {
                let mut features: libc::c_uint = 0;
                let rc = unsafe { libc::ioctl(f.as_raw_fd(), LIRC_GET_FEATURES, &mut features) };
                (rc >= 0 && features & LIRC_CAN_SEND_SCANCODE != 0).then(|| dev.clone())
            })
        })
        .next()
}
