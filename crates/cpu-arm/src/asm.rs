//! A tiny ARM/Thumb assembler used to build test programs and HLE stubs
//! without shipping any copyrighted firmware.

use std::collections::HashMap;

pub const EQ: u32 = 0x0;
pub const NE: u32 = 0x1;
pub const CS: u32 = 0x2;
pub const CC: u32 = 0x3;
pub const MI: u32 = 0x4;
pub const PL: u32 = 0x5;
pub const HI: u32 = 0x8;
pub const LS: u32 = 0x9;
pub const GE: u32 = 0xA;
pub const LT: u32 = 0xB;
pub const GT: u32 = 0xC;
pub const LE: u32 = 0xD;
pub const AL: u32 = 0xE;

pub const SP: u32 = 13;
pub const LR: u32 = 14;
pub const PC: u32 = 15;

/// Data-processing opcodes.
#[derive(Debug, Clone, Copy)]
#[repr(u32)]
pub enum Dp {
    And = 0,
    Eor = 1,
    Sub = 2,
    Rsb = 3,
    Add = 4,
    Adc = 5,
    Sbc = 6,
    Rsc = 7,
    Tst = 8,
    Teq = 9,
    Cmp = 10,
    Cmn = 11,
    Orr = 12,
    Mov = 13,
    Bic = 14,
    Mvn = 15,
}

/// Encode `imm` as an ARM rotated 8-bit immediate (`rot << 8 | imm8`).
pub fn encode_imm(imm: u32) -> Option<u32> {
    (0..16).find_map(|rot| {
        let v = imm.rotate_left(rot * 2);
        (v <= 0xFF).then_some((rot << 8) | v)
    })
}

/// ARM-mode assembler with forward/backward labels.
#[derive(Debug, Default, Clone)]
pub struct Assembler {
    base: u32,
    words: Vec<u32>,
    labels: HashMap<String, u32>,
    fixups: Vec<(usize, String, u32)>, // (index, label, cond|link bits)
}

impl Assembler {
    /// Start assembling code that will live at `base`.
    pub fn new(base: u32) -> Self {
        Self { base, ..Default::default() }
    }

    /// Address of the next instruction.
    pub fn here(&self) -> u32 {
        self.base + self.words.len() as u32 * 4
    }

    pub fn label(&mut self, name: &str) -> &mut Self {
        self.labels.insert(name.to_string(), self.here());
        self
    }

    pub fn word(&mut self, w: u32) -> &mut Self {
        self.words.push(w);
        self
    }

    pub fn dp_imm(&mut self, cond: u32, op: Dp, s: bool, rd: u32, rn: u32, imm: u32) -> &mut Self {
        let enc = encode_imm(imm).unwrap_or_else(|| panic!("immediate {imm:#x} not encodable"));
        let op = op as u32;
        let s = s || (8..=11).contains(&op);
        self.word(cond << 28 | 1 << 25 | op << 21 | (s as u32) << 20 | rn << 16 | rd << 12 | enc)
    }

    /// Data processing with a register operand shifted by an immediate.
    #[allow(clippy::too_many_arguments)]
    pub fn dp_reg(&mut self, cond: u32, op: Dp, s: bool, rd: u32, rn: u32, rm: u32, shift: u32, amount: u32) -> &mut Self {
        let op = op as u32;
        let s = s || (8..=11).contains(&op);
        self.word(cond << 28 | op << 21 | (s as u32) << 20 | rn << 16 | rd << 12 | (amount & 31) << 7 | (shift & 3) << 5 | rm)
    }

    pub fn mov_imm(&mut self, rd: u32, imm: u32) -> &mut Self {
        if encode_imm(imm).is_some() {
            self.dp_imm(AL, Dp::Mov, false, rd, 0, imm)
        } else if encode_imm(!imm).is_some() {
            self.dp_imm(AL, Dp::Mvn, false, rd, 0, !imm)
        } else {
            self.load_const(rd, imm)
        }
    }

    /// Materialise any 32-bit constant with MOV + up to three ORRs.
    pub fn load_const(&mut self, rd: u32, value: u32) -> &mut Self {
        if encode_imm(value).is_some() {
            return self.dp_imm(AL, Dp::Mov, false, rd, 0, value);
        }
        let mut first = true;
        for byte in 0..4 {
            let part = value & (0xFF << (byte * 8));
            if part == 0 {
                continue;
            }
            if first {
                self.dp_imm(AL, Dp::Mov, false, rd, 0, part);
                first = false;
            } else {
                self.dp_imm(AL, Dp::Orr, false, rd, rd, part);
            }
        }
        if first {
            self.dp_imm(AL, Dp::Mov, false, rd, 0, 0);
        }
        self
    }

    pub fn mov(&mut self, rd: u32, rm: u32) -> &mut Self {
        self.dp_reg(AL, Dp::Mov, false, rd, 0, rm, 0, 0)
    }

    pub fn add_imm(&mut self, rd: u32, rn: u32, imm: u32) -> &mut Self {
        self.dp_imm(AL, Dp::Add, false, rd, rn, imm)
    }

    pub fn sub_imm(&mut self, rd: u32, rn: u32, imm: u32) -> &mut Self {
        self.dp_imm(AL, Dp::Sub, false, rd, rn, imm)
    }

    pub fn subs_imm(&mut self, rd: u32, rn: u32, imm: u32) -> &mut Self {
        self.dp_imm(AL, Dp::Sub, true, rd, rn, imm)
    }

    pub fn cmp_imm(&mut self, rn: u32, imm: u32) -> &mut Self {
        self.dp_imm(AL, Dp::Cmp, true, 0, rn, imm)
    }

    /// `LDR/STR{B} rd, [rn, #±off]` (pre-indexed, no writeback).
    fn mem(&mut self, load: bool, byte: bool, rd: u32, rn: u32, off: i32) -> &mut Self {
        let up = off >= 0;
        let mag = off.unsigned_abs();
        assert!(mag < 4096);
        self.word(AL << 28 | 1 << 26 | 1 << 24 | (up as u32) << 23 | (byte as u32) << 22 | (load as u32) << 20 | rn << 16 | rd << 12 | mag)
    }

    pub fn ldr(&mut self, rd: u32, rn: u32, off: i32) -> &mut Self {
        self.mem(true, false, rd, rn, off)
    }
    pub fn str(&mut self, rd: u32, rn: u32, off: i32) -> &mut Self {
        self.mem(false, false, rd, rn, off)
    }
    pub fn ldrb(&mut self, rd: u32, rn: u32, off: i32) -> &mut Self {
        self.mem(true, true, rd, rn, off)
    }
    pub fn strb(&mut self, rd: u32, rn: u32, off: i32) -> &mut Self {
        self.mem(false, true, rd, rn, off)
    }

    /// `LDRH/STRH rd, [rn, #off]` with 0 <= off < 256.
    fn half(&mut self, load: bool, rd: u32, rn: u32, off: u32) -> &mut Self {
        assert!(off < 256);
        self.word(AL << 28 | 1 << 24 | 1 << 23 | 1 << 22 | (load as u32) << 20 | rn << 16 | rd << 12 | (off >> 4) << 8 | 0xB << 4 | (off & 0xF))
    }
    pub fn ldrh(&mut self, rd: u32, rn: u32, off: u32) -> &mut Self {
        self.half(true, rd, rn, off)
    }
    pub fn strh(&mut self, rd: u32, rn: u32, off: u32) -> &mut Self {
        self.half(false, rd, rn, off)
    }
    /// `STRH rd, [rn], #off` (post-increment).
    pub fn strh_post(&mut self, rd: u32, rn: u32, off: u32) -> &mut Self {
        assert!(off < 256);
        self.word(AL << 28 | 1 << 23 | 1 << 22 | rn << 16 | rd << 12 | (off >> 4) << 8 | 0xB << 4 | (off & 0xF))
    }

    /// `STMFD sp!, {list}`
    pub fn push(&mut self, list: u16) -> &mut Self {
        self.word(AL << 28 | 0x092D_0000 | list as u32)
    }
    /// `LDMFD sp!, {list}`
    pub fn pop(&mut self, list: u16) -> &mut Self {
        self.word(AL << 28 | 0x08BD_0000 | list as u32)
    }

    pub fn bx(&mut self, rm: u32) -> &mut Self {
        self.word(AL << 28 | 0x012F_FF10 | rm)
    }

    pub fn swi(&mut self, n: u32) -> &mut Self {
        self.word(AL << 28 | 0x0F00_0000 | (n & 0xFF_FFFF))
    }

    /// `MSR CPSR_c, #imm`
    pub fn msr_cpsr_c(&mut self, imm: u32) -> &mut Self {
        self.word(AL << 28 | 0x0321_F000 | encode_imm(imm).expect("encodable"))
    }

    /// `MRS rd, CPSR`
    pub fn mrs_cpsr(&mut self, rd: u32) -> &mut Self {
        self.word(AL << 28 | 0x010F_0000 | rd << 12)
    }

    pub fn mul(&mut self, rd: u32, rm: u32, rs: u32) -> &mut Self {
        self.word(AL << 28 | rd << 16 | rs << 8 | 0x90 | rm)
    }

    /// `SUBS PC, LR, #4` (return from IRQ).
    pub fn irq_return(&mut self) -> &mut Self {
        self.word(0xE25E_F004)
    }

    fn branch_to(&mut self, cond: u32, link: bool, label: &str) -> &mut Self {
        let bits = cond << 28 | 0x0A00_0000 | (link as u32) << 24;
        self.fixups.push((self.words.len(), label.to_string(), bits));
        self.word(0)
    }

    pub fn b(&mut self, cond: u32, label: &str) -> &mut Self {
        self.branch_to(cond, false, label)
    }

    pub fn bl(&mut self, label: &str) -> &mut Self {
        self.branch_to(AL, true, label)
    }

    /// Branch to an absolute address (must be within ±32 MiB).
    pub fn b_abs(&mut self, cond: u32, target: u32) -> &mut Self {
        let off = (target.wrapping_sub(self.here() + 8) as i32) >> 2;
        self.word(cond << 28 | 0x0A00_0000 | (off as u32 & 0x00FF_FFFF))
    }

    /// Resolve labels and return the machine code as little-endian bytes.
    pub fn finish(&self) -> Vec<u8> {
        let mut words = self.words.clone();
        for (idx, label, bits) in &self.fixups {
            let target = *self.labels.get(label).unwrap_or_else(|| panic!("undefined label {label}"));
            let pc = self.base + *idx as u32 * 4 + 8;
            let off = (target.wrapping_sub(pc) as i32) >> 2;
            words[*idx] = bits | (off as u32 & 0x00FF_FFFF);
        }
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    pub fn len_bytes(&self) -> usize {
        self.words.len() * 4
    }
}

/// Thumb encoders for tests.
pub mod thumb {
    pub fn mov_imm(rd: u16, imm: u16) -> u16 {
        0x2000 | rd << 8 | (imm & 0xFF)
    }
    pub fn add_imm(rd: u16, imm: u16) -> u16 {
        0x3000 | rd << 8 | (imm & 0xFF)
    }
    pub fn sub_imm(rd: u16, imm: u16) -> u16 {
        0x3800 | rd << 8 | (imm & 0xFF)
    }
    pub fn cmp_imm(rd: u16, imm: u16) -> u16 {
        0x2800 | rd << 8 | (imm & 0xFF)
    }
    pub fn lsl(rd: u16, rs: u16, n: u16) -> u16 {
        (n & 31) << 6 | rs << 3 | rd
    }
    pub fn add_reg(rd: u16, rs: u16, rn: u16) -> u16 {
        0x1800 | rn << 6 | rs << 3 | rd
    }
    pub fn alu(op: u16, rd: u16, rs: u16) -> u16 {
        0x4000 | op << 6 | rs << 3 | rd
    }
    pub fn str_imm(rd: u16, rb: u16, word_off: u16) -> u16 {
        0x6000 | (word_off & 31) << 6 | rb << 3 | rd
    }
    pub fn ldr_imm(rd: u16, rb: u16, word_off: u16) -> u16 {
        0x6800 | (word_off & 31) << 6 | rb << 3 | rd
    }
    pub fn push(list: u16, lr: bool) -> u16 {
        0xB400 | (lr as u16) << 8 | list
    }
    pub fn pop(list: u16, pc: bool) -> u16 {
        0xBC00 | (pc as u16) << 8 | list
    }
    pub fn bx(rs: u16) -> u16 {
        0x4700 | rs << 3
    }
    pub fn b_cond(cond: u16, half_off: i16) -> u16 {
        0xD000 | cond << 8 | (half_off as u16 & 0xFF)
    }
    pub fn b(half_off: i16) -> u16 {
        0xE000 | (half_off as u16 & 0x7FF)
    }
    /// `BL` as two halves; `off` is the byte offset from the prefix + 4.
    pub fn bl(off: i32) -> [u16; 2] {
        let o = off >> 1;
        [0xF000 | ((o >> 11) as u16 & 0x7FF), 0xF800 | (o as u16 & 0x7FF)]
    }
    pub fn swi(n: u16) -> u16 {
        0xDF00 | (n & 0xFF)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn immediates() {
        assert_eq!(encode_imm(0xFF), Some(0xFF));
        assert_eq!(encode_imm(0x0400_0000), Some(0x301));
        assert_eq!(encode_imm(0x101), None);
    }

    #[test]
    fn branch_fixups() {
        let mut a = Assembler::new(0x0800_0000);
        a.label("top").mov_imm(0, 1).b(AL, "top").b(NE, "end").label("end");
        let code = a.finish();
        let w1 = u32::from_le_bytes(code[4..8].try_into().unwrap());
        assert_eq!(w1, 0xEAFF_FFFD); // b top (pc+8 = 0x0C, target 0x00)
        let w2 = u32::from_le_bytes(code[8..12].try_into().unwrap());
        assert_eq!(w2, 0x1AFF_FFFF); // bne end (pc+8 = 0x10, target 0x0C)
    }
}
