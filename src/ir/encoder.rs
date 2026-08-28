//! Userspace protocol encoding: protocol + scancode → raw pulse/space
//! durations in microseconds (mark first). Used by the GPIO bit-bang
//! transmitter; the kernel LIRC TX path encodes by itself.

/// Encode a learned code into pulse durations (µs). Returns `None` when the
/// protocol has no userspace encoder, or when the scancode does not fit the
/// protocol's bit width (never silently truncated).
pub fn encode(proto: &str, scancode: u64) -> Option<Vec<u32>> {
    match proto {
        "nec" => encode_nec(scancode),
        "necx" => encode_necx(scancode),
        "nec32" => encode_nec32(scancode),
        _ => None,
    }
}

/// NEC 16-bit: addr, ~addr, cmd, ~cmd (LSB-first), 38 kHz.
/// rc-core stores `scancode = (addr << 8) | cmd` with inversion implicit.
fn encode_nec(scancode: u64) -> Option<Vec<u32>> {
    if scancode > u16::MAX as u64 {
        return None;
    }
    let scancode = scancode as u16;
    let addr = (scancode >> 8) as u8;
    let cmd = scancode as u8;
    let mut bits = Vec::with_capacity(32);
    bits.extend(bits_lsb_first(addr));
    bits.extend(bits_lsb_first(!addr));
    bits.extend(bits_lsb_first(cmd));
    bits.extend(bits_lsb_first(!cmd));
    Some(frame(&bits))
}

/// Extended NEC (RC_PROTO_NECX): 24-bit scancode laid out as
/// `addr = bits 16-23`, `addr2 = bits 8-15`, `cmd = bits 0-7`.
/// Wire order: addr, addr2, cmd, !cmd — per `ir_nec_scancode_to_raw` in
/// drivers/media/rc/ir-nec-decoder.c.
fn encode_necx(scancode: u64) -> Option<Vec<u32>> {
    if scancode > 0x00ff_ffff {
        return None;
    }
    let addr = (scancode >> 16) as u8;
    let addr2 = (scancode >> 8) as u8;
    let cmd = scancode as u8;
    let mut bits = Vec::with_capacity(32);
    bits.extend(bits_lsb_first(addr));
    bits.extend(bits_lsb_first(addr2));
    bits.extend(bits_lsb_first(cmd));
    bits.extend(bits_lsb_first(!cmd));
    Some(frame(&bits))
}

/// NEC32 (RC_PROTO_NEC32): the learned scancode packs
/// `~addr = bits 24-31`, `addr = bits 16-23`, `~cmd = bits 8-15`,
/// `cmd = bits 0-7` (see `ir_nec_bytes_to_scancode` in
/// include/media/rc-core.h). Wire order: addr, ~addr, cmd, ~cmd,
/// LSB-first within each byte — per `ir_nec_scancode_to_raw` in
/// drivers/media/rc/ir-nec-decoder.c.
fn encode_nec32(scancode: u64) -> Option<Vec<u32>> {
    if scancode > u32::MAX as u64 {
        return None;
    }
    let scancode = scancode as u32;
    let bytes = [
        ((scancode >> 16) & 0xff) as u8, // addr
        ((scancode >> 24) & 0xff) as u8, // !addr
        (scancode & 0xff) as u8,         // cmd
        ((scancode >> 8) & 0xff) as u8,  // !cmd
    ];
    let mut bits = Vec::with_capacity(32);
    for byte in bytes {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Decode the data bytes back out of a `frame()` pulse train: strip the
    /// 9 ms / 4.5 ms header and the trailing mark + space, turn each
    /// (mark, space) pair into a bit (560 space = 0, 1690 = 1) and re-pack
    /// the LSB-first bits per byte.
    fn decode_bytes(frame: &[u32]) -> Vec<u8> {
        assert_eq!(frame[0], 9000);
        assert_eq!(frame[1], 4500);
        let pairs = &frame[2..frame.len() - 2];
        assert_eq!(pairs.len() % 2, 0);
        let bits: Vec<bool> = pairs
            .chunks(2)
            .map(|p| {
                assert_eq!(p[0], 560);
                p[1] == 1690
            })
            .collect();
        assert_eq!(bits.len() % 8, 0);
        bits.chunks(8)
            .map(|byte_bits| {
                byte_bits
                    .iter()
                    .enumerate()
                    .fold(0u8, |acc, (i, &bit)| acc | ((bit as u8) << i))
            })
            .collect()
    }

    /// NEC 16-bit: 0x807F = addr 0x80, cmd 0x7F → wire order
    /// addr, ~addr, cmd, ~cmd = 80 7F 7F 80.
    #[test]
    fn nec_sends_addr_inv_addr_cmd_inv_cmd() {
        let bytes = decode_bytes(&encode("nec", 0x807F).unwrap());
        assert_eq!(bytes, vec![0x80, 0x7F, 0x7F, 0x80]);
    }

    /// NECX layout per ir_nec_scancode_to_raw (RC_PROTO_NECX) in
    /// drivers/media/rc/ir-nec-decoder.c: addr = bits 16-23,
    /// addr2 = bits 8-15, cmd = bits 0-7, then !cmd.
    /// 0xFFEE80 → addr 0xFF, addr2 0xEE, cmd 0x80, !cmd 0x7F.
    #[test]
    fn necx_uses_24bit_layout_and_appends_inverted_cmd() {
        let bytes = decode_bytes(&encode("necx", 0xFFEE80).unwrap());
        assert_eq!(bytes, vec![0xFF, 0xEE, 0x80, 0x7F]);
    }

    /// NEC32 per ir_nec_scancode_to_raw (RC_PROTO_NEC32): the learned value
    /// packs ~addr = bits 24-31, addr = bits 16-23, ~cmd = bits 8-15,
    /// cmd = bits 0-7; wire order is addr, ~addr, cmd, ~cmd.
    /// 0x00FF_807F → FF 00 7F 80.
    #[test]
    fn nec32_addr_inverted_addr_cmd_inverted_cmd() {
        let bytes = decode_bytes(&encode("nec32", 0x00FF_807F).unwrap());
        assert_eq!(bytes, vec![0xFF, 0x00, 0x7F, 0x80]);
    }

    /// Scancodes wider than the protocol bit width are rejected instead of
    /// being silently truncated.
    #[test]
    fn out_of_range_scancodes_return_none() {
        assert!(encode("nec", 0x1_0000).is_none()); // > 16 bit
        assert!(encode("necx", 0x100_0000).is_none()); // > 24 bit
        assert!(encode("nec32", 0x1_0000_0000).is_none()); // > 32 bit
        assert!(encode("unknown", 0).is_none());
    }
}
