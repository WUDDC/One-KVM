use serde::{Deserialize, Serialize};
use typeshare::typeshare;

/// Infrared remote hardware and behavior settings.
///
/// Register defaults target the S905L3A/B (Amlogic SM1/G12A family) reference
/// board: IR TX on GPIOX_23 (periphs bank), WS2812 status LED on GPIOAO_8
/// (AO bank). See `src/ir/` for the verified register layout.
#[typeshare]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct IrConfig {
    pub enabled: bool,
    /// LIRC device used for learning: `auto` or e.g. `/dev/lirc0`.
    pub rx_device: String,
    /// `auto` | `lirc` | `gpio` | `none`.
    pub tx_mode: String,
    /// Chardev GPIO chip for the raw TX fallback.
    pub tx_gpio_chip: String,
    /// GPIO line number for the raw TX fallback (GPIOX_23 = 23).
    pub tx_gpio_line: u32,
    /// `/dev/mem` physical base of the TX GPIO bank (0 disables mmap TX).
    pub tx_mmap_base: u64,
    /// Offset of the output-enable register from `tx_mmap_base` (bytes).
    pub tx_mmap_oen_offset: u32,
    /// Offset of the data-out register from `tx_mmap_base` (bytes).
    pub tx_mmap_out_offset: u32,
    /// Bit within the TX GPIO bank registers.
    pub tx_bit: u32,
    pub carrier: u32,
    pub learn_timeout_ms: u64,
    pub led_enabled: bool,
    /// `/dev/mem` physical base of the LED GPIO bank (0 disables the LED).
    pub led_mmap_base: u64,
    pub led_oen_offset: u32,
    pub led_out_offset: u32,
    pub led_bit: u32,
    /// WS2812 brightness, 0-100.
    pub led_brightness: u32,
}

impl Default for IrConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            rx_device: "auto".to_string(),
            tx_mode: "auto".to_string(),
            tx_gpio_chip: "/dev/gpiochip0".to_string(),
            tx_gpio_line: 23,
            // Periphs GPIO bank (0xff634440): OEN = +0x1c (reg 7), OUT = +0x20 (reg 8).
            tx_mmap_base: 0xff63_4440,
            tx_mmap_oen_offset: 28,
            tx_mmap_out_offset: 32,
            tx_bit: 23,
            carrier: 38000,
            learn_timeout_ms: 10000,
            led_enabled: true,
            // AO GPIO bank (0xff800024): OEN = +0x0, OUT = +0x10 (reg 4).
            led_mmap_base: 0xff80_0024,
            led_oen_offset: 0,
            led_out_offset: 16,
            led_bit: 8,
            led_brightness: 40,
        }
    }
}

impl IrConfig {
    pub fn normalize(&mut self) {
        if !matches!(self.tx_mode.as_str(), "auto" | "lirc" | "gpio" | "none") {
            self.tx_mode = "auto".to_string();
        }
        self.tx_gpio_line = self.tx_gpio_line.min(1023);
        self.tx_bit = self.tx_bit.min(31);
        self.led_bit = self.led_bit.min(31);
        self.carrier = self.carrier.clamp(20000, 60000);
        self.learn_timeout_ms = self.learn_timeout_ms.clamp(1000, 60000);
        self.led_brightness = self.led_brightness.clamp(1, 100);
        if self.rx_device.trim().is_empty() {
            self.rx_device = "auto".to_string();
        }
    }
}
