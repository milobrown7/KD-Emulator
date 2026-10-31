//! High-level emulation of the GBA BIOS.
//!
//! No Nintendo firmware is required. The 16 KiB BIOS region contains a small
//! stub assembled at start-up (exception vectors plus the standard IRQ
//! dispatcher that calls the handler pointer at `0x03007FFC`); every `SWI`
//! is intercepted and implemented here.

use crate::bus::GbaBus;
use cpu_arm::asm::{Assembler, AL};
use cpu_arm::{Access, Arm, Bus, SwiOutcome, MODE_IRQ, MODE_SVC, MODE_SYS};

/// Mirror of `IF` that IRQ handlers acknowledge into (`0x03007FF8`).
pub const BIOS_IF: u32 = 0x0300_7FF8;

/// Assemble the replacement BIOS image.
pub fn stub_image() -> Vec<u8> {
    let mut a = Assembler::new(0);
    a.b_abs(AL, 0xE0); // 0x00 reset → boot stub
    a.word(0xE1B0_F00E); // 0x04 undefined: movs pc, lr
    a.word(0xE1B0_F00E); // 0x08 swi (HLE intercepts; never reached)
    a.word(0xE25E_F004); // 0x0C prefetch abort: subs pc, lr, #4
    a.word(0xE25E_F008); // 0x10 data abort: subs pc, lr, #8
    a.b_abs(AL, 0x14); // 0x14 reserved
    a.b_abs(AL, 0x128); // 0x18 IRQ
    a.word(0xE25E_F004); // 0x1C FIQ
    let mut image = a.finish();
    image.resize(0x4000, 0);

    // Reset: jump to the cartridge.
    let mut boot = Assembler::new(0xE0);
    boot.load_const(0, 0x0800_0000).bx(0);
    put(&mut image, 0xE0, &boot.finish());

    // IRQ dispatcher, same instruction sequence and address as the real BIOS.
    let mut irq = Assembler::new(0x128);
    irq.push(0b0101_0000_0000_1111); // stmfd sp!, {r0-r3, r12, lr}
    irq.mov_imm(0, 0x0400_0000);
    irq.word(0xE28F_E000); // add lr, pc, #0
    irq.word(0xE510_F004); // ldr pc, [r0, #-4]
    irq.pop(0b0101_0000_0000_1111); // ldmfd sp!, {r0-r3, r12, lr}
    irq.word(0xE25E_F004); // subs pc, lr, #4
    put(&mut image, 0x128, &irq.finish());
    image
}

fn put(image: &mut [u8], at: usize, code: &[u8]) {
    image[at..at + code.len()].copy_from_slice(code);
}

fn r8(bus: &mut GbaBus, a: u32) -> u8 {
    bus.read8(a, Access::Seq)
}
fn r16(bus: &mut GbaBus, a: u32) -> u16 {
    bus.read16(a & !1, Access::Seq)
}
fn r32(bus: &mut GbaBus, a: u32) -> u32 {
    bus.read32(a & !3, Access::Seq)
}
fn w16(bus: &mut GbaBus, a: u32, v: u16) {
    bus.write16(a & !1, v, Access::Seq)
}
fn w32(bus: &mut GbaBus, a: u32, v: u32) {
    bus.write32(a & !3, v, Access::Seq)
}

pub(crate) fn swi(bus: &mut GbaBus, cpu: &mut Arm, n: u8) -> SwiOutcome {
    match n {
        0x00 => soft_reset(bus, cpu),
        0x01 => register_ram_reset(bus, cpu.r[0]),
        0x02 | 0x03 => {
            bus.halt_request = true;
        }
        0x04 => return intr_wait(bus, cpu, cpu.r[0] != 0, cpu.r[1] as u16),
        0x05 => {
            cpu.r[0] = 1;
            cpu.r[1] = 1;
            return intr_wait(bus, cpu, true, 1);
        }
        0x06 => div(cpu, cpu.r[0] as i32, cpu.r[1] as i32),
        0x07 => div(cpu, cpu.r[1] as i32, cpu.r[0] as i32),
        0x08 => cpu.r[0] = isqrt(cpu.r[0]),
        0x09 => cpu.r[0] = arctan(cpu.r[0] as i16 as i32) as u16 as u32,
        0x0A => cpu.r[0] = arctan2(cpu.r[0] as i16 as i32, cpu.r[1] as i16 as i32) as u16 as u32,
        0x0B => cpu_set(bus, cpu.r[0], cpu.r[1], cpu.r[2]),
        0x0C => cpu_fast_set(bus, cpu.r[0], cpu.r[1], cpu.r[2]),
        0x0D => cpu.r[0] = 0xBAAE_187F, // checksum of the original GBA BIOS
        0x0E => bg_affine_set(bus, cpu.r[0], cpu.r[1], cpu.r[2]),
        0x0F => obj_affine_set(bus, cpu.r[0], cpu.r[1], cpu.r[2], cpu.r[3]),
        0x10 => bit_unpack(bus, cpu.r[0], cpu.r[1], cpu.r[2]),
        0x11 => lz77(bus, cpu.r[0], cpu.r[1], false),
        0x12 => lz77(bus, cpu.r[0], cpu.r[1], true),
        0x13 => huffman(bus, cpu.r[0], cpu.r[1]),
        0x14 => rle(bus, cpu.r[0], cpu.r[1], false),
        0x15 => rle(bus, cpu.r[0], cpu.r[1], true),
        0x16 => diff8(bus, cpu.r[0], cpu.r[1], false),
        0x17 => diff8(bus, cpu.r[0], cpu.r[1], true),
        0x18 => diff16(bus, cpu.r[0], cpu.r[1]),
        0x19 => {} // SoundBias: ramp is instantaneous here
        0x1F => cpu.r[0] = midi_key_to_freq(bus, cpu.r[0], cpu.r[1] as u8, cpu.r[2] as u8),
        0x25 => cpu.r[0] = 1, // MultiBoot: no link partner → failure
        0x1A..=0x1E | 0x20..=0x24 | 0x26..=0x2A => {} // sound driver hooks: games ship their own
        _ => tracing::warn!("unimplemented BIOS SWI {n:#04x}"),
    }
    SwiOutcome::Handled
}

fn soft_reset(bus: &mut GbaBus, cpu: &mut Arm) {
    let from_ewram = bus.iwram[0x7FFA] != 0;
    for b in &mut bus.iwram[0x7E00..] {
        *b = 0;
    }
    let entry = if from_ewram { 0x0200_0000 } else { 0x0800_0000 };
    init_stacks(cpu);
    cpu.r[..13].fill(0);
    cpu.r[14] = entry;
    cpu.boot(bus, entry, MODE_SYS, false);
}

/// Stack pointers the BIOS sets up before entering the game.
pub fn init_stacks(cpu: &mut Arm) {
    cpu.set_banked_sp(MODE_SVC, 0x0300_7FE0);
    cpu.set_banked_sp(MODE_IRQ, 0x0300_7FA0);
    cpu.set_banked_sp(MODE_SYS, 0x0300_7F00);
}

fn register_ram_reset(bus: &mut GbaBus, flags: u32) {
    if flags & 0x01 != 0 {
        bus.ewram.fill(0);
    }
    if flags & 0x02 != 0 {
        bus.iwram[..0x7E00].fill(0);
    }
    if flags & 0x04 != 0 {
        bus.ppu.palette.fill(0);
    }
    if flags & 0x08 != 0 {
        bus.ppu.vram.fill(0);
    }
    if flags & 0x10 != 0 {
        bus.ppu.oam.fill(0);
    }
    if flags & 0x20 != 0 {
        for off in (0x120..0x160).step_by(2) {
            bus.io_write16(off, 0);
        }
        bus.io_write16(0x134, 0x8000);
    }
    if flags & 0x40 != 0 {
        for off in (0x60..0xA8).step_by(2) {
            bus.io_write16(off, 0);
        }
    }
    if flags & 0x80 != 0 {
        for off in (0x00..0x60).step_by(2) {
            bus.io_write16(off, 0);
        }
        bus.io_write16(0x00, 0x0080);
        for off in (0xB0..0x110).step_by(2) {
            bus.io_write16(off, 0);
        }
        bus.io_write16(0x204, 0);
        bus.io_write16(0x200, 0);
    }
}

/// IntrWait: `discard` drops already-pending flags first, then the CPU
/// halts until one of `mask` is acknowledged in `BIOS_IF`. The SWI is
/// re-executed after every interrupt; `bus.intr_wait_active` tells a repeat
/// apart from a fresh call so flags are discarded only once.
fn intr_wait(bus: &mut GbaBus, _cpu: &mut Arm, discard: bool, mask: u16) -> SwiOutcome {
    bus.ime = true;
    let bios_if = r16(bus, BIOS_IF);
    if discard && !bus.intr_wait_active {
        w16(bus, BIOS_IF, bios_if & !mask);
    } else if bios_if & mask != 0 {
        w16(bus, BIOS_IF, bios_if & !mask);
        bus.intr_wait_active = false;
        return SwiOutcome::Handled;
    }
    bus.intr_wait_active = true;
    SwiOutcome::HaltAndRepeat
}

fn div(cpu: &mut Arm, num: i32, den: i32) {
    if den == 0 {
        // The real BIOS loops forever; return something sane instead.
        tracing::warn!("BIOS Div by zero");
        cpu.r[0] = if num < 0 { u32::MAX } else { 1 };
        cpu.r[1] = num as u32;
        cpu.r[3] = 1;
        return;
    }
    let q = num.wrapping_div(den);
    let r = num.wrapping_rem(den);
    cpu.r[0] = q as u32;
    cpu.r[1] = r as u32;
    cpu.r[3] = q.unsigned_abs();
}

pub fn isqrt(v: u32) -> u32 {
    if v == 0 {
        return 0;
    }
    let mut x = (v as f64).sqrt() as u32;
    while (x as u64) * (x as u64) > v as u64 {
        x -= 1;
    }
    while ((x + 1) as u64) * ((x + 1) as u64) <= v as u64 {
        x += 1;
    }
    x
}

/// BIOS ArcTan polynomial (input and output in 1.14 fixed point).
pub fn arctan(i: i32) -> i32 {
    let a = -((i.wrapping_mul(i)) >> 14);
    let mut b = ((0xA9 * a) >> 14) + 0x390;
    for k in [0x91C, 0xFB6, 0x16AA, 0x2081, 0x3651, 0xA2F9] {
        b = ((b.wrapping_mul(a)) >> 14) + k;
    }
    (i.wrapping_mul(b)) >> 16
}

/// BIOS ArcTan2: full-circle angle 0..0xFFFF of the vector (x, y).
pub fn arctan2(x: i32, y: i32) -> i32 {
    if y == 0 {
        return if x >= 0 { 0 } else { 0x8000 };
    }
    if x == 0 {
        return if y >= 0 { 0x4000 } else { 0xC000 };
    }
    if y >= 0 {
        if x >= 0 {
            if x >= y {
                return arctan((y << 14) / x);
            }
        } else if -x >= y {
            return arctan((y << 14) / x) + 0x8000;
        }
        0x4000 - arctan((x << 14) / y)
    } else {
        if x <= 0 {
            if -x > -y {
                return arctan((y << 14) / x) + 0x8000;
            }
        } else if x >= -y {
            return arctan((y << 14) / x) + 0x10000;
        }
        0xC000 - arctan((x << 14) / y)
    }
}

fn cpu_set(bus: &mut GbaBus, mut src: u32, mut dst: u32, ctrl: u32) {
    let count = ctrl & 0x1F_FFFF;
    let fill = ctrl & (1 << 24) != 0;
    if ctrl & (1 << 26) != 0 {
        let v = r32(bus, src);
        for _ in 0..count {
            let val = if fill { v } else { r32(bus, src) };
            w32(bus, dst, val);
            if !fill {
                src += 4;
            }
            dst += 4;
        }
    } else {
        let v = r16(bus, src);
        for _ in 0..count {
            let val = if fill { v } else { r16(bus, src) };
            w16(bus, dst, val);
            if !fill {
                src += 2;
            }
            dst += 2;
        }
    }
}

fn cpu_fast_set(bus: &mut GbaBus, mut src: u32, mut dst: u32, ctrl: u32) {
    let count = (ctrl & 0x1F_FFFF).div_ceil(8) * 8;
    let fill = ctrl & (1 << 24) != 0;
    let v = r32(bus, src);
    for _ in 0..count {
        let val = if fill { v } else { r32(bus, src) };
        w32(bus, dst, val);
        if !fill {
            src += 4;
        }
        dst += 4;
    }
}

fn sin_cos(angle: u16) -> (i32, i32) {
    let theta = (angle >> 8) as f64 / 128.0 * std::f64::consts::PI;
    ((theta.sin() * 16384.0).round() as i32, (theta.cos() * 16384.0).round() as i32)
}

fn bg_affine_set(bus: &mut GbaBus, mut src: u32, mut dst: u32, count: u32) {
    for _ in 0..count {
        let ox = r32(bus, src) as i32 as i64;
        let oy = r32(bus, src + 4) as i32 as i64;
        let cx = r16(bus, src + 8) as i16 as i64;
        let cy = r16(bus, src + 10) as i16 as i64;
        let sx = r16(bus, src + 12) as i16 as i64;
        let sy = r16(bus, src + 14) as i16 as i64;
        let (sin, cos) = sin_cos(r16(bus, src + 16));
        let (sin, cos) = (sin as i64, cos as i64);
        let pa = (sx * cos) >> 14;
        let pb = -(sx * sin) >> 14;
        let pc = (sy * sin) >> 14;
        let pd = (sy * cos) >> 14;
        w16(bus, dst, pa as u16);
        w16(bus, dst + 2, pb as u16);
        w16(bus, dst + 4, pc as u16);
        w16(bus, dst + 6, pd as u16);
        w32(bus, dst + 8, (ox - (pa * cx + pb * cy)) as u32);
        w32(bus, dst + 12, (oy - (pc * cx + pd * cy)) as u32);
        src += 20;
        dst += 16;
    }
}

fn obj_affine_set(bus: &mut GbaBus, mut src: u32, mut dst: u32, count: u32, stride: u32) {
    for _ in 0..count {
        let sx = r16(bus, src) as i16 as i32;
        let sy = r16(bus, src + 2) as i16 as i32;
        let (sin, cos) = sin_cos(r16(bus, src + 4));
        let vals = [(sx * cos) >> 14, -(sx * sin) >> 14, (sy * sin) >> 14, (sy * cos) >> 14];
        for v in vals {
            w16(bus, dst, v as u16);
            dst += stride;
        }
        src += 8;
    }
}

fn bit_unpack(bus: &mut GbaBus, mut src: u32, mut dst: u32, info: u32) {
    let len = r16(bus, info) as u32;
    let src_w = r8(bus, info + 2) as u32;
    let dst_w = r8(bus, info + 3) as u32;
    let data = r32(bus, info + 4);
    let offset = data & 0x7FFF_FFFF;
    let zero_flag = data & 0x8000_0000 != 0;
    if ![1, 2, 4, 8].contains(&src_w) || ![1, 2, 4, 8, 16, 32].contains(&dst_w) {
        return;
    }
    let mut out = 0u32;
    let mut out_bits = 0;
    for _ in 0..len {
        let byte = r8(bus, src) as u32;
        src += 1;
        let mut bit = 0;
        while bit < 8 {
            let v = (byte >> bit) & ((1 << src_w) - 1);
            let v = if v != 0 || zero_flag { v + offset } else { 0 };
            out |= (v & (((1u64 << dst_w) - 1) as u32)) << out_bits;
            out_bits += dst_w;
            if out_bits >= 32 {
                w32(bus, dst, out);
                dst += 4;
                out = 0;
                out_bits = 0;
            }
            bit += src_w;
        }
    }
}

/// Byte sink for the decompressors; the VRAM variants must write halfwords.
/// Keeps a history buffer for LZ77 back-references so VRAM output never has
/// to be read back.
struct Sink {
    dst: u32,
    vram: bool,
    history: Vec<u8>,
}

impl Sink {
    fn new(dst: u32, vram: bool) -> Self {
        Self { dst, vram, history: Vec::new() }
    }
    fn written(&self) -> u32 {
        self.history.len() as u32
    }
    fn put(&mut self, bus: &mut GbaBus, b: u8) {
        self.history.push(b);
        if !self.vram {
            bus.write8(self.dst, b, Access::Seq);
            self.dst += 1;
        } else if self.history.len() % 2 == 0 {
            let lo = self.history[self.history.len() - 2];
            w16(bus, self.dst, lo as u16 | (b as u16) << 8);
            self.dst += 2;
        }
    }
    fn peek_back(&self, back: u32) -> u8 {
        let len = self.history.len();
        if back as usize > len {
            0
        } else {
            self.history[len - back as usize]
        }
    }
}

fn lz77(bus: &mut GbaBus, mut src: u32, dst: u32, vram: bool) {
    let header = r32(bus, src);
    let size = header >> 8;
    src += 4;
    let mut out = Sink::new(dst, vram);
    while out.written() < size {
        let flags = r8(bus, src);
        src += 1;
        for i in (0..8).rev() {
            if out.written() >= size {
                break;
            }
            if flags & (1 << i) == 0 {
                let b = r8(bus, src);
                src += 1;
                out.put(bus, b);
            } else {
                let b0 = r8(bus, src) as u32;
                let b1 = r8(bus, src + 1) as u32;
                src += 2;
                let len = (b0 >> 4) + 3;
                let disp = (((b0 & 0xF) << 8) | b1) + 1;
                for _ in 0..len {
                    if out.written() >= size {
                        break;
                    }
                    let b = out.peek_back(disp);
                    out.put(bus, b);
                }
            }
        }
    }
}

fn rle(bus: &mut GbaBus, mut src: u32, dst: u32, vram: bool) {
    let size = r32(bus, src) >> 8;
    src += 4;
    let mut out = Sink::new(dst, vram);
    while out.written() < size {
        let flag = r8(bus, src);
        src += 1;
        if flag & 0x80 != 0 {
            let len = (flag & 0x7F) as u32 + 3;
            let b = r8(bus, src);
            src += 1;
            for _ in 0..len {
                if out.written() < size {
                    out.put(bus, b);
                }
            }
        } else {
            for _ in 0..(flag as u32 + 1) {
                let b = r8(bus, src);
                src += 1;
                if out.written() < size {
                    out.put(bus, b);
                }
            }
        }
    }
}

fn huffman(bus: &mut GbaBus, src: u32, mut dst: u32) {
    let header = r32(bus, src);
    let bits = header & 0xF;
    let size = header >> 8;
    if bits != 4 && bits != 8 {
        return;
    }
    let tree_size = (r8(bus, src + 4) as u32 + 1) * 2;
    let root = src + 5;
    let mut stream = src + 4 + tree_size;
    let mut out_word = 0u32;
    let mut out_bits = 0;
    let mut written = 0;
    let mut node_addr = root;
    let mut node = r8(bus, node_addr);
    'outer: while written < size {
        let word = r32(bus, stream);
        stream += 4;
        for i in (0..32).rev() {
            let bit = (word >> i) & 1;
            let offset = (node & 0x3F) as u32;
            let next = (node_addr & !1) + offset * 2 + 2 + bit;
            let is_leaf = node & (if bit == 0 { 0x80 } else { 0x40 }) != 0;
            if is_leaf {
                let value = r8(bus, next) as u32;
                out_word |= value << out_bits;
                out_bits += bits;
                if out_bits == 32 {
                    w32(bus, dst, out_word);
                    dst += 4;
                    written += 4;
                    out_word = 0;
                    out_bits = 0;
                    if written >= size {
                        break 'outer;
                    }
                }
                node_addr = root;
                node = r8(bus, root);
            } else {
                node_addr = next;
                node = r8(bus, next);
            }
        }
    }
}

fn diff8(bus: &mut GbaBus, mut src: u32, dst: u32, vram: bool) {
    let size = r32(bus, src) >> 8;
    src += 4;
    let mut out = Sink::new(dst, vram);
    let mut acc = 0u8;
    while out.written() < size {
        acc = acc.wrapping_add(r8(bus, src));
        src += 1;
        out.put(bus, acc);
    }
}

fn diff16(bus: &mut GbaBus, mut src: u32, mut dst: u32) {
    let size = r32(bus, src) >> 8;
    src += 4;
    let mut acc = 0u16;
    let mut done = 0;
    while done < size {
        acc = acc.wrapping_add(r16(bus, src));
        src += 2;
        w16(bus, dst, acc);
        dst += 2;
        done += 2;
    }
}

/// MidiKey2Freq: `freq = WaveData.freq / 2^((180 - key - fine/256) / 12)`.
fn midi_key_to_freq(bus: &mut GbaBus, wave: u32, key: u8, fine: u8) -> u32 {
    let base = r32(bus, wave + 4) as f64;
    let exp = (180.0 - key as f64 - fine as f64 / 256.0) / 12.0;
    (base / 2f64.powf(exp)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arctan2_quadrants() {
        assert_eq!(arctan2(1, 0), 0);
        assert_eq!(arctan2(0, 1), 0x4000);
        assert_eq!(arctan2(-1, 0), 0x8000);
        assert_eq!(arctan2(0, -1), 0xC000);
        let a = arctan2(100, 100);
        assert!((a - 0x2000).abs() < 8, "45° → {a:#x}");
        let b = arctan2(-100, -100);
        assert!((b - 0xA000).abs() < 8, "225° → {b:#x}");
    }

    #[test]
    fn sqrt_is_exact_floor() {
        for v in [0u32, 1, 2, 3, 4, 15, 16, 17, 1 << 20, u32::MAX] {
            let s = isqrt(v) as u64;
            assert!(s * s <= v as u64 && (s + 1) * (s + 1) > v as u64, "{v}");
        }
    }

    #[test]
    fn stub_has_irq_dispatcher() {
        let img = stub_image();
        let word = |a: usize| u32::from_le_bytes(img[a..a + 4].try_into().unwrap());
        assert_eq!(word(0x128), 0xE92D_500F);
        assert_eq!(word(0x134), 0xE510_F004);
        assert_eq!(word(0x13C), 0xE25E_F004);
        assert_eq!(word(0x18) >> 24, 0xEA, "IRQ vector is a branch");
    }
}
