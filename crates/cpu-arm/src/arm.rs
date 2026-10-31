//! 32-bit ARM instruction set (ARMv4T + ARMv5TE).

use crate::cpu::*;
use crate::{Access, Bus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum Op {
    DataProc,
    Mrs,
    MsrReg,
    MsrImm,
    Mul,
    MulLong,
    Swap,
    Bx,
    BlxReg,
    Clz,
    QArith,
    SmulHalf,
    Bkpt,
    HalfTransfer,
    SingleTransfer,
    BlockTransfer,
    Branch,
    CoprocTransfer,
    CoprocData,
    CoprocReg,
    Swi,
    Undefined,
}

const fn classify(idx: usize) -> Op {
    let hi = (idx >> 4) as u32; // bits 27..20
    let lo = (idx & 0xF) as u32; // bits 7..4
    if hi & 0xE0 == 0x00 {
        // 000x xxxx
        if lo == 0b1001 {
            if hi & 0xFC == 0x00 {
                return Op::Mul;
            }
            if hi & 0xF8 == 0x08 {
                return Op::MulLong;
            }
            if hi & 0xFB == 0x10 {
                return Op::Swap;
            }
            return Op::Undefined;
        }
        if lo & 0b1001 == 0b1001 {
            return Op::HalfTransfer;
        }
        // Miscellaneous instructions live in the TST/TEQ/CMP/CMN space with S=0.
        if hi & 0xF9 == 0x10 {
            if lo == 0 {
                return if hi & 0x02 == 0 { Op::Mrs } else { Op::MsrReg };
            }
            if hi == 0x12 && lo == 0b0001 {
                return Op::Bx;
            }
            if hi == 0x12 && lo == 0b0011 {
                return Op::BlxReg;
            }
            if hi == 0x16 && lo == 0b0001 {
                return Op::Clz;
            }
            if lo == 0b0101 {
                return Op::QArith;
            }
            if hi == 0x12 && lo == 0b0111 {
                return Op::Bkpt;
            }
            if lo & 0b1001 == 0b1000 {
                return Op::SmulHalf;
            }
            return Op::Undefined;
        }
        return Op::DataProc;
    }
    if hi & 0xE0 == 0x20 {
        // 001x xxxx: immediate data processing / MSR immediate
        if hi & 0xFB == 0x32 {
            return Op::MsrImm;
        }
        if hi & 0xF9 == 0x30 {
            return Op::Undefined;
        }
        return Op::DataProc;
    }
    if hi & 0xE0 == 0x60 && lo & 1 == 1 {
        return Op::Undefined;
    }
    if hi & 0xC0 == 0x40 {
        return Op::SingleTransfer;
    }
    if hi & 0xE0 == 0x80 {
        return Op::BlockTransfer;
    }
    if hi & 0xE0 == 0xA0 {
        return Op::Branch;
    }
    if hi & 0xE0 == 0xC0 {
        return Op::CoprocTransfer;
    }
    if hi & 0xF0 == 0xE0 {
        return if lo & 1 == 0 { Op::CoprocData } else { Op::CoprocReg };
    }
    Op::Swi
}

static LUT: [Op; 4096] = {
    let mut t = [Op::Undefined; 4096];
    let mut i = 0;
    while i < 4096 {
        t[i] = classify(i);
        i += 1;
    }
    t
};

#[inline]
fn lut_index(insn: u32) -> usize {
    (((insn >> 16) & 0xFF0) | ((insn >> 4) & 0xF)) as usize
}

pub(crate) fn execute<B: Bus>(cpu: &mut Arm, bus: &mut B, insn: u32) {
    let cond = insn >> 28;
    if cond != 0xE && !cpu.cond_passes(cond) {
        if cond == 0xF && cpu.is_v5() {
            return execute_unconditional(cpu, bus, insn);
        }
        return;
    }
    match LUT[lut_index(insn)] {
        Op::DataProc => data_processing(cpu, bus, insn),
        Op::Mrs => mrs(cpu, insn),
        Op::MsrReg => msr(cpu, insn, cpu.r[(insn & 0xF) as usize]),
        Op::MsrImm => {
            let rot = ((insn >> 8) & 0xF) * 2;
            msr(cpu, insn, (insn & 0xFF).rotate_right(rot))
        }
        Op::Mul => multiply(cpu, bus, insn),
        Op::MulLong => multiply_long(cpu, bus, insn),
        Op::Swap => swap(cpu, bus, insn),
        Op::Bx => {
            let target = cpu.r[(insn & 0xF) as usize];
            cpu.branch_exchange(bus, target);
        }
        Op::BlxReg if cpu.is_v5() => {
            let target = cpu.r[(insn & 0xF) as usize];
            cpu.r[14] = cpu.r[15].wrapping_sub(4);
            cpu.branch_exchange(bus, target);
        }
        Op::Clz if cpu.is_v5() => {
            let rm = cpu.r[(insn & 0xF) as usize];
            cpu.r[((insn >> 12) & 0xF) as usize] = rm.leading_zeros();
        }
        Op::QArith if cpu.is_v5() => saturating_arith(cpu, insn),
        Op::SmulHalf if cpu.is_v5() => signed_halfword_multiply(cpu, insn),
        Op::Bkpt if cpu.is_v5() => {
            let ret = cpu.pc().wrapping_add(4);
            cpu.exception(bus, Exception::PrefetchAbort, ret);
        }
        Op::HalfTransfer => halfword_transfer(cpu, bus, insn),
        Op::SingleTransfer => single_transfer(cpu, bus, insn),
        Op::BlockTransfer => block_transfer(cpu, bus, insn),
        Op::Branch => {
            let offset = ((insn << 8) as i32 >> 6) as u32;
            if insn & (1 << 24) != 0 {
                cpu.r[14] = cpu.r[15].wrapping_sub(4);
            }
            let target = cpu.r[15].wrapping_add(offset);
            cpu.branch_to(bus, target);
        }
        Op::CoprocReg => coprocessor_register(cpu, bus, insn),
        Op::CoprocTransfer | Op::CoprocData => cpu.undefined(bus, insn),
        Op::Swi => cpu.software_interrupt(bus, insn & 0x00FF_FFFF),
        _ => cpu.undefined(bus, insn),
    }
}

/// ARMv5 `cond = 1111` space: BLX immediate and PLD; the rest is undefined.
fn execute_unconditional<B: Bus>(cpu: &mut Arm, bus: &mut B, insn: u32) {
    if insn & 0x0E00_0000 == 0x0A00_0000 {
        let offset = ((insn << 8) as i32 >> 6) as u32 | ((insn >> 23) & 2);
        cpu.r[14] = cpu.r[15].wrapping_sub(4);
        let target = cpu.r[15].wrapping_add(offset);
        cpu.cpsr |= FLAG_T;
        cpu.branch_to(bus, target);
    } else if insn & 0x0D70_F000 == 0x0550_F000 {
        // PLD: cache hint, no architectural effect.
    } else {
        cpu.undefined(bus, insn);
    }
}

/// Barrel shifter. Returns (value, carry_out).
#[inline]
pub(crate) fn shift(kind: u32, value: u32, amount: u32, carry: bool, immediate: bool) -> (u32, bool) {
    match kind {
        0 => match amount {
            0 => (value, carry),
            1..=31 => (value << amount, (value >> (32 - amount)) & 1 != 0),
            32 => (0, value & 1 != 0),
            _ => (0, false),
        },
        1 => {
            let amount = if immediate && amount == 0 { 32 } else { amount };
            match amount {
                0 => (value, carry),
                1..=31 => (value >> amount, (value >> (amount - 1)) & 1 != 0),
                32 => (0, value >> 31 != 0),
                _ => (0, false),
            }
        }
        2 => {
            let amount = if immediate && amount == 0 { 32 } else { amount };
            match amount {
                0 => (value, carry),
                1..=31 => (((value as i32) >> amount) as u32, (value >> (amount - 1)) & 1 != 0),
                _ => (((value as i32) >> 31) as u32, value >> 31 != 0),
            }
        }
        _ => {
            if immediate && amount == 0 {
                // RRX
                (((carry as u32) << 31) | (value >> 1), value & 1 != 0)
            } else if amount == 0 {
                (value, carry)
            } else {
                let a = amount & 31;
                if a == 0 {
                    (value, value >> 31 != 0)
                } else {
                    (value.rotate_right(a), (value >> (a - 1)) & 1 != 0)
                }
            }
        }
    }
}

fn data_processing<B: Bus>(cpu: &mut Arm, bus: &mut B, insn: u32) {
    let opcode = (insn >> 21) & 0xF;
    let set_flags = insn & (1 << 20) != 0;
    let rn_idx = ((insn >> 16) & 0xF) as usize;
    let rd = ((insn >> 12) & 0xF) as usize;
    let carry_in = cpu.carry();

    let mut rn = cpu.r[rn_idx];
    let (op2, shifter_carry) = if insn & (1 << 25) != 0 {
        let rot = ((insn >> 8) & 0xF) * 2;
        let v = (insn & 0xFF).rotate_right(rot);
        (v, if rot == 0 { carry_in } else { v >> 31 != 0 })
    } else {
        let rm_idx = (insn & 0xF) as usize;
        let kind = (insn >> 5) & 3;
        if insn & (1 << 4) != 0 {
            // Register-specified shift: one extra internal cycle and PC
            // reads 12 ahead.
            bus.idle(1);
            let amount = cpu.r[((insn >> 8) & 0xF) as usize] & 0xFF;
            let mut rm = cpu.r[rm_idx];
            if rm_idx == 15 {
                rm = rm.wrapping_add(4);
            }
            if rn_idx == 15 {
                rn = rn.wrapping_add(4);
            }
            shift(kind, rm, amount, carry_in, false)
        } else {
            let amount = (insn >> 7) & 0x1F;
            shift(kind, cpu.r[rm_idx], amount, carry_in, true)
        }
    };

    let logical = |cpu: &mut Arm, res: u32| {
        if set_flags {
            cpu.set_nz(res);
            cpu.set_flag(FLAG_C, shifter_carry);
        }
        res
    };

    let (result, write) = match opcode {
        0x0 => (logical(cpu, rn & op2), true),
        0x1 => (logical(cpu, rn ^ op2), true),
        0x2 => (if set_flags { cpu.sub_flags(rn, op2, true) } else { rn.wrapping_sub(op2) }, true),
        0x3 => (if set_flags { cpu.sub_flags(op2, rn, true) } else { op2.wrapping_sub(rn) }, true),
        0x4 => (if set_flags { cpu.add_flags(rn, op2, false) } else { rn.wrapping_add(op2) }, true),
        0x5 => (
            if set_flags { cpu.add_flags(rn, op2, carry_in) } else { rn.wrapping_add(op2).wrapping_add(carry_in as u32) },
            true,
        ),
        0x6 => (
            if set_flags {
                cpu.sub_flags(rn, op2, carry_in)
            } else {
                rn.wrapping_sub(op2).wrapping_sub(!carry_in as u32)
            },
            true,
        ),
        0x7 => (
            if set_flags {
                cpu.sub_flags(op2, rn, carry_in)
            } else {
                op2.wrapping_sub(rn).wrapping_sub(!carry_in as u32)
            },
            true,
        ),
        0x8 => (logical(cpu, rn & op2), false),
        0x9 => (logical(cpu, rn ^ op2), false),
        0xA => (cpu.sub_flags(rn, op2, true), false),
        0xB => (cpu.add_flags(rn, op2, false), false),
        0xC => (logical(cpu, rn | op2), true),
        0xD => (logical(cpu, op2), true),
        0xE => (logical(cpu, rn & !op2), true),
        _ => (logical(cpu, !op2), true),
    };

    if rd == 15 {
        if set_flags {
            // Exception return: restore CPSR from SPSR.
            let spsr = cpu.spsr();
            cpu.set_cpsr(spsr);
        }
        if write {
            if cpu.thumb() {
                cpu.branch_to(bus, result & !1);
            } else {
                cpu.branch_to(bus, result & !3);
            }
        }
    } else if write {
        cpu.r[rd] = result;
    }
}

fn mrs(cpu: &mut Arm, insn: u32) {
    let rd = ((insn >> 12) & 0xF) as usize;
    cpu.r[rd] = if insn & (1 << 22) != 0 { cpu.spsr() } else { cpu.cpsr };
}

fn msr(cpu: &mut Arm, insn: u32, value: u32) {
    let mut mask = 0u32;
    if insn & (1 << 19) != 0 {
        mask |= 0xFF00_0000;
    }
    if insn & (1 << 18) != 0 {
        mask |= 0x00FF_0000;
    }
    if insn & (1 << 17) != 0 {
        mask |= 0x0000_FF00;
    }
    if insn & (1 << 16) != 0 {
        mask |= 0x0000_00FF;
    }
    if insn & (1 << 22) != 0 {
        if bank_of(cpu.mode()) != 0 {
            let s = cpu.spsr();
            cpu.set_spsr((s & !mask) | (value & mask));
        }
    } else {
        if cpu.mode() == MODE_USR {
            mask &= 0xFF00_0000;
        }
        // The T bit cannot be changed by MSR.
        mask &= !FLAG_T;
        let new = (cpu.cpsr & !mask) | (value & mask);
        cpu.set_cpsr(new | 0x10);
    }
}

/// Multiplier early-termination cycles (`m` in the ARM7TDMI datasheet).
#[inline]
fn mul_cycles(rs: u32, signed: bool) -> u32 {
    let check = |mask: u32| rs & mask == 0 || (signed && rs & mask == mask);
    if check(0xFFFF_FF00) {
        1
    } else if check(0xFFFF_0000) {
        2
    } else if check(0xFF00_0000) {
        3
    } else {
        4
    }
}

fn multiply<B: Bus>(cpu: &mut Arm, bus: &mut B, insn: u32) {
    let rd = ((insn >> 16) & 0xF) as usize;
    let rn = cpu.r[((insn >> 12) & 0xF) as usize];
    let rs = cpu.r[((insn >> 8) & 0xF) as usize];
    let rm = cpu.r[(insn & 0xF) as usize];
    let accumulate = insn & (1 << 21) != 0;
    let mut res = rm.wrapping_mul(rs);
    let mut cycles = mul_cycles(rs, true);
    if accumulate {
        res = res.wrapping_add(rn);
        cycles += 1;
    }
    bus.idle(cycles);
    cpu.r[rd] = res;
    if insn & (1 << 20) != 0 {
        cpu.set_nz(res);
    }
}

fn multiply_long<B: Bus>(cpu: &mut Arm, bus: &mut B, insn: u32) {
    let rd_hi = ((insn >> 16) & 0xF) as usize;
    let rd_lo = ((insn >> 12) & 0xF) as usize;
    let rs = cpu.r[((insn >> 8) & 0xF) as usize];
    let rm = cpu.r[(insn & 0xF) as usize];
    let signed = insn & (1 << 22) != 0;
    let accumulate = insn & (1 << 21) != 0;
    let mut res = if signed {
        (rm as i32 as i64).wrapping_mul(rs as i32 as i64) as u64
    } else {
        (rm as u64).wrapping_mul(rs as u64)
    };
    let mut cycles = mul_cycles(rs, signed) + 1;
    if accumulate {
        res = res.wrapping_add(((cpu.r[rd_hi] as u64) << 32) | cpu.r[rd_lo] as u64);
        cycles += 1;
    }
    bus.idle(cycles);
    cpu.r[rd_lo] = res as u32;
    cpu.r[rd_hi] = (res >> 32) as u32;
    if insn & (1 << 20) != 0 {
        cpu.set_flag(FLAG_N, res >> 63 != 0);
        cpu.set_flag(FLAG_Z, res == 0);
    }
}

#[inline]
fn sat_add(cpu: &mut Arm, a: i32, b: i32) -> i32 {
    match a.checked_add(b) {
        Some(v) => v,
        None => {
            cpu.cpsr |= FLAG_Q;
            if b < 0 {
                i32::MIN
            } else {
                i32::MAX
            }
        }
    }
}

#[inline]
fn sat_sub(cpu: &mut Arm, a: i32, b: i32) -> i32 {
    match a.checked_sub(b) {
        Some(v) => v,
        None => {
            cpu.cpsr |= FLAG_Q;
            if b > 0 {
                i32::MIN
            } else {
                i32::MAX
            }
        }
    }
}

fn saturating_arith(cpu: &mut Arm, insn: u32) {
    let rn = cpu.r[((insn >> 16) & 0xF) as usize] as i32;
    let rd = ((insn >> 12) & 0xF) as usize;
    let rm = cpu.r[(insn & 0xF) as usize] as i32;
    let res = match (insn >> 21) & 3 {
        0 => sat_add(cpu, rm, rn),
        1 => sat_sub(cpu, rm, rn),
        2 => {
            let d = sat_add(cpu, rn, rn);
            sat_add(cpu, rm, d)
        }
        _ => {
            let d = sat_add(cpu, rn, rn);
            sat_sub(cpu, rm, d)
        }
    };
    cpu.r[rd] = res as u32;
}

fn signed_halfword_multiply(cpu: &mut Arm, insn: u32) {
    let rd = ((insn >> 16) & 0xF) as usize;
    let rn_idx = ((insn >> 12) & 0xF) as usize;
    let rs = cpu.r[((insn >> 8) & 0xF) as usize];
    let rm = cpu.r[(insn & 0xF) as usize];
    let x = insn & (1 << 5) != 0;
    let y = insn & (1 << 6) != 0;
    let half = |v: u32, top: bool| -> i32 {
        if top {
            (v >> 16) as i16 as i32
        } else {
            v as i16 as i32
        }
    };
    match (insn >> 21) & 3 {
        0 => {
            // SMLAxy
            let p = half(rm, x).wrapping_mul(half(rs, y));
            let acc = cpu.r[rn_idx] as i32;
            let (res, ovf) = p.overflowing_add(acc);
            if ovf {
                cpu.cpsr |= FLAG_Q;
            }
            cpu.r[rd] = res as u32;
        }
        1 => {
            // SMLAWy / SMULWy
            let p = ((rm as i32 as i64 * half(rs, y) as i64) >> 16) as i32;
            if x {
                cpu.r[rd] = p as u32;
            } else {
                let (res, ovf) = p.overflowing_add(cpu.r[rn_idx] as i32);
                if ovf {
                    cpu.cpsr |= FLAG_Q;
                }
                cpu.r[rd] = res as u32;
            }
        }
        2 => {
            // SMLALxy: RdHi = rd, RdLo = rn_idx
            let p = half(rm, x) as i64 * half(rs, y) as i64;
            let acc = ((cpu.r[rd] as u64) << 32 | cpu.r[rn_idx] as u64) as i64;
            let res = acc.wrapping_add(p) as u64;
            cpu.r[rn_idx] = res as u32;
            cpu.r[rd] = (res >> 32) as u32;
        }
        _ => {
            // SMULxy
            cpu.r[rd] = half(rm, x).wrapping_mul(half(rs, y)) as u32;
        }
    }
}

fn swap<B: Bus>(cpu: &mut Arm, bus: &mut B, insn: u32) {
    let rn = cpu.r[((insn >> 16) & 0xF) as usize];
    let rd = ((insn >> 12) & 0xF) as usize;
    let rm = cpu.r[(insn & 0xF) as usize];
    if insn & (1 << 22) != 0 {
        let old = bus.read8(rn, Access::NonSeq);
        bus.write8(rn, rm as u8, Access::NonSeq);
        cpu.r[rd] = old as u32;
    } else {
        let old = bus.read32(rn & !3, Access::NonSeq).rotate_right((rn & 3) * 8);
        bus.write32(rn & !3, rm, Access::NonSeq);
        cpu.r[rd] = old;
    }
    bus.idle(1);
    cpu.next_fetch = Access::NonSeq;
}

/// LDR with the architecture's unaligned rotation.
#[inline]
pub(crate) fn load_word<B: Bus>(bus: &mut B, addr: u32, access: Access) -> u32 {
    bus.read32(addr & !3, access).rotate_right((addr & 3) * 8)
}

/// LDRH: ARM7 rotates unaligned halfwords, ARM9 forces alignment.
#[inline]
pub(crate) fn load_half<B: Bus>(cpu: &Arm, bus: &mut B, addr: u32) -> u32 {
    let v = bus.read16(addr & !1, Access::NonSeq) as u32;
    if !cpu.is_v5() && addr & 1 != 0 {
        v.rotate_right(8)
    } else {
        v
    }
}

/// LDRSH: an unaligned ARM7 access loads a sign-extended byte.
#[inline]
pub(crate) fn load_signed_half<B: Bus>(cpu: &Arm, bus: &mut B, addr: u32) -> u32 {
    if !cpu.is_v5() && addr & 1 != 0 {
        bus.read8(addr, Access::NonSeq) as i8 as i32 as u32
    } else {
        bus.read16(addr & !1, Access::NonSeq) as i16 as i32 as u32
    }
}

/// Write a loaded value to a register, handling PC (with ARMv5
/// interworking).
#[inline]
pub(crate) fn write_loaded<B: Bus>(cpu: &mut Arm, bus: &mut B, rd: usize, value: u32) {
    if rd == 15 {
        if cpu.is_v5() {
            cpu.branch_exchange(bus, value);
        } else {
            cpu.branch_to(bus, value);
        }
    } else {
        cpu.r[rd] = value;
    }
}

fn single_transfer<B: Bus>(cpu: &mut Arm, bus: &mut B, insn: u32) {
    let pre = insn & (1 << 24) != 0;
    let up = insn & (1 << 23) != 0;
    let byte = insn & (1 << 22) != 0;
    let writeback = insn & (1 << 21) != 0;
    let load = insn & (1 << 20) != 0;
    let rn = ((insn >> 16) & 0xF) as usize;
    let rd = ((insn >> 12) & 0xF) as usize;

    let offset = if insn & (1 << 25) != 0 {
        let amount = (insn >> 7) & 0x1F;
        shift((insn >> 5) & 3, cpu.r[(insn & 0xF) as usize], amount, cpu.carry(), true).0
    } else {
        insn & 0xFFF
    };
    let base = cpu.r[rn];
    let offset_addr = if up { base.wrapping_add(offset) } else { base.wrapping_sub(offset) };
    let addr = if pre { offset_addr } else { base };
    let do_writeback = !pre || writeback;

    if load {
        let value = if byte { bus.read8(addr, Access::NonSeq) as u32 } else { load_word(bus, addr, Access::NonSeq) };
        bus.idle(1);
        if do_writeback {
            cpu.r[rn] = offset_addr;
        }
        cpu.next_fetch = Access::NonSeq;
        write_loaded(cpu, bus, rd, value);
    } else {
        let value = if rd == 15 { cpu.r[15].wrapping_add(4) } else { cpu.r[rd] };
        if byte {
            bus.write8(addr, value as u8, Access::NonSeq);
        } else {
            bus.write32(addr & !3, value, Access::NonSeq);
        }
        if do_writeback {
            cpu.r[rn] = offset_addr;
        }
        cpu.next_fetch = Access::NonSeq;
    }
}

fn halfword_transfer<B: Bus>(cpu: &mut Arm, bus: &mut B, insn: u32) {
    let pre = insn & (1 << 24) != 0;
    let up = insn & (1 << 23) != 0;
    let imm = insn & (1 << 22) != 0;
    let writeback = insn & (1 << 21) != 0;
    let load = insn & (1 << 20) != 0;
    let rn = ((insn >> 16) & 0xF) as usize;
    let rd = ((insn >> 12) & 0xF) as usize;
    let sh = (insn >> 5) & 3;

    let offset = if imm { ((insn >> 4) & 0xF0) | (insn & 0xF) } else { cpu.r[(insn & 0xF) as usize] };
    let base = cpu.r[rn];
    let offset_addr = if up { base.wrapping_add(offset) } else { base.wrapping_sub(offset) };
    let addr = if pre { offset_addr } else { base };
    let do_writeback = !pre || writeback;
    cpu.next_fetch = Access::NonSeq;

    if load {
        let value = match sh {
            1 => load_half(cpu, bus, addr),
            2 => bus.read8(addr, Access::NonSeq) as i8 as i32 as u32,
            _ => load_signed_half(cpu, bus, addr),
        };
        bus.idle(1);
        if do_writeback {
            cpu.r[rn] = offset_addr;
        }
        write_loaded(cpu, bus, rd, value);
        return;
    }

    match sh {
        1 => {
            let value = if rd == 15 { cpu.r[15].wrapping_add(4) } else { cpu.r[rd] };
            bus.write16(addr & !1, value as u16, Access::NonSeq);
            if do_writeback {
                cpu.r[rn] = offset_addr;
            }
        }
        2 if cpu.is_v5() => {
            // LDRD
            let rd = rd & !1;
            let lo = bus.read32(addr & !3, Access::NonSeq);
            let hi = bus.read32((addr & !3).wrapping_add(4), Access::Seq);
            bus.idle(1);
            if do_writeback {
                cpu.r[rn] = offset_addr;
            }
            cpu.r[rd] = lo;
            write_loaded(cpu, bus, rd + 1, hi);
        }
        3 if cpu.is_v5() => {
            // STRD
            let rd = rd & !1;
            bus.write32(addr & !3, cpu.r[rd], Access::NonSeq);
            let hi = if rd + 1 == 15 { cpu.r[15].wrapping_add(4) } else { cpu.r[rd + 1] };
            bus.write32((addr & !3).wrapping_add(4), hi, Access::Seq);
            if do_writeback {
                cpu.r[rn] = offset_addr;
            }
        }
        _ => {
            // ARMv4: LDRD/STRD encodings are no-ops on the ARM7TDMI.
        }
    }
}

fn block_transfer<B: Bus>(cpu: &mut Arm, bus: &mut B, insn: u32) {
    let pre = insn & (1 << 24) != 0;
    let up = insn & (1 << 23) != 0;
    let psr_user = insn & (1 << 22) != 0;
    let writeback = insn & (1 << 21) != 0;
    let load = insn & (1 << 20) != 0;
    let rn = ((insn >> 16) & 0xF) as usize;
    let list = insn & 0xFFFF;
    block_transfer_common(cpu, bus, rn, list, pre, up, psr_user, writeback, load);
}

/// Shared by ARM LDM/STM and Thumb PUSH/POP/LDMIA/STMIA.
#[allow(clippy::too_many_arguments)]
pub(crate) fn block_transfer_common<B: Bus>(
    cpu: &mut Arm,
    bus: &mut B,
    rn: usize,
    list: u32,
    pre: bool,
    up: bool,
    psr_user: bool,
    writeback: bool,
    load: bool,
) {
    let base = cpu.r[rn];
    // Empty list quirk: transfers r15 and moves the base by 0x40.
    let (list, count) = if list == 0 { (1 << 15, 16u32) } else { (list, list.count_ones()) };
    let span = count * 4;
    let mut addr = match (pre, up) {
        (false, true) => base,
        (true, true) => base.wrapping_add(4),
        (false, false) => base.wrapping_sub(span).wrapping_add(4),
        (true, false) => base.wrapping_sub(span),
    };
    let new_base = if up { base.wrapping_add(span) } else { base.wrapping_sub(span) };
    let user_bank = psr_user && !(load && list & (1 << 15) != 0);
    let rn_in_list = list & (1 << rn) != 0;
    cpu.next_fetch = Access::NonSeq;

    let mut access = Access::NonSeq;
    if load {
        if writeback {
            cpu.r[rn] = new_base;
        }
        let mut pc_value = None;
        for i in 0..16 {
            if list & (1 << i) == 0 {
                continue;
            }
            let v = bus.read32(addr & !3, access);
            access = Access::Seq;
            addr = addr.wrapping_add(4);
            if i == 15 {
                pc_value = Some(v);
            } else if user_bank {
                cpu.set_user_reg(i, v);
            } else {
                cpu.r[i] = v;
            }
        }
        bus.idle(1);
        if writeback && rn_in_list {
            // ARM7: the loaded value wins. ARM9: writeback wins unless Rn is
            // the last register in the list (or the only one).
            let last = 31 - list.leading_zeros() as usize;
            if cpu.is_v5() && (list == 1 << rn || rn != last) {
                cpu.r[rn] = new_base;
            }
        }
        if let Some(pc) = pc_value {
            if psr_user {
                let spsr = cpu.spsr();
                cpu.set_cpsr(spsr);
                cpu.branch_to(bus, pc);
            } else {
                write_loaded(cpu, bus, 15, pc);
            }
        }
    } else {
        let first = list.trailing_zeros() as usize;
        for i in 0..16 {
            if list & (1 << i) == 0 {
                continue;
            }
            let v = if i == 15 {
                cpu.r[15].wrapping_add(4)
            } else if user_bank {
                cpu.user_reg(i)
            } else if i == rn && writeback && i != first && !cpu.is_v5() {
                new_base
            } else {
                cpu.r[i]
            };
            bus.write32(addr & !3, v, access);
            access = Access::Seq;
            addr = addr.wrapping_add(4);
        }
        if writeback {
            cpu.r[rn] = new_base;
        }
    }
}

fn coprocessor_register<B: Bus>(cpu: &mut Arm, bus: &mut B, insn: u32) {
    let cp = ((insn >> 8) & 0xF) as u8;
    let op1 = ((insn >> 21) & 7) as u8;
    let crn = ((insn >> 16) & 0xF) as u8;
    let rd = ((insn >> 12) & 0xF) as usize;
    let op2 = ((insn >> 5) & 7) as u8;
    let crm = (insn & 0xF) as u8;
    if insn & (1 << 20) != 0 {
        match bus.cp_read(cp, op1, crn, crm, op2) {
            Some(v) if rd == 15 => cpu.cpsr = (cpu.cpsr & 0x0FFF_FFFF) | (v & 0xF000_0000),
            Some(v) => cpu.r[rd] = v,
            None => cpu.undefined(bus, insn),
        }
    } else {
        let v = if rd == 15 { cpu.r[15].wrapping_add(4) } else { cpu.r[rd] };
        if !bus.cp_write(cp, op1, crn, crm, op2, v) {
            cpu.undefined(bus, insn);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lut_classification() {
        let op = |insn: u32| LUT[lut_index(insn)];
        assert_eq!(op(0xE0821003), Op::DataProc); // add r1, r2, r3
        assert_eq!(op(0xE3A00001), Op::DataProc); // mov r0, #1
        assert_eq!(op(0xE0010392), Op::Mul); // mul r1, r2, r3
        assert_eq!(op(0xE0821293), Op::MulLong); // umull r1, r2, r3, r2
        assert_eq!(op(0xE1020091), Op::Swap); // swp r0, r1, [r2]
        assert_eq!(op(0xE12FFF11), Op::Bx); // bx r1
        assert_eq!(op(0xE12FFF31), Op::BlxReg);
        assert_eq!(op(0xE16F0F11), Op::Clz);
        assert_eq!(op(0xE1010052), Op::QArith); // qadd r0, r2, r1
        assert_eq!(op(0xE1000382), Op::SmulHalf); // smlabb
        assert_eq!(op(0xE10F0000), Op::Mrs);
        assert_eq!(op(0xE129F000), Op::MsrReg);
        assert_eq!(op(0xE328F20F), Op::MsrImm);
        assert_eq!(op(0xE1D000B2), Op::HalfTransfer); // ldrh r0, [r0, #2]
        assert_eq!(op(0xE5910000), Op::SingleTransfer);
        assert_eq!(op(0xE7910002), Op::SingleTransfer);
        assert_eq!(op(0xE7900012), Op::Undefined);
        assert_eq!(op(0xE8BD8000), Op::BlockTransfer);
        assert_eq!(op(0xEAFFFFFE), Op::Branch);
        assert_eq!(op(0xEE110F10), Op::CoprocReg); // mrc p15
        assert_eq!(op(0xEF000005), Op::Swi);
    }

    #[test]
    fn shifter_edge_cases() {
        assert_eq!(shift(0, 0x8000_0001, 0, true, true), (0x8000_0001, true));
        assert_eq!(shift(0, 3, 32, false, false), (0, true));
        assert_eq!(shift(0, 3, 33, true, false), (0, false));
        assert_eq!(shift(1, 0x8000_0000, 0, false, true), (0, true)); // LSR #32
        assert_eq!(shift(2, 0x8000_0000, 0, false, true), (0xFFFF_FFFF, true)); // ASR #32
        assert_eq!(shift(2, 0x4000_0000, 40, true, false), (0, false));
        assert_eq!(shift(3, 0x0000_0003, 0, true, true), (0x8000_0001, true)); // RRX
        assert_eq!(shift(3, 0x8000_0001, 32, false, false), (0x8000_0001, true));
        assert_eq!(shift(3, 0x0000_0001, 1, false, false), (0x8000_0000, true));
        assert_eq!(shift(1, 0xF0, 0, true, false), (0xF0, true)); // reg shift by 0
    }
}
