//! Userspace protocol encoding: protocol + scancode → raw pulse/space
//! durations in microseconds (mark first). Used by the GPIO bit-bang
//! transmitter; the kernel LIRC TX path encodes by itself.

/// Encode a learned code into pulse durations (µs). Returns `None` when the
/// protocol has no userspace encoder (then the kernel TX path is required).
pub fn encode(proto: &str, scancode: u64) -> Option<Vec<u32>> {
    match proto {
        "nec" => encode_nec(scancode as u16),
        "necx" => encode_necx(scancode),
        "nec32" => encode_nec32(scancode as u32),
        _ => None,
    }
}

/// NEC 32-bit: addr, ~addr, cmd, ~cmd (LSB-first), 38 kHz.
/// rc-core stores `scancode = (addr << 8) | cmd` with inversion implicit.
fn encode_nec(scancode: u16) -> Option<Vec<u32>> {
    let addr = (scancode >> 8) as u8;
    let cmd = scancode as u8;
    let mut bits = Vec::with_capacity(32);
    bits.extend(bits_lsb_first(addr));
    bits.extend(bits_lsb_first(!addr));
    bits.extend(bits_lsb_first(cmd));
    bits.extend(bits_lsb_first(!cmd));
    Some(frame(&bits))
}

/// Extended NEC: 16-bit address + command (with inversion), LSB-first.
fn encode_necx(scancode: u64) -> Option<Vec<u32>> {
    let addr = ((scancode >> 8) & 0xff) as u8;
    let cmd = (scancode & 0xff) as u8;
    let mut bits = Vec::with_capacity(32);
    bits.extend(bits_lsb_first(addr));
    bits.extend(bits_lsb_first(cmd));
    bits.extend(bits_lsb_first(!cmd));
    Some(frame(&bits))
}

/// NEC32: raw 32-bit value, LSB-first, no inversion logic.
fn encode_nec32(scancode: u32) -> Option<Vec<u32>> {
    let mut bits = Vec::with_capacity(32);
    for byte in scancode.to_le_bytes() {
        bits.extend(bits_lsb_first(byte));
    }
    Some(frame(&bits))
}

fn bits_lsb_first(byte: u8) -> Vec<bool> {
    (0..8).map(|i| byte & (1 << i) != 0).collect()
}

/// NEC frame: 9 ms mark, 4.5 ms space, 32 bits, trailing mark.
fn frame(bits: &[bool]) -> Vec<u32> {
    let mut out = Vec::with_capacity(bits.len() * 2 + 4);
    out.push(9000);
    out.push(4500);
    for &bit in bits {
        out.push(560);
        out.push(if bit { 1690 } else { 560 });
    }
    out.push(560);
    out.push(40000); // trailing space (settling)
    out
}
