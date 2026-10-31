//! Cheat codes, applied once per frame through [`CheatMemory`].
//!
//! | Format | Systems | Support |
//! |---|---|---|
//! | Raw (`AAAAAAAA:VV`, `:VVVV`, `:VVVVVVVV`) | GBA, DS | constant writes |
//! | CodeBreaker (unencrypted) | GBA | types 2, 3, 6, 7, 8, A, B, C, D, E |
//! | GameShark / Action Replay v1–v2 | GBA | TEA decryption; types 0, 1, 2, D, E |
//! | Action Replay DS | DS | all documented types (0–F, D0–DC) |
//! | Gecko | GameCube, Wii | 00/02/04/06/08 writes, 20–2E conditionals, 42/4A, E0/E2 |
//!
//! Anything outside those tables is reported by [`Cheat::parse`] (when it
//! can tell) or skipped with a `tracing::warn!` when applied.

use crate::System;

/// Guest memory as seen by the main CPU. `size` is 1, 2 or 4 bytes; values
/// are numeric (the core handles the bus endianness).
pub trait CheatMemory {
    fn read(&mut self, addr: u32, size: u8) -> u32;
    fn write(&mut self, addr: u32, value: u32, size: u8);
}

/// Little-endian read of `size` bytes at `at` (0 when out of range).
pub fn read_le(m: &[u8], at: usize, size: u8) -> u32 {
    m.get(at..at + size as usize).map_or(0, |b| b.iter().rev().fold(0, |acc, &x| acc << 8 | x as u32))
}

/// Little-endian write (ignored when out of range).
pub fn write_le(m: &mut [u8], at: usize, value: u32, size: u8) {
    if let Some(b) = m.get_mut(at..at + size as usize) {
        b.copy_from_slice(&value.to_le_bytes()[..size as usize]);
    }
}

/// Big-endian read of `size` bytes at `at` (0 when out of range).
pub fn read_be(m: &[u8], at: usize, size: u8) -> u32 {
    m.get(at..at + size as usize).map_or(0, |b| b.iter().fold(0, |acc, &x| acc << 8 | x as u32))
}

/// Big-endian write (ignored when out of range).
pub fn write_be(m: &mut [u8], at: usize, value: u32, size: u8) {
    if let Some(b) = m.get_mut(at..at + size as usize) {
        b.copy_from_slice(&value.to_be_bytes()[4 - size as usize..]);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CheatFormat {
    Raw,
    CodeBreaker,
    GameShark,
    ActionReplayDs,
    Gecko,
}

impl CheatFormat {
    pub fn id(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::CodeBreaker => "codebreaker",
            Self::GameShark => "gameshark",
            Self::ActionReplayDs => "actionreplay",
            Self::Gecko => "gecko",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Some(match id {
            "raw" => Self::Raw,
            "codebreaker" => Self::CodeBreaker,
            "gameshark" => Self::GameShark,
            "actionreplay" => Self::ActionReplayDs,
            "gecko" => Self::Gecko,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Raw => "Raw",
            Self::CodeBreaker => "CodeBreaker",
            Self::GameShark => "GameShark / Action Replay",
            Self::ActionReplayDs => "Action Replay",
            Self::Gecko => "Gecko",
        }
    }

    /// Formats offered for a system, most common first.
    pub fn for_system(system: System) -> &'static [CheatFormat] {
        match system {
            System::Gba => &[Self::GameShark, Self::CodeBreaker, Self::Raw],
            System::Nds => &[Self::ActionReplayDs, Self::Raw],
            System::GameCube | System::Wii => &[Self::Gecko],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Line {
    a: u32,
    b: u32,
    /// Raw format only: write width in bytes.
    size: u8,
}

/// A parsed cheat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cheat {
    pub format: CheatFormat,
    lines: Vec<Line>,
}

const GSA_SEEDS: [u32; 4] = [0x09F4_FBBD, 0x9681_884A, 0x3520_27E9, 0xF3DE_E5A7];
const TEA_DELTA: u32 = 0x9E37_79B9;

/// Decrypt one GameShark / Action Replay v1–v2 line (TEA, 32 rounds).
pub fn gsa_decrypt(mut addr: u32, mut value: u32) -> (u32, u32) {
    let s = GSA_SEEDS;
    let mut sum = TEA_DELTA.wrapping_mul(32);
    for _ in 0..32 {
        value = value
            .wrapping_sub((addr << 4).wrapping_add(s[2]) ^ addr.wrapping_add(sum) ^ (addr >> 5).wrapping_add(s[3]));
        addr = addr
            .wrapping_sub((value << 4).wrapping_add(s[0]) ^ value.wrapping_add(sum) ^ (value >> 5).wrapping_add(s[1]));
        sum = sum.wrapping_sub(TEA_DELTA);
    }
    (addr, value)
}

/// Inverse of [`gsa_decrypt`] (used to test round trips).
pub fn gsa_encrypt(mut addr: u32, mut value: u32) -> (u32, u32) {
    let s = GSA_SEEDS;
    let mut sum = 0u32;
    for _ in 0..32 {
        sum = sum.wrapping_add(TEA_DELTA);
        addr = addr
            .wrapping_add((value << 4).wrapping_add(s[0]) ^ value.wrapping_add(sum) ^ (value >> 5).wrapping_add(s[1]));
        value = value
            .wrapping_add((addr << 4).wrapping_add(s[2]) ^ addr.wrapping_add(sum) ^ (addr >> 5).wrapping_add(s[3]));
    }
    (addr, value)
}

fn hex(s: &str) -> Result<u32, String> {
    u32::from_str_radix(s, 16).map_err(|_| format!("'{s}' is not hexadecimal"))
}

impl Cheat {
    /// Parse cheat text (one code per line; blank lines and `#` comments
    /// are ignored).
    pub fn parse(format: CheatFormat, text: &str) -> Result<Self, String> {
        let mut lines = Vec::new();
        for raw in text.lines() {
            let l = raw.split('#').next().unwrap_or("").trim();
            if l.is_empty() {
                continue;
            }
            if format == CheatFormat::Raw {
                let (a, v) = l
                    .split_once(':')
                    .or_else(|| l.split_once(char::is_whitespace))
                    .ok_or_else(|| format!("expected ADDRESS:VALUE in '{l}'"))?;
                let v = v.trim();
                let size = match v.len() {
                    1 | 2 => 1,
                    3 | 4 => 2,
                    5..=8 => 4,
                    _ => return Err(format!("value too long in '{l}'")),
                };
                lines.push(Line { a: hex(a.trim())?, b: hex(v)?, size });
                continue;
            }
            let compact: String = l.chars().filter(|c| !c.is_whitespace()).collect();
            let (a, b) = match (format, compact.len()) {
                (CheatFormat::CodeBreaker, 12) => (hex(&compact[..8])?, hex(&compact[8..])?),
                (_, 16) => (hex(&compact[..8])?, hex(&compact[8..])?),
                _ => return Err(format!("'{l}' has the wrong length for {}", format.label())),
            };
            let (a, b) = if format == CheatFormat::GameShark { gsa_decrypt(a, b) } else { (a, b) };
            lines.push(Line { a, b, size: 0 });
        }
        if lines.is_empty() {
            return Err("no codes".into());
        }
        Ok(Self { format, lines })
    }

    /// Apply the cheat for one frame.
    pub fn apply(&self, mem: &mut dyn CheatMemory) {
        match self.format {
            CheatFormat::Raw => {
                for l in &self.lines {
                    mem.write(l.a, l.b, l.size);
                }
            }
            CheatFormat::CodeBreaker => self.apply_codebreaker(mem),
            CheatFormat::GameShark => self.apply_gameshark(mem),
            CheatFormat::ActionReplayDs => self.apply_ards(mem),
            CheatFormat::Gecko => self.apply_gecko(mem),
        }
    }

    fn apply_codebreaker(&self, mem: &mut dyn CheatMemory) {
        let mut i = 0;
        while i < self.lines.len() {
            let Line { a, b, .. } = self.lines[i];
            let addr = a & 0x0FFF_FFFF;
            let v16 = b & 0xFFFF;
            i += 1;
            let cond = |ok: bool, i: &mut usize| {
                if !ok {
                    *i += 1;
                }
            };
            match a >> 28 {
                0x0 | 0x1 | 0x9 | 0xF => {} // master/hook/seed lines
                0x2 => {
                    let cur = mem.read(addr, 2);
                    mem.write(addr, cur | v16, 2);
                }
                0x3 => mem.write(addr, b & 0xFF, 1),
                0x6 => {
                    let cur = mem.read(addr, 2);
                    mem.write(addr, cur & v16, 2);
                }
                0x7 => cond(mem.read(addr, 2) == v16, &mut i),
                0x8 => mem.write(addr, v16, 2),
                0xA => cond(mem.read(addr, 2) != v16, &mut i),
                0xB => cond(mem.read(addr, 2) > v16, &mut i),
                0xC => cond(mem.read(addr, 2) < v16, &mut i),
                0xD => {
                    // Button condition: KEYINPUT is active low.
                    let keys = !mem.read(0x0400_0130, 2) & 0x3FF;
                    cond(keys & v16 == v16, &mut i)
                }
                0xE => {
                    let cur = mem.read(addr, 2);
                    mem.write(addr, cur.wrapping_add(v16) & 0xFFFF, 2);
                }
                t => tracing::warn!("CodeBreaker code type {t:X} is not supported"),
            }
        }
    }

    fn apply_gameshark(&self, mem: &mut dyn CheatMemory) {
        let mut i = 0;
        while i < self.lines.len() {
            let Line { a, b, .. } = self.lines[i];
            let addr = a & 0x0FFF_FFFF;
            i += 1;
            match a >> 28 {
                0x0 => mem.write(addr, b & 0xFF, 1),
                0x1 => mem.write(addr, b & 0xFFFF, 2),
                0x2 => mem.write(addr, b, 4),
                0xD => {
                    if mem.read(addr, 2) != b & 0xFFFF {
                        i += 1;
                    }
                }
                0xE => {
                    // E0zzxxxx aaaaaaaa: if half[a] == xxxx run the next zz lines.
                    let count = ((a >> 16) & 0xFF) as usize;
                    if mem.read(b & 0x0FFF_FFFF, 2) != a & 0xFFFF {
                        i += count;
                    }
                }
                0xF => {} // ROM hook / master code
                t => tracing::warn!("GameShark code type {t:X} is not supported"),
            }
        }
    }

    fn apply_ards(&self, mem: &mut dyn CheatMemory) {
        let mut offset = 0u32;
        let mut data = 0u32;
        // Number of enclosing blocks whose condition failed.
        let mut skip = 0u32;
        let mut loop_start: Option<usize> = None;
        let mut loop_left = 0u32;
        let mut i = 0;
        let n = self.lines.len();
        let mut guard = 0u32;
        while i < n {
            guard += 1;
            if guard > 100_000 {
                tracing::warn!("Action Replay code exceeded its execution budget");
                break;
            }
            let Line { a, b, .. } = self.lines[i];
            i += 1;
            let ty = a >> 28;
            let sub = a >> 24;
            if skip > 0 {
                match ty {
                    0x3..=0xA | 0xC => skip += 1,
                    0xD if sub == 0xD0 || sub == 0xD1 => skip -= 1,
                    0xD if sub == 0xD2 => {
                        skip = 0;
                        offset = 0;
                        data = 0;
                    }
                    0xE => i += (b as usize).div_ceil(8),
                    _ => {}
                }
                continue;
            }
            let x = a & 0x0FFF_FFFF;
            let target = |x: u32| if x == 0 { offset } else { x };
            match ty {
                0x0 => mem.write(x.wrapping_add(offset), b, 4),
                0x1 => mem.write(x.wrapping_add(offset), b & 0xFFFF, 2),
                0x2 => mem.write(x.wrapping_add(offset), b & 0xFF, 1),
                0x3..=0x6 => {
                    let v = mem.read(target(x), 4);
                    let ok = match ty {
                        0x3 => b > v,
                        0x4 => b < v,
                        0x5 => b == v,
                        _ => b != v,
                    };
                    if !ok {
                        skip = 1;
                    }
                }
                0x7..=0xA => {
                    let mask = b >> 16;
                    let z = b & 0xFFFF;
                    let v = !mask & mem.read(target(x), 2) & 0xFFFF;
                    let ok = match ty {
                        0x7 => z > v,
                        0x8 => z < v,
                        0x9 => z == v,
                        _ => z != v,
                    };
                    if !ok {
                        skip = 1;
                    }
                }
                0xB => offset = mem.read(x.wrapping_add(offset), 4),
                0xC => {
                    loop_start = Some(i);
                    loop_left = b;
                }
                0xD => match sub {
                    0xD0 => {}
                    0xD1 | 0xD2 => {
                        if let Some(start) = loop_start {
                            if loop_left > 0 {
                                loop_left -= 1;
                                i = start;
                                continue;
                            }
                            loop_start = None;
                        }
                        if sub == 0xD2 {
                            offset = 0;
                            data = 0;
                        }
                    }
                    0xD3 => offset = b,
                    0xD4 => data = data.wrapping_add(b),
                    0xD5 => data = b,
                    0xD6 => {
                        mem.write(b.wrapping_add(offset), data, 4);
                        offset = offset.wrapping_add(4);
                    }
                    0xD7 => {
                        mem.write(b.wrapping_add(offset), data & 0xFFFF, 2);
                        offset = offset.wrapping_add(2);
                    }
                    0xD8 => {
                        mem.write(b.wrapping_add(offset), data & 0xFF, 1);
                        offset = offset.wrapping_add(1);
                    }
                    0xD9 => data = mem.read(b.wrapping_add(offset), 4),
                    0xDA => data = mem.read(b.wrapping_add(offset), 2),
                    0xDB => data = mem.read(b.wrapping_add(offset), 1),
                    0xDC => offset = offset.wrapping_add(b),
                    s => tracing::warn!("Action Replay code type {s:02X} is not supported"),
                },
                0xE => {
                    // Patch b bytes from the following lines to x + offset.
                    let dst = x.wrapping_add(offset);
                    let mut bytes = Vec::with_capacity(b as usize);
                    let words = (b as usize).div_ceil(8);
                    for l in self.lines.iter().skip(i).take(words) {
                        bytes.extend_from_slice(&l.a.to_le_bytes());
                        bytes.extend_from_slice(&l.b.to_le_bytes());
                    }
                    for (k, &byte) in bytes.iter().take(b as usize).enumerate() {
                        mem.write(dst.wrapping_add(k as u32), byte as u32, 1);
                    }
                    i += words;
                }
                _ => {
                    // F: copy b bytes from [offset] to x.
                    for k in 0..b.min(0x10000) {
                        let v = mem.read(offset.wrapping_add(k), 1);
                        mem.write(x.wrapping_add(k), v, 1);
                    }
                }
            }
        }
    }

    fn apply_gecko(&self, mem: &mut dyn CheatMemory) {
        let mut ba = 0x8000_0000u32;
        let mut po = 0x8000_0000u32;
        let mut skip = 0u32;
        let mut i = 0;
        let n = self.lines.len();
        while i < n {
            let Line { a, b, .. } = self.lines[i];
            i += 1;
            let ty = (a >> 24) & 0xEE; // drop the pointer bit (0x10) and the endif bit
            let base = if a & 0x1000_0000 != 0 { po } else { ba };
            let addr = base.wrapping_add(a & 0x01FF_FFFF);
            if skip > 0 {
                match a >> 24 {
                    0x20..=0x3F if a & 1 != 0 && skip == 1 => skip = 0, // endif closes the failed block; evaluate
                    0x20..=0x3F if a & 1 == 0 => {
                        skip += 1;
                        continue;
                    }
                    0xE2 => {
                        skip = skip.saturating_sub((a & 0xFF).max(1));
                        continue;
                    }
                    0xE0 => skip = 0,
                    0x06 | 0x16 => {
                        i += (b as usize).div_ceil(8);
                        continue;
                    }
                    0x08 | 0x18 => {
                        i += 1;
                        continue;
                    }
                    _ => continue,
                }
            }
            match ty {
                0x00 => {
                    let count = (b >> 16) + 1;
                    for k in 0..count {
                        mem.write(addr.wrapping_add(k), b & 0xFF, 1);
                    }
                }
                0x02 => {
                    let count = (b >> 16) + 1;
                    for k in 0..count {
                        mem.write(addr.wrapping_add(k * 2), b & 0xFFFF, 2);
                    }
                }
                0x04 => mem.write(addr & !3, b, 4),
                0x06 => {
                    let words = (b as usize).div_ceil(8);
                    let mut bytes = Vec::with_capacity(words * 8);
                    for l in self.lines.iter().skip(i).take(words) {
                        bytes.extend_from_slice(&l.a.to_be_bytes());
                        bytes.extend_from_slice(&l.b.to_be_bytes());
                    }
                    for (k, &byte) in bytes.iter().take(b as usize).enumerate() {
                        mem.write(addr.wrapping_add(k as u32), byte as u32, 1);
                    }
                    i += words;
                }
                0x08 => {
                    // 08XXXXXX YYYYYYYY / TNNNZZZZ VVVVVVVV: serial write.
                    let Some(next) = self.lines.get(i) else { break };
                    i += 1;
                    let size = [1u8, 2, 4][((next.a >> 28) & 3).min(2) as usize];
                    let count = ((next.a >> 16) & 0xFFF) + 1;
                    let step = next.a & 0xFFFF;
                    let (mut at, mut v) = (addr, b);
                    for _ in 0..count {
                        mem.write(at, v, size);
                        at = at.wrapping_add(step);
                        v = v.wrapping_add(next.b);
                    }
                }
                0x20 | 0x22 | 0x24 | 0x26 | 0x28 | 0x2A | 0x2C | 0x2E => {
                    // (A set lowest address bit means "endif, then if"; with
                    // nothing skipped there is no open block to close.)
                    let a2 = addr & !1;
                    let ok = if ty < 0x28 {
                        let v = mem.read(a2 & !3, 4);
                        match ty {
                            0x20 => v == b,
                            0x22 => v != b,
                            0x24 => v > b,
                            _ => v < b,
                        }
                    } else {
                        let v = mem.read(a2 & !1, 2) & !(b >> 16) & 0xFFFF;
                        let y = b & 0xFFFF;
                        match ty {
                            0x28 => v == y,
                            0x2A => v != y,
                            0x2C => v > y,
                            _ => v < y,
                        }
                    };
                    if !ok {
                        skip = 1;
                    }
                }
                0x42 if a >> 24 == 0x42 => ba = b,
                0x4A if a >> 24 == 0x4A => po = b,
                0xE0 | 0xE2 if a >> 24 == ty => {
                    if b >> 16 != 0 {
                        ba = b & 0xFFFF_0000;
                    }
                    if b & 0xFFFF != 0 {
                        po = (b & 0xFFFF) << 16;
                    }
                }
                _ => tracing::warn!("Gecko code type {:02X} is not supported", a >> 24),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    struct Le(HashMap<u32, u8>);

    impl CheatMemory for Le {
        fn read(&mut self, addr: u32, size: u8) -> u32 {
            (0..size as u32).fold(0, |acc, k| acc | (*self.0.get(&(addr + k)).unwrap_or(&0) as u32) << (8 * k))
        }
        fn write(&mut self, addr: u32, value: u32, size: u8) {
            for k in 0..size as u32 {
                self.0.insert(addr + k, (value >> (8 * k)) as u8);
            }
        }
    }

    #[derive(Default)]
    struct Be(HashMap<u32, u8>);

    impl CheatMemory for Be {
        fn read(&mut self, addr: u32, size: u8) -> u32 {
            (0..size as u32).fold(0, |acc, k| acc << 8 | *self.0.get(&(addr + k)).unwrap_or(&0) as u32)
        }
        fn write(&mut self, addr: u32, value: u32, size: u8) {
            for k in 0..size as u32 {
                self.0.insert(addr + k, (value >> (8 * (size as u32 - 1 - k))) as u8);
            }
        }
    }

    #[test]
    fn raw_codes_pick_width_from_digits() {
        let c = Cheat::parse(CheatFormat::Raw, "02000000:12\n02000010:3456  # comment\n02000020 789ABCDE").unwrap();
        let mut m = Le::default();
        c.apply(&mut m);
        assert_eq!(m.read(0x0200_0000, 2), 0x12);
        assert_eq!(m.read(0x0200_0010, 4), 0x3456);
        assert_eq!(m.read(0x0200_0020, 4), 0x789A_BCDE);
        assert!(Cheat::parse(CheatFormat::Raw, "zz:1").is_err());
        assert!(Cheat::parse(CheatFormat::Raw, "   \n# only comments").is_err());
    }

    #[test]
    fn gameshark_tea_round_trip_and_decoded_writes() {
        for (a, b) in [(0x0200_1234u32, 0x0000_0063u32), (0xDEAD_BEEF, 0x0123_4567)] {
            let (ea, eb) = gsa_encrypt(a, b);
            assert_ne!((ea, eb), (a, b));
            assert_eq!(gsa_decrypt(ea, eb), (a, b));
        }
        let (a1, b1) = gsa_encrypt(0x1300_0040, 0x0000_03E7); // 16-bit write of 999
        let text = format!("{a1:08X} {b1:08X}");
        let c = Cheat::parse(CheatFormat::GameShark, &text).unwrap();
        let mut m = Le::default();
        c.apply(&mut m);
        assert_eq!(m.read(0x0300_0040, 2), 999);
    }

    #[test]
    fn codebreaker_writes_and_conditions() {
        let text = "82000000 1234\n33000010 0099\n72000000 1234\n82000002 0001\n72000000 FFFF\n82000004 0002";
        let c = Cheat::parse(CheatFormat::CodeBreaker, text).unwrap();
        let mut m = Le::default();
        c.apply(&mut m);
        assert_eq!(m.read(0x0200_0000, 2), 0x1234);
        assert_eq!(m.read(0x0300_0010, 1), 0x99);
        assert_eq!(m.read(0x0200_0002, 2), 1, "condition true → next line runs");
        assert_eq!(m.read(0x0200_0004, 2), 0, "condition false → next line skipped");
    }

    #[test]
    fn action_replay_ds_offsets_conditions_and_loops() {
        let text = "\
            0213F000 0000270F\n\
            D3000000 02100000\n\
            10000010 00000063\n\
            5213F000 0000270F\n\
            20000020 00000001\n\
            D0000000 00000000\n\
            5213F000 00000000\n\
            20000021 00000001\n\
            D2000000 00000000\n\
            C0000000 00000003\n\
            D8000000 02200000\n\
            D2000000 00000000\n\
            D5000000 000000AB\n\
            D3000000 02300000\n\
            D8000000 00000000\n\
            D2000000 00000000";
        let c = Cheat::parse(CheatFormat::ActionReplayDs, text).unwrap();
        let mut m = Le::default();
        c.apply(&mut m);
        assert_eq!(m.read(0x0213_F000, 4), 9999);
        assert_eq!(m.read(0x0210_0010, 2), 0x63, "offset applied");
        assert_eq!(m.read(0x0210_0020, 1), 1, "true condition ran");
        assert_eq!(m.read(0x0210_0021, 1), 0, "false condition skipped until D2");
        assert_eq!(m.read(0x0230_0000, 1), 0xAB, "data register store");
    }

    #[test]
    fn action_replay_ds_patch_code() {
        let text = "E2000000 0000000A\n44332211 88776655\nAAAA0A09 00000000";
        let c = Cheat::parse(CheatFormat::ActionReplayDs, text).unwrap();
        let mut m = Le::default();
        c.apply(&mut m);
        let bytes: Vec<u32> = (0..10).map(|k| m.read(0x0200_0000 + k, 1)).collect();
        assert_eq!(bytes, vec![0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x09, 0x0A]);
        assert_eq!(m.read(0x0200_000A, 1), 0, "only b bytes written");
    }

    #[test]
    fn gecko_writes_and_conditionals_big_endian() {
        let text = "\
            04001000 12345678\n\
            02001010 0001ABCD\n\
            00001020 000200EE\n\
            20001000 12345678\n\
            04001030 00000001\n\
            E2000001 00000000\n\
            20001000 00000000\n\
            04001034 00000001\n\
            E0000000 80008000";
        let c = Cheat::parse(CheatFormat::Gecko, text).unwrap();
        let mut m = Be::default();
        c.apply(&mut m);
        assert_eq!(m.read(0x8000_1000, 4), 0x1234_5678);
        assert_eq!(m.read(0x8000_1010, 4), 0xABCD_ABCD, "16-bit write repeated twice");
        assert_eq!(m.read(0x8000_1020, 3), 0xEEEEEE, "8-bit write repeated 3 times");
        assert_eq!(m.read(0x8000_1030, 4), 1, "true conditional");
        assert_eq!(m.read(0x8000_1034, 4), 0, "false conditional skipped");
    }

    #[test]
    fn formats_round_trip_ids() {
        for f in [
            CheatFormat::Raw,
            CheatFormat::CodeBreaker,
            CheatFormat::GameShark,
            CheatFormat::ActionReplayDs,
            CheatFormat::Gecko,
        ] {
            assert_eq!(CheatFormat::from_id(f.id()), Some(f));
        }
        assert_eq!(CheatFormat::for_system(System::Nds)[0], CheatFormat::ActionReplayDs);
    }
}
