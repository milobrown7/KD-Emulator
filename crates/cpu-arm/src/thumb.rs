//! 16-bit Thumb instruction set — all 19 formats, plus the ARMv5 `BLX`
//! variants.

use crate::arm::{block_transfer_common, load_half, load_signed_half, load_word, shift};
use crate::cpu::*;
use crate::{Access, Bus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum Op {
    MoveShifted,
    AddSub,
    Imm,
    Alu,
    HiReg,
    PcLoad,
    LoadStoreReg,
    LoadStoreSigned,
    LoadStoreImm,
    LoadStoreHalf,
    SpLoadStore,
    LoadAddress,
    AddSp,
    PushPop,
    Multiple,
    CondBranch,
    Swi,
    Branch,
    BlPrefix,
    BlSuffix,
    BlxSuffix,
    Undefined,
}

const fn classify(idx: usize) -> Op {
    let i = (idx as u32) << 6; // bits 15..6 of the instruction
    if i & 0xF800 == 0x1800 {
        return Op::AddSub;
    }
    if i & 0xE000 == 0x0000 {
        return Op::MoveShifted;
    }
    if i & 0xE000 == 0x2000 {
        return Op::Imm;
    }
    if i & 0xFC00 == 0x4000 {
        return Op::Alu;
    }
    if i & 0xFC00 == 0x4400 {
        return Op::HiReg;
    }
    if i & 0xF800 == 0x4800 {
        return Op::PcLoad;
    }
    if i & 0xF200 == 0x5000 {
        return Op::LoadStoreReg;
    }
    if i & 0xF200 == 0x5200 {
        return Op::LoadStoreSigned;
    }
    if i & 0xE000 == 0x6000 {
        return Op::LoadStoreImm;
    }
    if i & 0xF000 == 0x8000 {
        return Op::LoadStoreHalf;
    }
    if i & 0xF000 == 0x9000 {
        return Op::SpLoadStore;
    }
    if i & 0xF000 == 0xA000 {
        return Op::LoadAddress;
    }
    if i & 0xFF00 == 0xB000 {
        return Op::AddSp;
    }
    if i & 0xF600 == 0xB400 {
        return Op::PushPop;
    }
    if i & 0xF000 == 0xC000 {
        return Op::Multiple;
    }
    if i & 0xFF00 == 0xDF00 {
        return Op::Swi;
    }
    if i & 0xFF00 == 0xDE00 {
        return Op::Undefined;
    }
    if i & 0xF000 == 0xD000 {
        return Op::CondBranch;
    }
    if i & 0xF800 == 0xE000 {
        return Op::Branch;
    }
    if i & 0xF800 == 0xE800 {
        return Op::BlxSuffix;
    }
    if i & 0xF800 == 0xF000 {
        return Op::BlPrefix;
    }
    if i & 0xF800 == 0xF800 {
        return Op::BlSuffix;
    }
    Op::Undefined
}

static LUT: [Op; 1024] = {
    let mut t = [Op::Undefined; 1024];
    let mut i = 0;
    while i < 1024 {
        t[i] = classify(i);
        i += 1;
    }
    t
};

#[inline]
fn lo3(v: u16, shift: u32) -> usize {
    ((v >> shift) & 7) as usize
}

pub(crate) fn execute<B: Bus>(cpu: &mut Arm, bus: &mut B, insn: u16) {
    match LUT[(insn >> 6) as usize] {
        Op::MoveShifted => {
            let kind = ((insn >> 11) & 3) as u32;
            let amount = ((insn >> 6) & 0x1F) as u32;
            let (v, c) = shift(kind, cpu.r[lo3(insn, 3)], amount, cpu.carry(), true);
            cpu.r[lo3(insn, 0)] = v;
            cpu.set_nz(v);
            cpu.set_flag(FLAG_C, c);
        }
        Op::AddSub => {
            let rs = cpu.r[lo3(insn, 3)];
            let operand = if insn & (1 << 10) != 0 { ((insn >> 6) & 7) as u32 } else { cpu.r[lo3(insn, 6)] };
            let v =
                if insn & (1 << 9) != 0 { cpu.sub_flags(rs, operand, true) } else { cpu.add_flags(rs, operand, false) };
            cpu.r[lo3(insn, 0)] = v;
        }
        Op::Imm => {
            let rd = lo3(insn, 8);
            let imm = (insn & 0xFF) as u32;
            match (insn >> 11) & 3 {
                0 => {
                    cpu.r[rd] = imm;
                    cpu.set_nz(imm);
                }
                1 => {
                    cpu.sub_flags(cpu.r[rd], imm, true);
                }
                2 => cpu.r[rd] = cpu.add_flags(cpu.r[rd], imm, false),
                _ => cpu.r[rd] = cpu.sub_flags(cpu.r[rd], imm, true),
            }
        }
        Op::Alu => alu(cpu, bus, insn),
        Op::HiReg => hi_reg(cpu, bus, insn),
        Op::PcLoad => {
            let addr = (cpu.r[15] & !3).wrapping_add(((insn & 0xFF) as u32) << 2);
            let v = bus.read32(addr, Access::NonSeq);
            bus.idle(1);
            cpu.r[lo3(insn, 8)] = v;
            cpu.next_fetch = Access::NonSeq;
        }
        Op::LoadStoreReg => {
            let addr = cpu.r[lo3(insn, 3)].wrapping_add(cpu.r[lo3(insn, 6)]);
            let rd = lo3(insn, 0);
            let load = insn & (1 << 11) != 0;
            let byte = insn & (1 << 10) != 0;
            transfer(cpu, bus, rd, addr, load, byte);
        }
        Op::LoadStoreSigned => {
            let addr = cpu.r[lo3(insn, 3)].wrapping_add(cpu.r[lo3(insn, 6)]);
            let rd = lo3(insn, 0);
            cpu.next_fetch = Access::NonSeq;
            match (insn >> 10) & 3 {
                0 => bus.write16(addr & !1, cpu.r[rd] as u16, Access::NonSeq),
                1 => {
                    cpu.r[rd] = bus.read8(addr, Access::NonSeq) as i8 as i32 as u32;
                    bus.idle(1);
                }
                2 => {
                    cpu.r[rd] = load_half(cpu, bus, addr);
                    bus.idle(1);
                }
                _ => {
                    cpu.r[rd] = load_signed_half(cpu, bus, addr);
                    bus.idle(1);
                }
            }
        }
        Op::LoadStoreImm => {
            let byte = insn & (1 << 12) != 0;
            let offset = ((insn >> 6) & 0x1F) as u32;
            let addr = cpu.r[lo3(insn, 3)].wrapping_add(if byte { offset } else { offset << 2 });
            transfer(cpu, bus, lo3(insn, 0), addr, insn & (1 << 11) != 0, byte);
        }
        Op::LoadStoreHalf => {
            let addr = cpu.r[lo3(insn, 3)].wrapping_add((((insn >> 6) & 0x1F) as u32) << 1);
            let rd = lo3(insn, 0);
            cpu.next_fetch = Access::NonSeq;
            if insn & (1 << 11) != 0 {
                cpu.r[rd] = load_half(cpu, bus, addr);
                bus.idle(1);
            } else {
                bus.write16(addr & !1, cpu.r[rd] as u16, Access::NonSeq);
            }
        }
        Op::SpLoadStore => {
            let addr = cpu.r[13].wrapping_add(((insn & 0xFF) as u32) << 2);
            transfer(cpu, bus, lo3(insn, 8), addr, insn & (1 << 11) != 0, false);
        }
        Op::LoadAddress => {
            let base = if insn & (1 << 11) != 0 { cpu.r[13] } else { cpu.r[15] & !3 };
            cpu.r[lo3(insn, 8)] = base.wrapping_add(((insn & 0xFF) as u32) << 2);
        }
        Op::AddSp => {
            let off = ((insn & 0x7F) as u32) << 2;
            cpu.r[13] = if insn & 0x80 != 0 { cpu.r[13].wrapping_sub(off) } else { cpu.r[13].wrapping_add(off) };
        }
        Op::PushPop => {
            let load = insn & (1 << 11) != 0;
            let extra = insn & (1 << 8) != 0;
            let mut list = (insn & 0xFF) as u32;
            if load {
                if extra {
                    list |= 1 << 15;
                }
                // POP = LDMIA sp!
                block_transfer_common(cpu, bus, 13, list, false, true, false, true, true);
            } else {
                if extra {
                    list |= 1 << 14;
                }
                // PUSH = STMDB sp!
                block_transfer_common(cpu, bus, 13, list, true, false, false, true, false);
            }
        }
        Op::Multiple => {
            let rb = lo3(insn, 8);
            let list = (insn & 0xFF) as u32;
            let load = insn & (1 << 11) != 0;
            if load && list & (1 << rb) != 0 && !cpu.is_v5() {
                // LDMIA with Rb in the list: no writeback on ARM7.
                block_transfer_common(cpu, bus, rb, list, false, true, false, false, true);
            } else {
                block_transfer_common(cpu, bus, rb, list, false, true, false, true, load);
            }
        }
        Op::CondBranch => {
            let cond = ((insn >> 8) & 0xF) as u32;
            if cpu.cond_passes(cond) {
                let off = ((insn as u8 as i8 as i32) << 1) as u32;
                let target = cpu.r[15].wrapping_add(off);
                cpu.branch_to(bus, target);
            }
        }
        Op::Swi => cpu.software_interrupt(bus, (insn & 0xFF) as u32),
        Op::Branch => {
            let off = ((((insn & 0x7FF) as i32) << 21) >> 20) as u32;
            let target = cpu.r[15].wrapping_add(off);
            cpu.branch_to(bus, target);
        }
        Op::BlPrefix => {
            let off = ((((insn & 0x7FF) as i32) << 21) >> 9) as u32;
            cpu.r[14] = cpu.r[15].wrapping_add(off);
        }
        Op::BlSuffix => {
            let next = cpu.r[15].wrapping_sub(2);
            let target = cpu.r[14].wrapping_add(((insn & 0x7FF) as u32) << 1);
            cpu.r[14] = next | 1;
            cpu.branch_to(bus, target);
        }
        Op::BlxSuffix if cpu.is_v5() => {
            let next = cpu.r[15].wrapping_sub(2);
            let target = cpu.r[14].wrapping_add(((insn & 0x7FF) as u32) << 1) & !3;
            cpu.r[14] = next | 1;
            cpu.cpsr &= !FLAG_T;
            cpu.branch_to(bus, target);
        }
        _ => cpu.undefined(bus, insn as u32),
    }
}

fn transfer<B: Bus>(cpu: &mut Arm, bus: &mut B, rd: usize, addr: u32, load: bool, byte: bool) {
    cpu.next_fetch = Access::NonSeq;
    if load {
        cpu.r[rd] = if byte { bus.read8(addr, Access::NonSeq) as u32 } else { load_word(bus, addr, Access::NonSeq) };
        bus.idle(1);
    } else if byte {
        bus.write8(addr, cpu.r[rd] as u8, Access::NonSeq);
    } else {
        bus.write32(addr & !3, cpu.r[rd], Access::NonSeq);
    }
}

fn alu<B: Bus>(cpu: &mut Arm, bus: &mut B, insn: u16) {
    let rd = lo3(insn, 0);
    let rs = cpu.r[lo3(insn, 3)];
    let a = cpu.r[rd];
    let c = cpu.carry();
    let shift_op = |cpu: &mut Arm, bus: &mut B, kind: u32| {
        bus.idle(1);
        let (v, carry) = shift(kind, a, rs & 0xFF, c, false);
        cpu.set_nz(v);
        cpu.set_flag(FLAG_C, carry);
        cpu.r[rd] = v;
    };
    match (insn >> 6) & 0xF {
        0x0 => {
            cpu.r[rd] = a & rs;
            cpu.set_nz(a & rs);
        }
        0x1 => {
            cpu.r[rd] = a ^ rs;
            cpu.set_nz(a ^ rs);
        }
        0x2 => shift_op(cpu, bus, 0),
        0x3 => shift_op(cpu, bus, 1),
        0x4 => shift_op(cpu, bus, 2),
        0x5 => cpu.r[rd] = cpu.add_flags(a, rs, c),
        0x6 => cpu.r[rd] = cpu.sub_flags(a, rs, c),
        0x7 => shift_op(cpu, bus, 3),
        0x8 => cpu.set_nz(a & rs),
        0x9 => cpu.r[rd] = cpu.sub_flags(0, rs, true),
        0xA => {
            cpu.sub_flags(a, rs, true);
        }
        0xB => {
            cpu.add_flags(a, rs, false);
        }
        0xC => {
            cpu.r[rd] = a | rs;
            cpu.set_nz(a | rs);
        }
        0xD => {
            let v = a.wrapping_mul(rs);
            let m = {
                let chk = |mask: u32| a & mask == 0 || a & mask == mask;
                if chk(0xFFFF_FF00) {
                    1
                } else if chk(0xFFFF_0000) {
                    2
                } else if chk(0xFF00_0000) {
                    3
                } else {
                    4
                }
            };
            bus.idle(m);
            cpu.r[rd] = v;
            cpu.set_nz(v);
        }
        0xE => {
            cpu.r[rd] = a & !rs;
            cpu.set_nz(a & !rs);
        }
        _ => {
            cpu.r[rd] = !rs;
            cpu.set_nz(!rs);
        }
    }
}

fn hi_reg<B: Bus>(cpu: &mut Arm, bus: &mut B, insn: u16) {
    let rd = (lo3(insn, 0)) | if insn & 0x80 != 0 { 8 } else { 0 };
    let rs = ((insn >> 3) & 0xF) as usize;
    let rs_val = cpu.r[rs];
    match (insn >> 8) & 3 {
        0 => {
            let v = cpu.r[rd].wrapping_add(rs_val);
            if rd == 15 {
                cpu.branch_to(bus, v);
            } else {
                cpu.r[rd] = v;
            }
        }
        1 => {
            cpu.sub_flags(cpu.r[rd], rs_val, true);
        }
        2 => {
            if rd == 15 {
                cpu.branch_to(bus, rs_val);
            } else {
                cpu.r[rd] = rs_val;
            }
        }
        _ => {
            if insn & 0x80 != 0 && cpu.is_v5() {
                // BLX Rs
                cpu.r[14] = cpu.r[15].wrapping_sub(2) | 1;
            }
            cpu.branch_exchange(bus, rs_val);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lut_classification() {
        let op = |insn: u16| LUT[(insn >> 6) as usize];
        assert_eq!(op(0x0088), Op::MoveShifted); // lsl r0, r1, #2
        assert_eq!(op(0x1888), Op::AddSub); // add r0, r1, r2
        assert_eq!(op(0x2001), Op::Imm); // mov r0, #1
        assert_eq!(op(0x4048), Op::Alu); // eor r0, r1
        assert_eq!(op(0x4770), Op::HiReg); // bx lr
        assert_eq!(op(0x4801), Op::PcLoad);
        assert_eq!(op(0x5888), Op::LoadStoreReg);
        assert_eq!(op(0x5E88), Op::LoadStoreSigned);
        assert_eq!(op(0x6808), Op::LoadStoreImm);
        assert_eq!(op(0x8808), Op::LoadStoreHalf);
        assert_eq!(op(0x9801), Op::SpLoadStore);
        assert_eq!(op(0xA801), Op::LoadAddress);
        assert_eq!(op(0xB082), Op::AddSp);
        assert_eq!(op(0xB510), Op::PushPop);
        assert_eq!(op(0xBD10), Op::PushPop);
        assert_eq!(op(0xC803), Op::Multiple);
        assert_eq!(op(0xD0FE), Op::CondBranch);
        assert_eq!(op(0xDF05), Op::Swi);
        assert_eq!(op(0xE7FE), Op::Branch);
        assert_eq!(op(0xF000), Op::BlPrefix);
        assert_eq!(op(0xF800), Op::BlSuffix);
        assert_eq!(op(0xE800), Op::BlxSuffix);
    }
}
