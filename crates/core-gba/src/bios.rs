//! High-level emulation of the GBA BIOS.
//!
//! No Nintendo firmware is required. The 16 KiB BIOS region contains a small
//! stub assembled at start-up (exception vectors plus the standard IRQ
//! dispatcher that calls the handler pointer at `0x03007FFC`); every `SWI`
//! is intercepted and implemented here.

use crate::bus::GbaBus;
use cpu_arm::asm::{Assembler, AL};
use cpu_arm::hle::*;
use cpu_arm::{Arm, SwiOutcome, MODE_IRQ, MODE_SVC, MODE_SYS};

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
        0x25 => cpu.r[0] = 1,                         // MultiBoot: no link partner → failure
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

#[cfg(test)]
mod tests {
    use super::*;

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
