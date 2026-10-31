//! High-level BIOS for both DS CPUs. The BIOS regions hold tiny assembled
//! stubs (exception vectors + the IRQ dispatchers) instead of Nintendo
//! firmware; every SWI is implemented here.

use crate::bus::{Bus7, Bus9, ARM7, ARM9};
use cpu_arm::asm::{Assembler, AL};
use cpu_arm::hle::*;
use cpu_arm::{Arm, Bus, SwiOutcome};

/// ARM9 BIOS at 0xFFFF0000. The IRQ handler pointer lives at
/// DTCM + 0x3FFC (read through CP15 c9).
pub fn stub9() -> Vec<u8> {
    let mut a = Assembler::new(0xFFFF_0000);
    a.b_abs(AL, 0xFFFF_0000); // reset (unused with direct boot)
    a.word(0xE1B0_F00E); // undefined: movs pc, lr
    a.word(0xE1B0_F00E); // swi (HLE)
    a.word(0xE25E_F004); // prefetch abort
    a.word(0xE25E_F008); // data abort
    a.b_abs(AL, 0xFFFF_0014);
    a.b_abs(AL, 0xFFFF_0100); // IRQ
    a.word(0xE25E_F004); // FIQ
    let mut img = a.finish();
    img.resize(0x8000, 0);
    let mut irq = Assembler::new(0xFFFF_0100);
    irq.push(0b0101_0000_0000_1111); // stmfd sp!, {r0-r3, r12, lr}
    irq.word(0xEE19_0F11); // mrc p15, 0, r0, c9, c1, 0 (DTCM base)
    irq.word(0xE1A0_0620); // mov r0, r0, lsr #12
    irq.word(0xE1A0_0600); // mov r0, r0, lsl #12
    irq.add_imm(0, 0, 0x4000);
    irq.word(0xE28F_E000); // add lr, pc, #0
    irq.word(0xE510_F004); // ldr pc, [r0, #-4]
    irq.pop(0b0101_0000_0000_1111);
    irq.word(0xE25E_F004); // subs pc, lr, #4
    let code = irq.finish();
    img[0x100..0x100 + code.len()].copy_from_slice(&code);
    img
}

/// ARM7 BIOS at 0x00000000; handler pointer at 0x0380FFFC (mirrored at
/// 0x03FFFFFC, reached as [0x04000000 - 4]).
pub fn stub7() -> Vec<u8> {
    let mut a = Assembler::new(0);
    a.b_abs(AL, 0);
    a.word(0xE1B0_F00E);
    a.word(0xE1B0_F00E);
    a.word(0xE25E_F004);
    a.word(0xE25E_F008);
    a.b_abs(AL, 0x14);
    a.b_abs(AL, 0x100);
    a.word(0xE25E_F004);
    let mut img = a.finish();
    img.resize(0x4000, 0);
    let mut irq = Assembler::new(0x100);
    irq.push(0b0101_0000_0000_1111);
    irq.mov_imm(0, 0x0400_0000);
    irq.word(0xE28F_E000);
    irq.word(0xE510_F004);
    irq.pop(0b0101_0000_0000_1111);
    irq.word(0xE25E_F004);
    let code = irq.finish();
    img[0x100..0x100 + code.len()].copy_from_slice(&code);
    img
}

fn bios_if_addr9(bus: &Bus9) -> u32 {
    bus.s.cp15.dtcm_base + 0x3FF8
}

const BIOS_IF7: u32 = 0x0380_FFF8;

fn intr_wait<B: Bus>(
    bus: &mut B,
    io_ime: &mut bool,
    active: &mut bool,
    flag_addr: u32,
    discard: bool,
    mask: u32,
) -> SwiOutcome {
    *io_ime = true;
    let flags = r32(bus, flag_addr);
    if discard && !*active {
        w32(bus, flag_addr, flags & !mask);
    } else if flags & mask != 0 {
        w32(bus, flag_addr, flags & !mask);
        *active = false;
        return SwiOutcome::Handled;
    }
    *active = true;
    SwiOutcome::HaltAndRepeat
}

fn common<B: Bus>(bus: &mut B, cpu: &mut Arm, n: u8) -> bool {
    match n {
        0x03 => bus.idle(cpu.r[0].saturating_mul(4).min(1 << 20)), // WaitByLoop
        0x09 => div(cpu, cpu.r[0] as i32, cpu.r[1] as i32),
        0x0B => cpu_set(bus, cpu.r[0], cpu.r[1], cpu.r[2]),
        0x0C => cpu_fast_set(bus, cpu.r[0], cpu.r[1], cpu.r[2]),
        0x0D => cpu.r[0] = isqrt(cpu.r[0]),
        0x0E => cpu.r[0] = crc16(bus, cpu.r[0] as u16, cpu.r[1], cpu.r[2]) as u32,
        0x0F => cpu.r[0] = 0, // IsDebugger: retail unit
        0x10 => bit_unpack(bus, cpu.r[0], cpu.r[1], cpu.r[2]),
        0x11 => lz77(bus, cpu.r[0], cpu.r[1], false),
        0x12 => lz77(bus, cpu.r[0], cpu.r[1], true),
        0x13 => huffman(bus, cpu.r[0], cpu.r[1]),
        0x14 => rle(bus, cpu.r[0], cpu.r[1], false),
        0x15 => rle(bus, cpu.r[0], cpu.r[1], true),
        0x16 => diff8(bus, cpu.r[0], cpu.r[1], false),
        0x17 => diff8(bus, cpu.r[0], cpu.r[1], true),
        0x18 => diff16(bus, cpu.r[0], cpu.r[1]),
        0x1F => {} // CustomPost
        _ => return false,
    }
    true
}

pub(crate) fn swi9(bus: &mut Bus9, cpu: &mut Arm, n: u8) -> SwiOutcome {
    match n {
        0x04 | 0x05 => {
            let (discard, mask) = if n == 5 { (true, 1) } else { (cpu.r[0] != 0, cpu.r[1]) };
            if n == 5 {
                cpu.r[0] = 1;
                cpu.r[1] = 1;
            }
            let addr = bios_if_addr9(bus);
            let mut ime = bus.s.io[ARM9].ime;
            let mut active = bus.s.io[ARM9].intr_wait_active;
            let out = intr_wait(bus, &mut ime, &mut active, addr, discard, mask);
            bus.s.io[ARM9].ime = ime;
            bus.s.io[ARM9].intr_wait_active = active;
            return out;
        }
        0x06 => bus.s.io[ARM9].halt_request = true,
        _ => {
            if !common(bus, cpu, n) {
                tracing::warn!("unimplemented ARM9 BIOS SWI {n:#04x}");
            }
        }
    }
    SwiOutcome::Handled
}

pub(crate) fn swi7(bus: &mut Bus7, cpu: &mut Arm, n: u8) -> SwiOutcome {
    match n {
        0x04 | 0x05 => {
            let (discard, mask) = if n == 5 { (true, 1) } else { (cpu.r[0] != 0, cpu.r[1]) };
            if n == 5 {
                cpu.r[0] = 1;
                cpu.r[1] = 1;
            }
            let mut ime = bus.s.io[ARM7].ime;
            let mut active = bus.s.io[ARM7].intr_wait_active;
            let out = intr_wait(bus, &mut ime, &mut active, BIOS_IF7, discard, mask);
            bus.s.io[ARM7].ime = ime;
            bus.s.io[ARM7].intr_wait_active = active;
            return out;
        }
        0x06 | 0x07 => bus.s.io[ARM7].halt_request = true,
        0x08 => {} // SoundBias ramp
        0x1A => {
            // GetSineTable: sin(i * 90° / 64) * 0x8000
            let i = (cpu.r[0] & 0x3F) as f64;
            cpu.r[0] = ((i * std::f64::consts::FRAC_PI_2 / 64.0).sin() * 32768.0) as u32 & 0xFFFF;
        }
        0x1B => {
            // GetPitchTable: (2^(i/768) - 1) * 0x10000
            let i = (cpu.r[0] % 768) as f64;
            cpu.r[0] = ((2f64.powf(i / 768.0) - 1.0) * 65536.0) as u32 & 0xFFFF;
        }
        0x1C => {
            // GetVolumeTable: exponential 0..127 curve over 724 steps
            let i = cpu.r[0].min(723) as f64;
            cpu.r[0] = (127.0 * 2f64.powf((i - 723.0) / 96.0)).round() as u32;
        }
        _ => {
            if !common(bus, cpu, n) {
                tracing::warn!("unimplemented ARM7 BIOS SWI {n:#04x}");
            }
        }
    }
    SwiOutcome::Handled
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stubs_have_irq_dispatchers() {
        let s9 = stub9();
        let w = |img: &[u8], a: usize| u32::from_le_bytes(img[a..a + 4].try_into().unwrap());
        assert_eq!(w(&s9, 0x100), 0xE92D_500F);
        assert_eq!(w(&s9, 0x104), 0xEE19_0F11);
        assert_eq!(w(&s9, 0x18) >> 24, 0xEA);
        let s7 = stub7();
        assert_eq!(w(&s7, 0x100), 0xE92D_500F);
        assert_eq!(w(&s7, 0x10C), 0xE510_F004);
    }
}
