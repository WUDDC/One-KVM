//! IR transmission: kernel LIRC TX device (preferred, encodes all
//! rc-core protocols) with a userspace 38 kHz GPIO bit-bang fallback.

use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::time::{Duration, Instant};

use tracing::{debug, warn};

use crate::error::{AppError, Result};

use super::encoder;
use super::rx;
use crate::config::IrConfig;

const LIRC_MODE_SCANCODE: libc::c_uint = 0x0000_0008;
const LIRC_CAN_SEND_SCANCODE: libc::c_uint = 0x0000_0008;
const LIRC_CAN_SET_SEND_CARRIER: libc::c_uint = 0x0000_1000;
const LIRC_GET_FEATURES: libc::c_ulong = 0x4004_6900;
const LIRC_SET_SEND_MODE: libc::c_ulong = 0x4004_6909;
const LIRC_SET_SEND_CARRIER: libc::c_ulong = 0x4004_6913;

/// `struct lirc_scancode` from `include/uapi/linux/lirc.h` (24 bytes).
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct LircScancode {
    timestamp: u64,
    flags: u16,
    rc_proto: u16,
    keycode: u32,
    scancode: u64,
}

pub struct Transmitter {
    inner: TransmitterInner,
}

enum TransmitterInner {
    Lirc { file: std::fs::File, has_carrier: bool },
    Gpio { line: gpio_cdev::LineHandle, mmap: Option<MmapGpio> },
}

impl Transmitter {
    /// Resolve the transmitter mode: `auto` prefers a kernel TX-capable LIRC
    /// device, then falls back to raw GPIO. Returns `(mode, device_path)`.
    pub fn probe(config: &IrConfig) -> Option<(String, Option<String>)> {
        let mode = config.tx_mode.as_str();
        if mode == "none" {
            return None;
        }

        if mode == "auto" || mode == "lirc" {
            if let Some(dev) = rx::probe_tx_device() {
                return Some(("lirc".to_string(), Some(dev)));
            }
            if mode == "lirc" {
                return None;
            }
        }

        if mode == "auto" || mode == "gpio" {
            return match Self::open_gpio(config) {
                Ok(_) => Some(("gpio".to_string(), None)),
                Err(_) => None,
            };
        }
        None
    }

    pub fn open(config: &IrConfig) -> Result<Self> {
        match Self::probe(config) {
            Some((mode, device)) => {
                if mode == "lirc" {
                    let file = std::fs::OpenOptions::new()
                        .write(true)
                        .open(device.as_deref().unwrap_or("/dev/lirc1"))
                        .map_err(|e| {
                            AppError::Internal(format!("open TX device failed: {e}"))
                        })?;
                    let mut features: libc::c_uint = 0;
                    let rc =
                        unsafe { libc::ioctl(file.as_raw_fd(), LIRC_GET_FEATURES, &mut features) };
                    let has_carrier = rc >= 0 && features & LIRC_CAN_SET_SEND_CARRIER != 0;
                    debug!("IR TX via kernel device {}", device.as_deref().unwrap_or("?"));
                    Ok(Self {
                        inner: TransmitterInner::Lirc { file, has_carrier },
                    })
                } else {
                    let line = Self::open_gpio(config)?;
                    Ok(Self { inner: line })
                }
            }
            None => Err(AppError::Internal(
                "no IR transmitter available (enable a gpio-ir-tx device or configure the TX GPIO)"
                    .to_string(),
            )),
        }
    }

    fn open_gpio(config: &IrConfig) -> Result<TransmitterInner> {
        if config.tx_gpio_chip.trim().is_empty() {
            return Err(AppError::Internal(
                "TX GPIO chip not configured".to_string(),
            ));
        }

        let mut chip = gpio_cdev::Chip::new(config.tx_gpio_chip.trim())
            .map_err(|e| AppError::Internal(format!("open {} failed: {e}", config.tx_gpio_chip)))?;
        let line = chip
            .get_line(config.tx_gpio_line)
            .map_err(|e| AppError::Internal(format!("GPIO line {} failed: {e}", config.tx_gpio_line)))?;
        let handle = line
            .request(gpio_cdev::LineRequestFlags::OUTPUT, 0, "one-kvm-ir")
            .map_err(|e| AppError::Internal(format!("GPIO TX request failed: {e}")))?;

        let mmap = MmapGpio::open(
            config.tx_mmap_base,
            config.tx_mmap_oen_offset,
            config.tx_mmap_out_offset,
            config.tx_bit,
        )
        .inspect_err(|e| warn!("mmap TX unavailable, falling back to line ioctls: {}", e))
        .ok();
        if mmap.is_some() {
            debug!("IR TX via GPIO mmap ({} bit {})", config.tx_gpio_chip, config.tx_bit);
        } else {
            debug!(
                "IR TX via GPIO ioctl ({} line {})",
                config.tx_gpio_chip, config.tx_gpio_line
            );
        }
        Ok(TransmitterInner::Gpio { line: handle, mmap })
    }

    /// Transmit a learned code.
    pub fn transmit(
        &mut self,
        proto: &str,
        scancode: Option<i64>,
        raw: Option<&[u32]>,
        carrier: i64,
    ) -> Result<()> {
        match &mut self.inner {
            TransmitterInner::Lirc { file, has_carrier } => {
                let rc_proto = rx::proto_from_name(proto).ok_or_else(|| {
                    AppError::BadRequest(format!("unknown IR protocol '{proto}'"))
                })?;
                let scancode = scancode
                    .map(|s| s as u64)
                    .ok_or_else(|| AppError::BadRequest("button has no scancode".to_string()))?;

                if *has_carrier && carrier > 0 {
                    let mut c = carrier as libc::c_uint;
                    unsafe {
                        libc::ioctl(file.as_raw_fd(), LIRC_SET_SEND_CARRIER, &mut c);
                    }
                }

                let mut mode: libc::c_uint = LIRC_MODE_SCANCODE;
                let rc = unsafe { libc::ioctl(file.as_raw_fd(), LIRC_SET_SEND_MODE, &mut mode) };
                if rc < 0 {
                    return Err(AppError::Internal(format!(
                        "LIRC_SET_SEND_MODE failed: {}",
                        std::io::Error::last_os_error()
                    )));
                }

                let sc = LircScancode {
                    rc_proto,
                    scancode,
                    ..Default::default()
                };
                let bytes: [u8; 24] = unsafe { std::mem::transmute(sc) };
                file.write_all(&bytes)
                    .and_then(|_| file.flush())
                    .map_err(|e| AppError::Internal(format!("IR send failed: {e}")))?;
                Ok(())
            }
            TransmitterInner::Gpio { line, mmap } => {
                let pulses = match raw {
                    Some(r) if !r.is_empty() => r.to_vec(),
                    _ => {
                        let sc = scancode.ok_or_else(|| {
                            AppError::BadRequest(
                                "button has neither raw data nor a scancode".to_string(),
                            )
                        })?;
                        encoder::encode(proto, sc as u64).ok_or_else(|| {
                            AppError::BadRequest(format!(
                                "protocol '{proto}' needs a kernel transmitter (gpio-ir-tx); \
                                 the GPIO fallback only supports NEC"
                            ))
                        })?
                    }
                };

                match mmap {
                    Some(m) => modulate_mmap(m, &pulses, carrier),
                    None => modulate_ioctl(line, &pulses, carrier),
                }
            }
        }
    }
}

const CARRIER_PERIOD_US: f64 = 1_000_000.0 / 38_000.0;
const DUTY_MARK_RATIO: f64 = 1.0 / 3.0;

/// LIRC raw trains are mark,space,mark,space... marks are modulated onto the
/// carrier; spaces hold the line low.
fn modulate_raw(mut write: impl FnMut(bool), pulses: &[u32], carrier: i64) {
    let (period, mark_us) = carrier_timing(carrier);
    for (index, &duration) in pulses.iter().enumerate() {
        let mut remaining = duration as f64;
        if index % 2 == 0 {
            while remaining >= period {
                write(true);
                spin_us(mark_us);
                write(false);
                spin_us(period - mark_us);
                remaining -= period;
            }
            if remaining > 0.5 {
                let on = (remaining * DUTY_MARK_RATIO).min(remaining);
                write(true);
                spin_us(on);
                write(false);
                spin_us(remaining - on);
            }
        } else {
            write(false);
            spin_us(remaining);
        }
    }
    write(false);
}

fn carrier_timing(carrier: i64) -> (f64, f64) {
    if carrier > 0 {
        let period = 1_000_000.0 / carrier as f64;
        (period, period * DUTY_MARK_RATIO)
    } else {
        (CARRIER_PERIOD_US, CARRIER_PERIOD_US * DUTY_MARK_RATIO)
    }
}

fn modulate_mmap(m: &MmapGpio, pulses: &[u32], carrier: i64) -> Result<()> {
    m.set_output(true);
    modulate_raw(|high| m.write(high), pulses, carrier);
    Ok(())
}

fn modulate_ioctl(line: &gpio_cdev::LineHandle, pulses: &[u32], carrier: i64) -> Result<()> {
    let set = |high: bool| -> Result<()> {
        line.set_value(if high { 1 } else { 0 })
            .map_err(|e| AppError::Internal(format!("GPIO write failed: {e}")))
    };
    set(false)?;
    modulate_raw(|high| set(high).expect("gpio write"), pulses, carrier);
    Ok(())
}

#[inline]
fn spin_us(micros: f64) {
    if micros <= 0.0 {
        return;
    }
    let target = Duration::from_secs_f64(micros / 1_000_000.0);
    let start = Instant::now();
    while start.elapsed() < target {
        std::hint::spin_loop();
    }
}

/// Direct register access for sub-microsecond carrier timing. Addresses
/// default to the Amlogic SM1/G12A periphs GPIO bank (GPIOX_23).
pub struct MmapGpio {
    map: *mut libc::c_void,
    oen_ptr: *mut u32,
    out_ptr: *mut u32,
    mask: u32,
}

unsafe impl Send for MmapGpio {}

impl MmapGpio {
    pub fn open(base: u64, oen_offset: u32, out_offset: u32, bit: u32) -> Result<Self> {
        if base == 0 {
            return Err(AppError::Internal("mmap GPIO base not configured".to_string()));
        }
        if bit >= 32 {
            return Err(AppError::Internal("mmap GPIO bit out of range".to_string()));
        }

        let page = base & !0xfff;
        let offset = (base & 0xfff) as usize;
        let path = std::ffi::CString::new("/dev/mem").unwrap();
        let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDWR | libc::O_SYNC) };
        if fd < 0 {
            return Err(AppError::Internal(format!(
                "open /dev/mem failed: {}",
                std::io::Error::last_os_error()
            )));
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
            return Err(AppError::Internal(format!(
                "mmap of GPIO registers failed: {}",
                std::io::Error::last_os_error()
            )));
        }

        let base_ptr = map as *mut u32;
        Ok(Self {
            map,
            oen_ptr: unsafe { base_ptr.add((offset + oen_offset as usize) / 4) },
            out_ptr: unsafe { base_ptr.add((offset + out_offset as usize) / 4) },
            mask: 1u32 << bit,
        })
    }

    /// Set the pin to output mode (meson OEN is active-low: 0 = output).
    fn set_output(&self, output: bool) {
        unsafe {
            let v = self.oen_ptr.read_volatile();
            self.oen_ptr
                .write_volatile(if output { v & !self.mask } else { v | self.mask });
        }
    }

    fn write(&self, high: bool) {
        unsafe {
            let v = self.out_ptr.read_volatile();
            self.out_ptr
                .write_volatile(if high { v | self.mask } else { v & !self.mask });
        }
    }
}

impl Drop for MmapGpio {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.map, 0x1000);
        }
    }
}
