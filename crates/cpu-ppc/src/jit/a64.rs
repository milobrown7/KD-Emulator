//! Minimal AArch64 instruction encoder (32-bit integer subset).

pub const WZR: u32 = 31;

pub fn movz(rd: u32, imm16: u32, hw: u32) -> u32 {
    0x5280_0000 | hw << 21 | (imm16 & 0xFFFF) << 5 | rd
}

pub fn movk(rd: u32, imm16: u32, hw: u32) -> u32 {
    0x7280_0000 | hw << 21 | (imm16 & 0xFFFF) << 5 | rd
}

/// Materialise a 32-bit constant (1 or 2 instructions).
pub fn mov_imm(rd: u32, v: u32) -> Vec<u32> {
    let lo = v & 0xFFFF;
    let hi = v >> 16;
    match (lo, hi) {
        (_, 0) => vec![movz(rd, lo, 0)],
        (0, _) => vec![movz(rd, hi, 1)],
        _ => vec![movz(rd, lo, 0), movk(rd, hi, 1)],
    }
}

/// `LDR Wt, [Xn, #byte_off]` (unsigned offset, multiple of 4).
pub fn ldr_w(rt: u32, rn: u32, byte_off: u32) -> u32 {
    debug_assert!(byte_off % 4 == 0 && byte_off / 4 < 4096);
    0xB940_0000 | (byte_off / 4) << 10 | rn << 5 | rt
}

/// `STR Wt, [Xn, #byte_off]`.
pub fn str_w(rt: u32, rn: u32, byte_off: u32) -> u32 {
    debug_assert!(byte_off % 4 == 0 && byte_off / 4 < 4096);
    0xB900_0000 | (byte_off / 4) << 10 | rn << 5 | rt
}

fn rrr(base: u32, rd: u32, rn: u32, rm: u32) -> u32 {
    base | rm << 16 | rn << 5 | rd
}

pub fn add(rd: u32, rn: u32, rm: u32) -> u32 {
    rrr(0x0B00_0000, rd, rn, rm)
}
pub fn sub(rd: u32, rn: u32, rm: u32) -> u32 {
    rrr(0x4B00_0000, rd, rn, rm)
}
pub fn and(rd: u32, rn: u32, rm: u32) -> u32 {
    rrr(0x0A00_0000, rd, rn, rm)
}
pub fn orr(rd: u32, rn: u32, rm: u32) -> u32 {
    rrr(0x2A00_0000, rd, rn, rm)
}
pub fn eor(rd: u32, rn: u32, rm: u32) -> u32 {
    rrr(0x4A00_0000, rd, rn, rm)
}
/// `rn & !rm`
pub fn bic(rd: u32, rn: u32, rm: u32) -> u32 {
    rrr(0x0A20_0000, rd, rn, rm)
}
/// `rn | !rm`
pub fn orn(rd: u32, rn: u32, rm: u32) -> u32 {
    rrr(0x2A20_0000, rd, rn, rm)
}
/// `rn ^ !rm`
pub fn eon(rd: u32, rn: u32, rm: u32) -> u32 {
    rrr(0x4A20_0000, rd, rn, rm)
}
pub fn mul(rd: u32, rn: u32, rm: u32) -> u32 {
    rrr(0x1B00_7C00, rd, rn, rm)
}
pub fn neg(rd: u32, rm: u32) -> u32 {
    sub(rd, WZR, rm)
}
pub fn mvn(rd: u32, rm: u32) -> u32 {
    orn(rd, WZR, rm)
}
pub fn mov(rd: u32, rm: u32) -> u32 {
    orr(rd, WZR, rm)
}

/// `ROR Wd, Wn, #shift` (EXTR Wd, Wn, Wn, #shift).
pub fn ror_imm(rd: u32, rn: u32, shift: u32) -> u32 {
    0x1380_0000 | rn << 16 | (shift & 31) << 10 | rn << 5 | rd
}

pub fn clz(rd: u32, rn: u32) -> u32 {
    0x5AC0_1000 | rn << 5 | rd
}

pub fn sxtb(rd: u32, rn: u32) -> u32 {
    0x1300_1C00 | rn << 5 | rd
}

pub fn sxth(rd: u32, rn: u32) -> u32 {
    0x1300_3C00 | rn << 5 | rd
}

pub fn ret() -> u32 {
    0xD65F_03C0
}

/// Encode a 32-bit logical immediate. Supports every contiguous (possibly
/// wrapping) run of ones — exactly the masks PowerPC `rlwinm` produces.
pub fn logical_imm32(mask: u32) -> Option<(u32, u32)> {
    if mask == 0 || mask == u32::MAX {
        return None;
    }
    let len = mask.count_ones();
    // Rotate so the run starts at bit 0, remembering by how much.
    for r in 0..32 {
        let rotated = mask.rotate_right(r);
        if rotated == (1u32 << len) - 1 {
            // mask == ROR(ones, immr) ⇔ rotated by r gives ones ⇒ immr = (32 - r) % 32
            return Some(((32 - r) % 32, len - 1));
        }
    }
    None
}

/// `AND Wd, Wn, #mask`.
pub fn and_imm(rd: u32, rn: u32, mask: u32) -> Option<u32> {
    let (immr, imms) = logical_imm32(mask)?;
    Some(0x1200_0000 | immr << 16 | imms << 10 | rn << 5 | rd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_encodings() {
        assert_eq!(movz(0, 42, 0), 0x5280_0540); // mov w0, #42
        assert_eq!(ret(), 0xD65F_03C0);
        assert_eq!(add(0, 1, 2), 0x0B02_0020); // add w0, w1, w2
        assert_eq!(sub(3, 4, 5), 0x4B05_0083); // sub w3, w4, w5
        assert_eq!(ldr_w(1, 0, 8), 0xB940_0801); // ldr w1, [x0, #8]
        assert_eq!(str_w(3, 0, 124), 0xB900_7C03); // str w3, [x0, #124]
        assert_eq!(mul(3, 1, 2), 0x1B02_7C23); // mul w3, w1, w2
        assert_eq!(ror_imm(1, 1, 8), 0x1381_2021); // ror w1, w1, #8
        assert_eq!(clz(3, 1), 0x5AC0_1023);
        assert_eq!(mov(3, 1), 0x2A01_03E3); // mov w3, w1
        assert_eq!(movk(2, 0x1234, 1), 0x72A2_4682); // movk w2, #0x1234, lsl #16
    }

    #[test]
    fn logical_immediates() {
        // and w3, w1, #0xff  → immr 0, imms 7
        assert_eq!(and_imm(3, 1, 0xFF), Some(0x1200_1C23));
        // and w3, w1, #0xff000000 → immr 8, imms 7
        assert_eq!(logical_imm32(0xFF00_0000), Some((8, 7)));
        // wrapping run 0xF000000F → 8 ones, immr 4
        assert_eq!(logical_imm32(0xF000_000F), Some((4, 7)));
        assert_eq!(logical_imm32(0x00FF_00FF), None, "not a single run");
        assert_eq!(logical_imm32(0), None);
    }
}
