//! Low-level emulation of the Macronix 16-bit audio DSP.
//!
//! Implements the register file (address/index/wrap registers, four
//! hardware stacks, two 40-bit accumulators, AX and product registers),
//! condition codes, the hardware loop machinery and the core instruction
//! set: immediate/register/memory moves, indirect loads/stores with
//! post-modify, accumulator arithmetic, shifts, branches, calls and loops.
//! The parallel "extended" load/store half of arithmetic opcodes and the
//! multiplier family are not executed yet and are logged when seen.
//!
//! Running real microcode needs the DSP IROM/DROM, which are Nintendo
//! firmware and are never bundled; the HLE mixer is the default.

pub const SR_C: u16 = 0x0001;
pub const SR_O: u16 = 0x0002;
pub const SR_Z: u16 = 0x0004;
pub const SR_S: u16 = 0x0008;
pub const SR_AS: u16 = 0x0010;
pub const SR_LZ: u16 = 0x0040;
pub const SR_SXM: u16 = 0x4000;

/// Hardware register addresses in data memory.
pub const DSCR: u16 = 0xFFC9;
pub const DSBL: u16 = 0xFFCB;
pub const DSPA: u16 = 0xFFCD;
pub const DSMAH: u16 = 0xFFCE;
pub const DSMAL: u16 = 0xFFCF;
pub const DIRQ: u16 = 0xFFFB;
pub const DMBH: u16 = 0xFFFC;
pub const DMBL: u16 = 0xFFFD;
pub const CMBH: u16 = 0xFFFE;
pub const CMBL: u16 = 0xFFFF;

/// Main memory as seen by DSP DMA (big-endian bytes).
pub trait DspMainMemory {
    fn read8(&self, addr: u32) -> u8;
    fn write8(&mut self, addr: u32, v: u8);
}

pub struct DspCore {
    pub ar: [u16; 4],
    pub ix: [u16; 4],
    pub wr: [u16; 4],
    pub stacks: [Vec<u16>; 4],
    /// 40-bit accumulators, sign-extended into i64.
    pub ac: [i64; 2],
    pub ax_l: [u16; 2],
    pub ax_h: [u16; 2],
    pub prod: i64,
    pub config: u16,
    pub sr: u16,
    pub pc: u16,
    pub iram: Vec<u16>,
    pub irom: Vec<u16>,
    pub dram: Vec<u16>,
    pub coef: Vec<u16>,
    pub halted: bool,
    /// DSP → CPU mailbox (bit 31 = valid).
    pub dsp_mail: u32,
    /// CPU → DSP mailbox (bit 31 = pending).
    pub cpu_mail: u32,
    pub interrupt_cpu: bool,
    hw: [u16; 0x100],
    pub cycles: u64,
}

impl Default for DspCore {
    fn default() -> Self {
        Self::new()
    }
}

#[inline]
fn sext40(v: i64) -> i64 {
    (v << 24) >> 24
}

impl DspCore {
    pub fn new() -> Self {
        Self {
            ar: [0; 4],
            ix: [0; 4],
            wr: [0xFFFF; 4],
            stacks: Default::default(),
            ac: [0; 2],
            ax_l: [0; 2],
            ax_h: [0; 2],
            prod: 0,
            config: 0xFF,
            sr: 0,
            pc: 0x8000,
            iram: vec![0; 0x1000],
            irom: vec![0; 0x1000],
            dram: vec![0; 0x1000],
            coef: vec![0; 0x800],
            halted: false,
            dsp_mail: 0,
            cpu_mail: 0,
            interrupt_cpu: false,
            hw: [0; 0x100],
            cycles: 0,
        }
    }

    pub fn reset(&mut self, from_iram: bool) {
        let (iram, irom, dram, coef) = (
            std::mem::take(&mut self.iram),
            std::mem::take(&mut self.irom),
            std::mem::take(&mut self.dram),
            std::mem::take(&mut self.coef),
        );
        *self = Self { iram, irom, dram, coef, ..Self::new() };
        self.pc = if from_iram { 0 } else { 0x8000 };
    }

    // ------------------------------------------------------------- memory

    fn imem(&self, addr: u16) -> u16 {
        match addr {
            0x0000..=0x0FFF => self.iram[addr as usize],
            0x8000..=0x8FFF => self.irom[(addr - 0x8000) as usize],
            _ => 0,
        }
    }

    pub fn read_dmem(&mut self, addr: u16) -> u16 {
        match addr {
            0x0000..=0x0FFF => self.dram[addr as usize],
            0x1000..=0x17FF => self.coef[(addr - 0x1000) as usize],
            DMBH => (self.dsp_mail >> 16) as u16,
            DMBL => self.dsp_mail as u16,
            CMBH => (self.cpu_mail >> 16) as u16,
            CMBL => {
                let v = self.cpu_mail as u16;
                self.cpu_mail &= 0x7FFF_FFFF;
                v
            }
            0xFF00..=0xFFFF => self.hw[(addr & 0xFF) as usize],
            _ => 0,
        }
    }

    pub fn write_dmem(&mut self, addr: u16, v: u16, main: &mut dyn DspMainMemory) {
        match addr {
            0x0000..=0x0FFF => self.dram[addr as usize] = v,
            DMBH => self.dsp_mail = (self.dsp_mail & 0xFFFF) | ((v as u32 & 0x7FFF) << 16),
            DMBL => self.dsp_mail = (self.dsp_mail & 0x7FFF_0000) | v as u32 | 0x8000_0000,
            DIRQ => {
                if v & 1 != 0 {
                    self.interrupt_cpu = true;
                }
            }
            DSBL => {
                self.hw[(addr & 0xFF) as usize] = v;
                self.dma(v, main);
            }
            0xFF00..=0xFFFF => self.hw[(addr & 0xFF) as usize] = v,
            _ => {}
        }
    }

    /// DSP DMA between main memory and DSP IRAM/DRAM.
    fn dma(&mut self, len: u16, main: &mut dyn DspMainMemory) {
        let ctrl = self.hw[(DSCR & 0xFF) as usize];
        let mut mem_addr = (self.hw[(DSMAH & 0xFF) as usize] as u32) << 16 | self.hw[(DSMAL & 0xFF) as usize] as u32;
        let mut dsp_addr = self.hw[(DSPA & 0xFF) as usize];
        let to_dsp = ctrl & 1 == 0;
        let imem = ctrl & 2 != 0;
        for _ in 0..len / 2 {
            if to_dsp {
                let w = u16::from_be_bytes([main.read8(mem_addr), main.read8(mem_addr + 1)]);
                if imem {
                    if let Some(slot) = self.iram.get_mut(dsp_addr as usize) {
                        *slot = w;
                    }
                } else if let Some(slot) = self.dram.get_mut(dsp_addr as usize) {
                    *slot = w;
                }
            } else {
                let w = if imem { self.imem(dsp_addr) } else { self.dram.get(dsp_addr as usize).copied().unwrap_or(0) };
                let [hi, lo] = w.to_be_bytes();
                main.write8(mem_addr, hi);
                main.write8(mem_addr + 1, lo);
            }
            mem_addr += 2;
            dsp_addr = dsp_addr.wrapping_add(1);
        }
        self.hw[(DSCR & 0xFF) as usize] &= !4; // not busy
    }

    // ----------------------------------------------------------- registers

    pub fn reg(&mut self, r: u16) -> u16 {
        let r = r & 0x1F;
        match r {
            0..=3 => self.ar[r as usize],
            4..=7 => self.ix[(r - 4) as usize],
            8..=11 => self.wr[(r - 8) as usize],
            12..=15 => self.stacks[(r - 12) as usize].pop().unwrap_or(0),
            16 | 17 => (((self.ac[(r - 16) as usize] >> 32) as i8) as i16) as u16,
            18 => self.config,
            19 => self.sr,
            20 => self.prod as u16,
            21 => (self.prod >> 16) as u16,
            22 => (self.prod >> 32) as u16 & 0xFF,
            23 => 0,
            24 | 25 => self.ax_l[(r - 24) as usize],
            26 | 27 => self.ax_h[(r - 26) as usize],
            28 | 29 => self.ac[(r - 28) as usize] as u16,
            _ => (self.ac[(r - 30) as usize] >> 16) as u16,
        }
    }

    pub fn set_reg(&mut self, r: u16, v: u16) {
        let r = r & 0x1F;
        match r {
            0..=3 => self.ar[r as usize] = v,
            4..=7 => self.ix[(r - 4) as usize] = v,
            8..=11 => self.wr[(r - 8) as usize] = v,
            12..=15 => self.stacks[(r - 12) as usize].push(v),
            16 | 17 => {
                let a = &mut self.ac[(r - 16) as usize];
                *a = sext40((*a & 0xFFFF_FFFF) | ((v as i8 as i64) << 32));
            }
            18 => self.config = v,
            19 => self.sr = v,
            20 => self.prod = (self.prod & !0xFFFF) | v as i64,
            21 => self.prod = (self.prod & !0xFFFF_0000) | (v as i64) << 16,
            22 => self.prod = sext40((self.prod & 0xFFFF_FFFF) | ((v as i64 & 0xFF) << 32)),
            23 => {}
            24 | 25 => self.ax_l[(r - 24) as usize] = v,
            26 | 27 => self.ax_h[(r - 26) as usize] = v,
            28 | 29 => {
                let a = &mut self.ac[(r - 28) as usize];
                *a = (*a & !0xFFFF) | v as i64;
            }
            _ => {
                let i = (r - 30) as usize;
                self.ac[i] = if self.sr & SR_SXM != 0 {
                    (v as i16 as i64) << 16
                } else {
                    sext40((self.ac[i] & !0xFFFF_0000) | (v as i64) << 16)
                };
            }
        }
    }

    fn ax(&self, i: usize) -> i64 {
        ((self.ax_h[i] as u32) << 16 | self.ax_l[i] as u32) as i32 as i64
    }

    fn set_flags(&mut self, r: i64, carry: bool, overflow: bool) {
        let mut sr = self.sr & !(SR_C | SR_O | SR_Z | SR_S | SR_AS | SR_LZ);
        if carry {
            sr |= SR_C;
        }
        if overflow {
            sr |= SR_O;
        }
        if r == 0 {
            sr |= SR_Z;
        }
        if r < 0 {
            sr |= SR_S;
        }
        if r != (r as i32) as i64 {
            sr |= SR_AS;
        }
        let top = ((r >> 30) & 3) as u8;
        if top == 0 || top == 3 {
            sr |= SR_LZ;
        }
        self.sr = sr;
    }

    fn add40(&mut self, a: i64, b: i64) -> i64 {
        let r = sext40(a + b);
        let carry = (a as u64 & 0xFF_FFFF_FFFF) + (b as u64 & 0xFF_FFFF_FFFF) > 0xFF_FFFF_FFFF;
        let overflow = (a < 0) == (b < 0) && (r < 0) != (a < 0);
        self.set_flags(r, carry, overflow);
        r
    }

    fn sub40(&mut self, a: i64, b: i64) -> i64 {
        let r = sext40(a - b);
        let carry = (a as u64 & 0xFF_FFFF_FFFF) >= (b as u64 & 0xFF_FFFF_FFFF);
        let overflow = (a < 0) != (b < 0) && (r < 0) != (a < 0);
        self.set_flags(r, carry, overflow);
        r
    }

    fn condition(&self, cc: u16) -> bool {
        let f = |b: u16| self.sr & b != 0;
        let os = f(SR_O) != f(SR_S);
        match cc & 0xF {
            0x0 => !os,
            0x1 => os,
            0x2 => !f(SR_Z) && !os,
            0x3 => f(SR_Z) || os,
            0x4 => !f(SR_Z),
            0x5 => f(SR_Z),
            0x6 => !f(SR_C),
            0x7 => f(SR_C),
            0x8 => !f(SR_AS),
            0x9 => f(SR_AS),
            0xA => (f(SR_AS) || f(SR_LZ)) && !f(SR_Z),
            0xB => !(f(SR_AS) || f(SR_LZ)) || f(SR_Z),
            0xC => !f(SR_LZ),
            0xD => f(SR_LZ),
            0xE => f(SR_O),
            _ => true,
        }
    }

    fn fetch(&mut self) -> u16 {
        let w = self.imem(self.pc);
        self.pc = self.pc.wrapping_add(1);
        w
    }

    // ---------------------------------------------------------- execution

    pub fn step(&mut self, main: &mut dyn DspMainMemory) {
        if self.halted {
            return;
        }
        self.cycles += 1;
        let at = self.pc;
        let op = self.fetch();
        self.execute(op, main);
        // Hardware loops: the last instruction of a loop body was just run.
        if let (Some(&end), Some(&count)) = (self.stacks[2].last(), self.stacks[3].last()) {
            if at == end && !self.halted {
                if count > 1 {
                    *self.stacks[3].last_mut().unwrap() = count - 1;
                    self.pc = *self.stacks[0].last().unwrap_or(&self.pc);
                } else {
                    self.stacks[0].pop();
                    self.stacks[2].pop();
                    self.stacks[3].pop();
                }
            }
        }
    }

    /// Run until halted or `max` instructions.
    pub fn run(&mut self, main: &mut dyn DspMainMemory, max: u64) {
        for _ in 0..max {
            if self.halted {
                break;
            }
            self.step(main);
        }
    }

    fn start_loop(&mut self, count: u16, body_start: u16, body_end: u16) {
        if count == 0 {
            self.pc = body_end.wrapping_add(1);
            return;
        }
        self.stacks[0].push(body_start);
        self.stacks[2].push(body_end);
        self.stacks[3].push(count);
    }

    fn indirect_post(&mut self, ar: usize, mode: u16) {
        match mode {
            1 => self.ar[ar] = self.ar[ar].wrapping_sub(1),
            2 => self.ar[ar] = self.ar[ar].wrapping_add(1),
            3 => self.ar[ar] = self.ar[ar].wrapping_add(self.ix[ar]),
            _ => {}
        }
    }

    fn execute(&mut self, op: u16, main: &mut dyn DspMainMemory) {
        match op >> 12 {
            0x0 => self.op0(op, main),
            0x1 => self.op1(op, main),
            0x2 => {
                let r = 0x18 + ((op >> 8) & 7);
                let addr = (self.config << 8) | (op & 0xFF);
                if op & 0x0800 == 0 {
                    let v = self.read_dmem(addr);
                    self.set_reg(r, v);
                } else {
                    let v = self.reg(r);
                    self.write_dmem(addr, v, main);
                }
            }
            _ => self.arith(op),
        }
    }

    fn op0(&mut self, op: u16, main: &mut dyn DspMainMemory) {
        let d = (op >> 8) & 1;
        match op {
            0x0000..=0x0003 => {}
            0x0004..=0x0007 => self.ar[(op & 3) as usize] = self.ar[(op & 3) as usize].wrapping_sub(1),
            0x0008..=0x000B => self.ar[(op & 3) as usize] = self.ar[(op & 3) as usize].wrapping_add(1),
            0x0010..=0x001F => {
                let (dd, s) = ((op & 3) as usize, ((op >> 2) & 3) as usize);
                self.ar[dd] = self.ar[dd].wrapping_add(self.ix[s]);
            }
            0x0021 => self.halted = true,
            _ if op & 0xFFE0 == 0x0040 => {
                let count = self.reg(op & 0x1F);
                let pc = self.pc;
                self.start_loop(count, pc, pc);
            }
            _ if op & 0xFFE0 == 0x0060 => {
                let count = self.reg(op & 0x1F);
                let end = self.fetch();
                let pc = self.pc;
                self.start_loop(count, pc, end);
            }
            _ if op & 0xFFE0 == 0x0080 => {
                let imm = self.fetch();
                self.set_reg(op & 0x1F, imm);
            }
            _ if op & 0xFFE0 == 0x00C0 => {
                let addr = self.fetch();
                let v = self.read_dmem(addr);
                self.set_reg(op & 0x1F, v);
            }
            _ if op & 0xFFE0 == 0x00E0 => {
                let addr = self.fetch();
                let v = self.reg(op & 0x1F);
                self.write_dmem(addr, v, main);
            }
            _ if op & 0xFEFF == 0x0200 => {
                let imm = self.fetch() as i16 as i64;
                let a = self.ac[d as usize];
                self.ac[d as usize] = self.add40(a, imm << 16);
            }
            _ if op & 0xFEFF == 0x0220 || op & 0xFEFF == 0x0240 || op & 0xFEFF == 0x0260 => {
                let imm = self.fetch() as i64;
                let a = self.ac[d as usize];
                let m = (a >> 16) & 0xFFFF;
                let nm = match op & 0xFEFF {
                    0x0220 => m ^ imm,
                    0x0240 => m & imm,
                    _ => m | imm,
                };
                let r = sext40((a & !0xFFFF_0000) | (nm << 16));
                self.ac[d as usize] = r;
                self.set_flags(r, false, false);
            }
            _ if op & 0xFEFF == 0x0280 => {
                let imm = self.fetch() as i16 as i64;
                let a = self.ac[d as usize];
                self.sub40(a, imm << 16);
            }
            _ if op & 0xFFF0 == 0x0290 => {
                let target = self.fetch();
                if self.condition(op) {
                    self.pc = target;
                }
            }
            _ if op & 0xFFF0 == 0x02B0 => {
                let target = self.fetch();
                if self.condition(op) {
                    self.stacks[0].push(self.pc);
                    self.pc = target;
                }
            }
            _ if op & 0xFFF0 == 0x02D0 => {
                if self.condition(op) {
                    self.pc = self.stacks[0].pop().unwrap_or(0);
                }
            }
            _ if op & 0xFFF0 == 0x02F0 => {
                if self.condition(op) {
                    self.sr = self.stacks[1].pop().unwrap_or(0);
                    self.pc = self.stacks[0].pop().unwrap_or(0);
                }
            }
            _ if op & 0xFE00 == 0x0400 => {
                let a = self.ac[d as usize];
                self.ac[d as usize] = self.add40(a, (op as u8 as i8 as i64) << 16);
            }
            _ if op & 0xFE00 == 0x0600 => {
                let a = self.ac[d as usize];
                self.sub40(a, (op as u8 as i8 as i64) << 16);
            }
            _ if op & 0xF800 == 0x0800 => {
                let r = 0x18 + ((op >> 8) & 7);
                self.set_reg(r, op as u8 as i8 as i16 as u16);
            }
            _ => tracing::warn!("unimplemented DSP opcode {op:#06x} at {:#06x}", self.pc.wrapping_sub(1)),
        }
    }

    fn op1(&mut self, op: u16, main: &mut dyn DspMainMemory) {
        match op & 0xFF00 {
            0x1000 => {
                let pc = self.pc;
                self.start_loop(op & 0xFF, pc, pc);
                return;
            }
            0x1100 => {
                let end = self.fetch();
                let pc = self.pc;
                self.start_loop(op & 0xFF, pc, end);
                return;
            }
            0x1200 => {
                self.sr &= !(1 << ((op & 7) + 6));
                return;
            }
            0x1300 => {
                self.sr |= 1 << ((op & 7) + 6);
                return;
            }
            0x1600 => {
                let imm = self.fetch();
                self.write_dmem(0xFF00 | (op & 0xFF), imm, main);
                return;
            }
            _ => {}
        }
        match op & 0xFEC0 {
            0x1400 | 0x1440 | 0x1480 | 0x14C0 => {
                let r = ((op >> 8) & 1) as usize;
                let n = (op & 0x3F) as u32;
                let a = self.ac[r];
                let res = match op & 0xFEC0 {
                    0x1400 => sext40(a << n),
                    0x1440 => sext40(((a as u64 & 0xFF_FFFF_FFFF) >> ((64 - n) & 0x3F)) as i64),
                    0x1480 => sext40(a << n),
                    _ => a >> ((64 - n) & 0x3F),
                };
                self.ac[r] = res;
                self.set_flags(res, false, false);
                return;
            }
            _ => {}
        }
        if op & 0xFF10 == 0x1700 || op & 0xFF10 == 0x1710 {
            let target = self.reg((op >> 5) & 7);
            if self.condition(op) {
                if op & 0x10 != 0 {
                    self.stacks[0].push(self.pc);
                }
                self.pc = target;
            }
            return;
        }
        if op & 0xFC00 == 0x1C00 {
            // MRR $D, $S
            let v = self.reg(op & 0x1F);
            self.set_reg((op >> 5) & 0x1F, v);
        } else if op & 0xFE00 == 0x1800 {
            // LRR / LRRD / LRRI / LRRN: $D <- @$S with post-modify
            let s = ((op >> 5) & 3) as usize;
            let addr = self.ar[s];
            let v = self.read_dmem(addr);
            self.set_reg(op & 0x1F, v);
            self.indirect_post(s, (op >> 7) & 3);
        } else if op & 0xFE00 == 0x1A00 {
            // SRR / SRRD / SRRI / SRRN: @$D <- $S with post-modify
            let dr = ((op >> 5) & 3) as usize;
            let v = self.reg(op & 0x1F);
            let addr = self.ar[dr];
            self.write_dmem(addr, v, main);
            self.indirect_post(dr, (op >> 7) & 3);
        } else {
            tracing::warn!("unimplemented DSP opcode {op:#06x}");
        }
    }

    fn arith(&mut self, op: u16) {
        if op & 0xFF != 0 && op >> 12 >= 4 {
            tracing::warn!("DSP extended opcode part {:#04x} ignored", op & 0xFF);
        }
        let d = ((op >> 8) & 1) as usize;
        match op & 0xFE00 {
            0x4C00 => {
                let (a, b) = (self.ac[d], self.ac[1 - d]);
                self.ac[d] = self.add40(a, b);
                return;
            }
            0x5C00 => {
                let (a, b) = (self.ac[d], self.ac[1 - d]);
                self.ac[d] = self.sub40(a, b);
                return;
            }
            0x6C00 => {
                let v = self.ac[1 - d];
                self.ac[d] = v;
                self.set_flags(v, false, false);
                return;
            }
            0x7400 | 0x7600 | 0x7800 | 0x7A00 => {
                let a = self.ac[d];
                self.ac[d] = match op & 0xFE00 {
                    0x7400 => self.add40(a, 0x10000),
                    0x7600 => self.add40(a, 1),
                    0x7800 => self.sub40(a, 0x10000),
                    _ => self.sub40(a, 1),
                };
                return;
            }
            0x7C00 => {
                let a = self.ac[d];
                self.ac[d] = self.sub40(0, a);
                return;
            }
            _ => {}
        }
        match op & 0xF800 {
            0x4000 | 0x5000 | 0x6000 => {
                let s = 0x18 + ((op >> 9) & 3);
                let v = (self.reg(s) as i16 as i64) << 16;
                let a = self.ac[d];
                self.ac[d] = match op & 0xF800 {
                    0x4000 => self.add40(a, v),
                    0x5000 => self.sub40(a, v),
                    _ => {
                        self.set_flags(v, false, false);
                        v
                    }
                };
                return;
            }
            _ => {}
        }
        match op & 0xFC00 {
            0x4800 | 0x5800 | 0x6800 => {
                let x = self.ax(((op >> 9) & 1) as usize);
                let a = self.ac[d];
                self.ac[d] = match op & 0xFC00 {
                    0x4800 => self.add40(a, x),
                    0x5800 => self.sub40(a, x),
                    _ => {
                        self.set_flags(x, false, false);
                        x
                    }
                };
                return;
            }
            _ => {}
        }
        match op & 0xF700 {
            0x8100 => {
                let r = ((op >> 11) & 1) as usize;
                self.ac[r] = 0;
                self.set_flags(0, false, false);
            }
            0x8200 if op & 0xFF00 == 0x8200 => {
                let (a, b) = (self.ac[0], self.ac[1]);
                self.sub40(a, b);
            }
            0xB100 => {
                let r = ((op >> 11) & 1) as usize;
                let v = self.ac[r];
                self.set_flags(v, false, false);
            }
            0x8000 => {} // NX: extension only
            _ => tracing::warn!("unimplemented DSP arithmetic opcode {op:#06x}"),
        }
    }

    /// CPU writes its mailbox (high then low).
    pub fn cpu_write_mail(&mut self, v: u32) {
        self.cpu_mail = v | 0x8000_0000;
    }

    /// CPU reads the DSP mailbox; reading the low half acknowledges it.
    pub fn cpu_read_mail(&mut self) -> Option<u32> {
        if self.dsp_mail & 0x8000_0000 != 0 {
            let v = self.dsp_mail & 0x7FFF_FFFF;
            self.dsp_mail = v;
            Some(v)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Main(Vec<u8>);
    impl DspMainMemory for Main {
        fn read8(&self, a: u32) -> u8 {
            self.0[a as usize]
        }
        fn write8(&mut self, a: u32, v: u8) {
            self.0[a as usize] = v;
        }
    }

    fn core_with(prog: &[u16]) -> DspCore {
        let mut d = DspCore::new();
        d.iram[..prog.len()].copy_from_slice(prog);
        d.reset(true);
        d
    }

    const AC0M: u16 = 30;
    const AR0: u16 = 0;
    const HALT: u16 = 0x0021;

    #[test]
    fn immediate_loads_and_accumulator_math() {
        let prog = [
            0x0080 | AC0M,
            0x0010, // lri $ac0.m, #0x10
            0x0200,
            0x0005, // addi $ac0, #5
            0x0081 | 0x1E,
            0x0003, // lri $ac1.m, #3
            0x4C00, // add $ac0, $ac1  → 0x18 << 16
            0x00E0 | AC0M,
            0x0100, // sr @0x100, $ac0.m
            HALT,
        ];
        let mut d = core_with(&prog);
        d.run(&mut Main(vec![]), 100);
        assert!(d.halted);
        assert_eq!(d.dram[0x100], 0x18);
        assert_eq!(d.ac[0], 0x18 << 16);
    }

    #[test]
    fn block_loop_sums_values() {
        // ac0 = sum of 1..=10 via BLOOPI and INC on ac1
        let prog = [
            0x8100, // clr $ac0
            0x8900, // clr $ac1
            0x110A,
            0x0005,         // bloopi #10, end=5
            0x7600 | 0x100, // 4: inc $ac1
            0x4C00,         // 5: add $ac0, $ac1
            HALT,
        ];
        let mut d = core_with(&prog);
        d.run(&mut Main(vec![]), 200);
        assert!(d.halted);
        assert_eq!(d.ac[0], 55);
        assert!(d.stacks.iter().all(|s| s.is_empty()), "loop stacks unwound");
    }

    #[test]
    fn indirect_store_with_post_increment_and_conditional_jump() {
        // store 0..4 to dram[0x200..], loop with CMPI / JNZ
        let prog = [
            0x0080 | AR0,
            0x0200,        // lri $ar0, #0x200
            0x8100,        // clr $ac0
            0x1B00 | AC0M, // 3: srri @$ar0, $ac0.m  (post-increment)
            0x7400,        // incm $ac0
            0x0280,
            0x0005, // cmpi $ac0, #5
            0x0294,
            0x0003, // jnz 3
            HALT,
        ];
        let mut d = core_with(&prog);
        d.run(&mut Main(vec![]), 200);
        assert!(d.halted);
        assert_eq!(&d.dram[0x200..0x205], &[0, 1, 2, 3, 4]);
        assert_eq!(d.ar[0], 0x205);
    }

    #[test]
    fn mailbox_echo_and_dma() {
        // Wait for CPU mail, write it back + 1 into the DSP mailbox.
        let prog = [
            0x00C0 | 0x1C,
            CMBH, // lr $ac0.l, @CMBH (pending flag)
            0x00C0 | 0x1E,
            CMBL,   // lr $ac0.m, @CMBL (acknowledges)
            0x0401, // addis $ac0, #1
            0x00E0 | 0x1E,
            DMBH, // sr @DMBH, $ac0.m
            0x00E0 | 0x1E,
            DMBL, // sr @DMBL, $ac0.m → mail valid
            HALT,
        ];
        let mut d = core_with(&prog);
        d.cpu_write_mail(0x0000_1233);
        d.run(&mut Main(vec![]), 100);
        let mail = d.cpu_read_mail().expect("DSP answered");
        assert_eq!(mail & 0xFFFF, 0x1234);
        assert!(d.cpu_read_mail().is_none(), "read acknowledges");

        // DMA 8 bytes from main memory into DRAM
        let mut main = Main(vec![0; 0x100]);
        main.0[0x40..0x48].copy_from_slice(&[0, 1, 0, 2, 0, 3, 0, 4]);
        let mut d = DspCore::new();
        d.write_dmem(DSMAH, 0, &mut main);
        d.write_dmem(DSMAL, 0x40, &mut main);
        d.write_dmem(DSPA, 0x10, &mut main);
        d.write_dmem(DSCR, 0, &mut main);
        d.write_dmem(DSBL, 8, &mut main);
        assert_eq!(&d.dram[0x10..0x14], &[1, 2, 3, 4]);
    }
}
