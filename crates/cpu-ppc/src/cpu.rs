//! Gekko / Broadway register file, exceptions, MMU glue and interpreter.

use crate::mmu::{page_table_walk, Bat, SoftTlb};
use crate::paired::{dequantize, dequantize_pair, ps_arith, quantize, round_single, PsOp, Quant};
use kd_core::{SaveState, StateReader, StateResult, StateWriter};

pub const MSR_POW: u32 = 1 << 18;
pub const MSR_ILE: u32 = 1 << 16;
pub const MSR_EE: u32 = 1 << 15;
pub const MSR_PR: u32 = 1 << 14;
pub const MSR_FP: u32 = 1 << 13;
pub const MSR_ME: u32 = 1 << 12;
pub const MSR_IP: u32 = 1 << 6;
pub const MSR_IR: u32 = 1 << 5;
pub const MSR_DR: u32 = 1 << 4;
pub const MSR_RI: u32 = 1 << 1;
pub const MSR_LE: u32 = 1;

pub const XER_SO: u32 = 1 << 31;
pub const XER_OV: u32 = 1 << 30;
pub const XER_CA: u32 = 1 << 29;

pub const SPR_XER: usize = 1;
pub const SPR_LR: usize = 8;
pub const SPR_CTR: usize = 9;
pub const SPR_DSISR: usize = 18;
pub const SPR_DAR: usize = 19;
pub const SPR_DEC: usize = 22;
pub const SPR_SDR1: usize = 25;
pub const SPR_SRR0: usize = 26;
pub const SPR_SRR1: usize = 27;
pub const SPR_TBL_R: usize = 268;
pub const SPR_TBU_R: usize = 269;
pub const SPR_TBL_W: usize = 284;
pub const SPR_TBU_W: usize = 285;
pub const SPR_PVR: usize = 287;
pub const SPR_GQR0: usize = 912;
pub const SPR_HID2: usize = 920;
pub const SPR_WPAR: usize = 921;
pub const SPR_HID0: usize = 1008;
pub const SPR_HID4: usize = 1011;

pub const EXC_DSI: u32 = 0x300;
pub const EXC_ISI: u32 = 0x400;
pub const EXC_EXTERNAL: u32 = 0x500;
pub const EXC_ALIGNMENT: u32 = 0x600;
pub const EXC_PROGRAM: u32 = 0x700;
pub const EXC_FP_UNAVAILABLE: u32 = 0x800;
pub const EXC_DECREMENTER: u32 = 0x900;
pub const EXC_SYSCALL: u32 = 0xC00;

/// CPU timebase ticks once every 12 core cycles (486 MHz / 40.5 MHz on
/// GameCube; Wii keeps the same ratio).
pub const TB_RATIO: u32 = 12;

/// Physical memory interface. All accesses are big-endian and naturally
/// aligned (the CPU splits unaligned accesses).
pub trait PpcBus {
    fn read8(&mut self, pa: u32) -> u8;
    fn read16(&mut self, pa: u32) -> u16;
    fn read32(&mut self, pa: u32) -> u32;
    fn read64(&mut self, pa: u32) -> u64 {
        (self.read32(pa) as u64) << 32 | self.read32(pa + 4) as u64
    }
    fn write8(&mut self, pa: u32, v: u8);
    fn write16(&mut self, pa: u32, v: u16);
    fn write32(&mut self, pa: u32, v: u32);
    fn write64(&mut self, pa: u32, v: u64) {
        self.write32(pa, (v >> 32) as u32);
        self.write32(pa + 4, v as u32);
    }
    /// `dcbz` / `dcbz_l`: clear a 32-byte cache block.
    fn zero_block(&mut self, pa: u32) {
        for i in 0..8 {
            self.write32(pa + i * 4, 0);
        }
    }
    /// `icbi`: instructions at `pa` may have changed (JIT invalidation).
    fn invalidate_icache(&mut self, _pa: u32) {}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PpcModel {
    Gekko,
    Broadway,
}

#[derive(Clone)]
pub struct Ppc {
    pub model: PpcModel,
    pub gpr: [u32; 32],
    /// Paired-single view of the FPRs: [ps0, ps1].
    pub fpr: [[f64; 2]; 32],
    pub pc: u32,
    npc: u32,
    pub cr: u32,
    pub msr: u32,
    pub fpscr: u32,
    /// All SPRs; LR, CTR, XER, SRR0/1, DEC, GQRs, HIDs and BATs live here.
    pub spr: Box<[u32]>,
    pub sr: [u32; 16],
    pub tb: u64,
    tb_accum: u32,
    pub ibat: [Bat; 8],
    pub dbat: [Bat; 8],
    pub itlb: SoftTlb,
    pub dtlb: SoftTlb,
    /// External interrupt line from the Processor Interface.
    pub external_interrupt: bool,
    dec_pending: bool,
    reservation: Option<u32>,
    exception_taken: bool,
    pub cycles: u64,
    pub retired: u64,
}

impl std::fmt::Debug for Ppc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Ppc {{ pc: {:#010x}, msr: {:#x}, lr: {:#x}, gpr: {:x?} }}", self.pc, self.msr, self.lr(), self.gpr)
    }
}

#[inline]
fn rd(i: u32) -> usize {
    ((i >> 21) & 31) as usize
}
#[inline]
fn ra(i: u32) -> usize {
    ((i >> 16) & 31) as usize
}
#[inline]
fn rb(i: u32) -> usize {
    ((i >> 11) & 31) as usize
}
#[inline]
fn rc_reg(i: u32) -> usize {
    ((i >> 6) & 31) as usize
}
#[inline]
fn simm(i: u32) -> u32 {
    i as i16 as i32 as u32
}

/// PowerPC rotate mask for MB..ME (big-endian bit numbering).
#[inline]
pub fn rot_mask(mb: u32, me: u32) -> u32 {
    let a = u32::MAX >> mb;
    let b = u32::MAX << (31 - me);
    if mb <= me {
        a & b
    } else {
        a | b
    }
}

impl Ppc {
    pub fn new(model: PpcModel) -> Self {
        let mut spr = vec![0u32; 1024].into_boxed_slice();
        spr[SPR_PVR] = match model {
            PpcModel::Gekko => 0x0008_3214,
            PpcModel::Broadway => 0x0008_7102,
        };
        Self {
            model,
            gpr: [0; 32],
            fpr: [[0.0; 2]; 32],
            pc: 0xFFF0_0100,
            npc: 0,
            cr: 0,
            msr: MSR_IP,
            fpscr: 0,
            spr,
            sr: [0; 16],
            tb: 0,
            tb_accum: 0,
            ibat: [Bat::default(); 8],
            dbat: [Bat::default(); 8],
            itlb: SoftTlb::new(),
            dtlb: SoftTlb::new(),
            external_interrupt: false,
            dec_pending: false,
            reservation: None,
            exception_taken: false,
            cycles: 0,
            retired: 0,
        }
    }

    // ------------------------------------------------------------- SPRs

    #[inline]
    pub fn lr(&self) -> u32 {
        self.spr[SPR_LR]
    }
    #[inline]
    pub fn set_lr(&mut self, v: u32) {
        self.spr[SPR_LR] = v;
    }
    #[inline]
    pub fn ctr(&self) -> u32 {
        self.spr[SPR_CTR]
    }
    #[inline]
    pub fn xer(&self) -> u32 {
        self.spr[SPR_XER]
    }
    pub fn gqr(&self, i: usize) -> u32 {
        self.spr[SPR_GQR0 + i]
    }
    pub fn hid2(&self) -> u32 {
        self.spr[SPR_HID2]
    }

    #[inline]
    fn ca(&self) -> u32 {
        (self.xer() >> 29) & 1
    }
    #[inline]
    fn set_ca(&mut self, c: bool) {
        if c {
            self.spr[SPR_XER] |= XER_CA;
        } else {
            self.spr[SPR_XER] &= !XER_CA;
        }
    }
    fn set_ov(&mut self, ov: bool) {
        if ov {
            self.spr[SPR_XER] |= XER_OV | XER_SO;
        } else {
            self.spr[SPR_XER] &= !XER_OV;
        }
    }

    /// Write an SPR with side effects (BAT/TLB maintenance).
    pub fn write_spr(&mut self, n: usize, v: u32) {
        match n {
            SPR_TBL_W => self.tb = (self.tb & !0xFFFF_FFFF) | v as u64,
            SPR_TBU_W => self.tb = (self.tb & 0xFFFF_FFFF) | (v as u64) << 32,
            SPR_PVR => {}
            _ => self.spr[n] = v,
        }
        if (528..=543).contains(&n) || (560..=575).contains(&n) || n == SPR_HID4 || n == SPR_SDR1 {
            self.rebuild_bats();
        }
    }

    pub fn read_spr(&self, n: usize) -> u32 {
        match n {
            SPR_TBL_R => self.tb as u32,
            SPR_TBU_R => (self.tb >> 32) as u32,
            SPR_WPAR => self.spr[n] & !0x1F, // FIFO never reports "not empty"
            _ => self.spr[n],
        }
    }

    fn rebuild_bats(&mut self) {
        for i in 0..4 {
            self.ibat[i] = Bat { upper: self.spr[528 + i * 2], lower: self.spr[529 + i * 2] };
            self.dbat[i] = Bat { upper: self.spr[536 + i * 2], lower: self.spr[537 + i * 2] };
            self.ibat[4 + i] = Bat { upper: self.spr[560 + i * 2], lower: self.spr[561 + i * 2] };
            self.dbat[4 + i] = Bat { upper: self.spr[568 + i * 2], lower: self.spr[569 + i * 2] };
        }
        self.itlb.flush();
        self.dtlb.flush();
    }

    fn bat_count(&self) -> usize {
        // Broadway's extra BATs are enabled by HID4[SBE].
        if self.model == PpcModel::Broadway && self.spr[SPR_HID4] & 0x0200_0000 != 0 {
            8
        } else {
            4
        }
    }

    /// Set up the BATs the GameCube/Wii IPL leaves behind: 0x80000000
    /// (cached) and 0xC0000000 (uncached) both map physical 0, plus
    /// relocation on.
    pub fn setup_os_bats(&mut self) {
        let mem_size_upper = 0x1FFE; // 256 MiB, Vs | Vp
        self.write_spr(528, 0x8000_0000 | mem_size_upper);
        self.write_spr(529, 0x0000_0002);
        self.write_spr(536, 0x8000_0000 | mem_size_upper);
        self.write_spr(537, 0x0000_0002);
        self.write_spr(538, 0xC000_0000 | mem_size_upper);
        self.write_spr(539, 0x0000_002A);
        if self.model == PpcModel::Broadway {
            // MEM2 at 0x90000000 / 0xD0000000
            self.write_spr(530, 0x9000_0000 | mem_size_upper);
            self.write_spr(531, 0x1000_0002);
            self.write_spr(540, 0x9000_0000 | mem_size_upper);
            self.write_spr(541, 0x1000_0002);
            self.write_spr(542, 0xD000_0000 | mem_size_upper);
            self.write_spr(543, 0x1000_002A);
        }
        self.msr |= MSR_IR | MSR_DR | MSR_FP | MSR_RI;
        self.msr &= !MSR_IP;
    }

    // -------------------------------------------------------- exceptions

    pub fn exception(&mut self, vector: u32, srr0: u32, srr1_flags: u32) {
        self.spr[SPR_SRR0] = srr0;
        self.spr[SPR_SRR1] = (self.msr & 0x87C0_FFFF) | srr1_flags;
        let le = if self.msr & MSR_ILE != 0 { MSR_LE } else { 0 };
        let old_pr = self.msr & MSR_PR;
        self.msr = (self.msr & (MSR_ME | MSR_IP | MSR_ILE)) | le;
        if old_pr != 0 {
            self.itlb.flush();
            self.dtlb.flush();
        }
        self.pc = if self.msr & MSR_IP != 0 { 0xFFF0_0000 } else { 0 } | vector;
        self.exception_taken = true;
        self.reservation = None;
    }

    // ---------------------------------------------------------- the MMU

    /// Effective → physical. Raises DSI/ISI on failure.
    pub fn translate<B: PpcBus>(&mut self, bus: &mut B, ea: u32, instr: bool, write: bool) -> Option<u32> {
        let relocate = self.msr & if instr { MSR_IR } else { MSR_DR } != 0;
        if !relocate {
            return Some(ea);
        }
        let tlb = if instr { &mut self.itlb } else { &mut self.dtlb };
        if let Some(pa) = tlb.lookup(ea) {
            return Some(pa);
        }
        let supervisor = self.msr & MSR_PR == 0;
        let bats = if instr { &self.ibat } else { &self.dbat };
        let mut pa = bats[..self.bat_count()].iter().find_map(|b| b.translate(ea, supervisor));
        if pa.is_none() {
            let sr = self.sr[(ea >> 28) as usize];
            if let Some(hit) = page_table_walk(ea, sr, self.spr[SPR_SDR1], |a| bus.read32(a)) {
                // Referenced (and Changed on stores) bits.
                let rc = 0x100 | if write { 0x80 } else { 0 };
                if hit.word1 & rc != rc {
                    bus.write32(hit.pte_addr + 4, hit.word1 | rc);
                }
                pa = Some(hit.pa);
            }
        }
        match pa {
            Some(p) => {
                let tlb = if instr { &mut self.itlb } else { &mut self.dtlb };
                tlb.insert(ea, p);
                Some(p)
            }
            None => {
                if instr {
                    self.exception(EXC_ISI, ea, 0x4000_0000);
                } else {
                    self.spr[SPR_DAR] = ea;
                    self.spr[SPR_DSISR] = 0x4000_0000 | if write { 0x0200_0000 } else { 0 };
                    self.exception(EXC_DSI, self.pc, 0);
                }
                None
            }
        }
    }

    fn read<B: PpcBus>(&mut self, bus: &mut B, ea: u32, size: u32) -> Option<u64> {
        if ea % size != 0 || (ea & 0xFFF) + size > 0x1000 {
            let mut v = 0u64;
            for i in 0..size {
                let pa = self.translate(bus, ea.wrapping_add(i), false, false)?;
                v = (v << 8) | bus.read8(pa) as u64;
            }
            return Some(v);
        }
        let pa = self.translate(bus, ea, false, false)?;
        Some(match size {
            1 => bus.read8(pa) as u64,
            2 => bus.read16(pa) as u64,
            4 => bus.read32(pa) as u64,
            _ => bus.read64(pa),
        })
    }

    fn write<B: PpcBus>(&mut self, bus: &mut B, ea: u32, size: u32, v: u64) -> Option<()> {
        if ea % size != 0 || (ea & 0xFFF) + size > 0x1000 {
            for i in 0..size {
                let pa = self.translate(bus, ea.wrapping_add(i), false, true)?;
                bus.write8(pa, (v >> ((size - 1 - i) * 8)) as u8);
            }
            return Some(());
        }
        let pa = self.translate(bus, ea, false, true)?;
        match size {
            1 => bus.write8(pa, v as u8),
            2 => bus.write16(pa, v as u16),
            4 => bus.write32(pa, v as u32),
            _ => bus.write64(pa, v),
        }
        if self.reservation == Some(ea & !3) {
            self.reservation = None;
        }
        Some(())
    }

    // -------------------------------------------------------- CR helpers

    pub fn cr_field(&self, n: usize) -> u32 {
        (self.cr >> (28 - n * 4)) & 0xF
    }

    pub fn set_cr_field(&mut self, n: usize, v: u32) {
        let sh = 28 - n * 4;
        self.cr = (self.cr & !(0xF << sh)) | ((v & 0xF) << sh);
    }

    #[inline]
    fn cr_bit(&self, b: u32) -> bool {
        (self.cr >> (31 - b)) & 1 != 0
    }

    #[inline]
    fn set_cr_bit(&mut self, b: u32, v: bool) {
        let m = 1 << (31 - b);
        if v {
            self.cr |= m;
        } else {
            self.cr &= !m;
        }
    }

    fn update_cr0(&mut self, r: u32) {
        let v = if (r as i32) < 0 {
            8
        } else if r != 0 {
            4
        } else {
            2
        };
        self.set_cr_field(0, v | (self.xer() >> 31));
    }

    fn compare(&mut self, crf: usize, lt: bool, gt: bool) {
        let v = if lt {
            8
        } else if gt {
            4
        } else {
            2
        };
        self.set_cr_field(crf, v | (self.xer() >> 31));
    }

    fn fp_cr1(&mut self) {
        self.set_cr_field(1, self.fpscr >> 28);
    }

    fn fcompare(&mut self, crf: usize, a: f64, b: f64) {
        let c = if a.is_nan() || b.is_nan() {
            1
        } else if a < b {
            8
        } else if a > b {
            4
        } else {
            2
        };
        self.fpscr = (self.fpscr & !0xF000) | (c << 12);
        self.set_cr_field(crf, c);
    }

    // ---------------------------------------------------------- timebase

    /// Advance the timebase/decrementer by `ticks` timebase ticks.
    pub fn advance_timebase(&mut self, ticks: u32) {
        self.tb = self.tb.wrapping_add(ticks as u64);
        let old = self.spr[SPR_DEC];
        let new = old.wrapping_sub(ticks);
        self.spr[SPR_DEC] = new;
        if old & 0x8000_0000 == 0 && (new & 0x8000_0000 != 0 || ticks > old) {
            self.dec_pending = true;
        }
    }

    // --------------------------------------------------------- execution

    /// Take pending interrupts, then execute one instruction.
    pub fn step<B: PpcBus>(&mut self, bus: &mut B) {
        self.cycles += 1;
        self.tb_accum += 1;
        if self.tb_accum >= TB_RATIO {
            self.tb_accum = 0;
            self.advance_timebase(1);
        }
        if self.msr & MSR_EE != 0 {
            if self.external_interrupt {
                self.exception(EXC_EXTERNAL, self.pc, 0);
                return;
            }
            if self.dec_pending {
                self.dec_pending = false;
                self.exception(EXC_DECREMENTER, self.pc, 0);
                return;
            }
        }
        self.exception_taken = false;
        let Some(pa) = self.translate(bus, self.pc, true, false) else { return };
        let insn = bus.read32(pa);
        self.execute(bus, insn);
    }

    /// Execute one already-fetched instruction at `self.pc`.
    pub fn execute<B: PpcBus>(&mut self, bus: &mut B, insn: u32) {
        self.exception_taken = false;
        self.npc = self.pc.wrapping_add(4);
        self.retired += 1;
        match insn >> 26 {
            3 => self.trap(insn, self.gpr[ra(insn)], simm(insn)),
            4 => self.op4(bus, insn),
            7 => {
                let v = (self.gpr[ra(insn)] as i32).wrapping_mul(insn as i16 as i32);
                self.gpr[rd(insn)] = v as u32;
            }
            8 => {
                let a = self.gpr[ra(insn)];
                let imm = simm(insn);
                let (r, c) = add3(!a, imm, 1);
                self.gpr[rd(insn)] = r;
                self.set_ca(c);
            }
            10 => {
                let a = self.gpr[ra(insn)];
                let b = insn & 0xFFFF;
                self.compare(((insn >> 23) & 7) as usize, a < b, a > b);
            }
            11 => {
                let a = self.gpr[ra(insn)] as i32;
                let b = insn as i16 as i32;
                self.compare(((insn >> 23) & 7) as usize, a < b, a > b);
            }
            12 | 13 => {
                let (r, c) = add3(self.gpr[ra(insn)], simm(insn), 0);
                self.gpr[rd(insn)] = r;
                self.set_ca(c);
                if insn >> 26 == 13 {
                    self.update_cr0(r);
                }
            }
            14 => {
                let a = if ra(insn) == 0 { 0 } else { self.gpr[ra(insn)] };
                self.gpr[rd(insn)] = a.wrapping_add(simm(insn));
            }
            15 => {
                let a = if ra(insn) == 0 { 0 } else { self.gpr[ra(insn)] };
                self.gpr[rd(insn)] = a.wrapping_add(insn << 16);
            }
            16 => {
                let target = if insn & 2 != 0 { simm(insn & 0xFFFC) } else { self.pc.wrapping_add(simm(insn & 0xFFFC)) };
                self.branch_cond(insn, target, true);
            }
            17 => self.exception(EXC_SYSCALL, self.pc.wrapping_add(4), 0),
            18 => {
                let li = (((insn & 0x03FF_FFFC) << 6) as i32 >> 6) as u32;
                let target = if insn & 2 != 0 { li } else { self.pc.wrapping_add(li) };
                if insn & 1 != 0 {
                    self.set_lr(self.pc.wrapping_add(4));
                }
                self.npc = target;
            }
            19 => self.op19(insn),
            20 => {
                let sh = rb(insn) as u32;
                let m = rot_mask((insn >> 6) & 31, (insn >> 1) & 31);
                let r = (self.gpr[rd(insn)].rotate_left(sh) & m) | (self.gpr[ra(insn)] & !m);
                self.gpr[ra(insn)] = r;
                if insn & 1 != 0 {
                    self.update_cr0(r);
                }
            }
            21 => {
                let sh = rb(insn) as u32;
                let r = self.gpr[rd(insn)].rotate_left(sh) & rot_mask((insn >> 6) & 31, (insn >> 1) & 31);
                self.gpr[ra(insn)] = r;
                if insn & 1 != 0 {
                    self.update_cr0(r);
                }
            }
            23 => {
                let sh = self.gpr[rb(insn)] & 31;
                let r = self.gpr[rd(insn)].rotate_left(sh) & rot_mask((insn >> 6) & 31, (insn >> 1) & 31);
                self.gpr[ra(insn)] = r;
                if insn & 1 != 0 {
                    self.update_cr0(r);
                }
            }
            24 => self.gpr[ra(insn)] = self.gpr[rd(insn)] | (insn & 0xFFFF),
            25 => self.gpr[ra(insn)] = self.gpr[rd(insn)] | (insn << 16),
            26 => self.gpr[ra(insn)] = self.gpr[rd(insn)] ^ (insn & 0xFFFF),
            27 => self.gpr[ra(insn)] = self.gpr[rd(insn)] ^ (insn << 16),
            28 => {
                let r = self.gpr[rd(insn)] & (insn & 0xFFFF);
                self.gpr[ra(insn)] = r;
                self.update_cr0(r);
            }
            29 => {
                let r = self.gpr[rd(insn)] & (insn << 16);
                self.gpr[ra(insn)] = r;
                self.update_cr0(r);
            }
            31 => self.op31(bus, insn),
            32..=55 => self.load_store_d(bus, insn),
            56 | 57 => self.psq_load(bus, insn, false),
            59 => self.op59(insn),
            60 | 61 => self.psq_store(bus, insn, false),
            63 => self.op63(insn),
            _ => self.illegal(insn),
        }
        if !self.exception_taken {
            self.pc = self.npc;
        }
    }

    fn illegal(&mut self, insn: u32) {
        tracing::warn!("illegal PowerPC instruction {insn:#010x} at {:#010x}", self.pc);
        self.exception(EXC_PROGRAM, self.pc, 0x0008_0000);
    }

    fn trap(&mut self, insn: u32, a: u32, b: u32) {
        let to = rd(insn) as u32;
        let (sa, sb) = (a as i32, b as i32);
        let hit = (to & 16 != 0 && sa < sb)
            || (to & 8 != 0 && sa > sb)
            || (to & 4 != 0 && a == b)
            || (to & 2 != 0 && a < b)
            || (to & 1 != 0 && a > b);
        if hit {
            self.exception(EXC_PROGRAM, self.pc, 0x0002_0000);
        }
    }

    fn branch_cond(&mut self, insn: u32, target: u32, allow_ctr: bool) {
        let bo = rd(insn) as u32;
        let bi = ra(insn) as u32;
        if allow_ctr && bo & 4 == 0 {
            self.spr[SPR_CTR] = self.ctr().wrapping_sub(1);
        }
        let ctr_ok = bo & 4 != 0 || ((self.ctr() != 0) ^ (bo & 2 != 0));
        let cond_ok = bo & 16 != 0 || (self.cr_bit(bi) == (bo & 8 != 0));
        if insn & 1 != 0 {
            self.set_lr(self.pc.wrapping_add(4));
        }
        if ctr_ok && cond_ok {
            self.npc = target;
        }
    }

    fn op19(&mut self, insn: u32) {
        let xo = (insn >> 1) & 0x3FF;
        let (bd, ba, bb) = (rd(insn) as u32, ra(insn) as u32, rb(insn) as u32);
        let cr_op = |cpu: &mut Ppc, f: fn(bool, bool) -> bool| {
            let v = f(cpu.cr_bit(ba), cpu.cr_bit(bb));
            cpu.set_cr_bit(bd, v);
        };
        match xo {
            0 => {
                let v = self.cr_field(((insn >> 18) & 7) as usize);
                self.set_cr_field(((insn >> 23) & 7) as usize, v);
            }
            16 => {
                let target = self.lr() & !3;
                self.branch_cond(insn, target, true);
            }
            528 => {
                let target = self.ctr() & !3;
                self.branch_cond(insn, target, false);
            }
            33 => cr_op(self, |a, b| !(a | b)),
            129 => cr_op(self, |a, b| a & !b),
            193 => cr_op(self, |a, b| a ^ b),
            225 => cr_op(self, |a, b| !(a & b)),
            257 => cr_op(self, |a, b| a & b),
            289 => cr_op(self, |a, b| a == b),
            417 => cr_op(self, |a, b| a | !b),
            449 => cr_op(self, |a, b| a | b),
            50 => {
                // rfi
                let srr1 = self.spr[SPR_SRR1];
                let mask = 0x87C0_FF73;
                let old_pr = self.msr & MSR_PR;
                self.msr = (self.msr & !mask) | (srr1 & mask);
                if old_pr != self.msr & MSR_PR {
                    self.itlb.flush();
                    self.dtlb.flush();
                }
                self.npc = self.spr[SPR_SRR0] & !3;
            }
            150 => {} // isync
            _ => self.illegal(insn),
        }
    }

    fn arith(&mut self, insn: u32, xo9: u32) {
        let a = self.gpr[ra(insn)];
        let b = self.gpr[rb(insn)];
        let oe = insn & 0x400 != 0;
        let ca = self.ca();
        let (r, carry, ov): (u32, Option<bool>, bool) = match xo9 {
            266 => {
                let r = a.wrapping_add(b);
                (r, None, add_ov(a, b, r))
            }
            10 => {
                let (r, c) = add3(a, b, 0);
                (r, Some(c), add_ov(a, b, r))
            }
            138 => {
                let (r, c) = add3(a, b, ca);
                (r, Some(c), add_ov(a, b, r))
            }
            234 => {
                let (r, c) = add3(a, u32::MAX, ca);
                (r, Some(c), add_ov(a, u32::MAX, r))
            }
            202 => {
                let (r, c) = add3(a, 0, ca);
                (r, Some(c), add_ov(a, 0, r))
            }
            40 => {
                let r = b.wrapping_sub(a);
                (r, None, add_ov(!a, b, r))
            }
            8 => {
                let (r, c) = add3(!a, b, 1);
                (r, Some(c), add_ov(!a, b, r))
            }
            136 => {
                let (r, c) = add3(!a, b, ca);
                (r, Some(c), add_ov(!a, b, r))
            }
            232 => {
                let (r, c) = add3(!a, u32::MAX, ca);
                (r, Some(c), add_ov(!a, u32::MAX, r))
            }
            200 => {
                let (r, c) = add3(!a, 0, ca);
                (r, Some(c), add_ov(!a, 0, r))
            }
            104 => (a.wrapping_neg(), None, a == 0x8000_0000),
            235 => {
                let p = (a as i32 as i64) * (b as i32 as i64);
                (p as u32, None, p != p as i32 as i64)
            }
            75 => (((a as i32 as i64 * b as i32 as i64) >> 32) as u32, None, false),
            11 => (((a as u64 * b as u64) >> 32) as u32, None, false),
            491 => {
                let (sa, sb) = (a as i32, b as i32);
                if sb == 0 || (sa == i32::MIN && sb == -1) {
                    (if sa < 0 && sb == 0 { u32::MAX } else { 0 }, None, true)
                } else {
                    ((sa / sb) as u32, None, false)
                }
            }
            459 => match a.checked_div(b) {
                Some(q) => (q, None, false),
                None => (0, None, true),
            },
            _ => unreachable!(),
        };
        self.gpr[rd(insn)] = r;
        if let Some(c) = carry {
            self.set_ca(c);
        }
        if oe {
            self.set_ov(ov);
        }
        if insn & 1 != 0 {
            self.update_cr0(r);
        }
    }

    fn op31<B: PpcBus>(&mut self, bus: &mut B, insn: u32) {
        let xo = (insn >> 1) & 0x3FF;
        let xo9 = xo & 0x1FF;
        if matches!(xo9, 8 | 10 | 11 | 40 | 75 | 104 | 136 | 138 | 200 | 202 | 232 | 234 | 235 | 266 | 459 | 491) {
            return self.arith(insn, xo9);
        }
        let s = self.gpr[rd(insn)];
        let b = self.gpr[rb(insn)];
        let base = if ra(insn) == 0 { 0 } else { self.gpr[ra(insn)] };
        let ea = base.wrapping_add(b);
        let logic = |cpu: &mut Ppc, r: u32| {
            cpu.gpr[ra(insn)] = r;
            if insn & 1 != 0 {
                cpu.update_cr0(r);
            }
        };
        match xo {
            0 => {
                let (x, y) = (self.gpr[ra(insn)] as i32, b as i32);
                self.compare(((insn >> 23) & 7) as usize, x < y, x > y);
            }
            32 => {
                let x = self.gpr[ra(insn)];
                self.compare(((insn >> 23) & 7) as usize, x < b, x > b);
            }
            4 => self.trap(insn, self.gpr[ra(insn)], b),
            28 => logic(self, s & b),
            60 => logic(self, s & !b),
            124 => logic(self, !(s | b)),
            284 => logic(self, !(s ^ b)),
            316 => logic(self, s ^ b),
            412 => logic(self, s | !b),
            444 => logic(self, s | b),
            476 => logic(self, !(s & b)),
            26 => logic(self, s.leading_zeros()),
            922 => logic(self, s as i16 as i32 as u32),
            954 => logic(self, s as i8 as i32 as u32),
            24 => {
                let n = b & 0x3F;
                logic(self, if n > 31 { 0 } else { s << n });
            }
            536 => {
                let n = b & 0x3F;
                logic(self, if n > 31 { 0 } else { s >> n });
            }
            792 => {
                let n = b & 0x3F;
                let neg = (s as i32) < 0;
                let (r, ca) = if n > 31 {
                    (if neg { u32::MAX } else { 0 }, neg)
                } else {
                    (((s as i32) >> n) as u32, neg && n > 0 && s & ((1u32 << n) - 1) != 0)
                };
                self.set_ca(ca);
                logic(self, r);
            }
            824 => {
                let n = rb(insn) as u32;
                let neg = (s as i32) < 0;
                let ca = neg && n > 0 && s & ((1u32 << n) - 1) != 0;
                self.set_ca(ca);
                logic(self, ((s as i32) >> n) as u32);
            }
            // loads
            23 | 55 | 87 | 119 | 279 | 311 | 343 | 375 | 534 | 790 => {
                let (size, sign, update, rev) = match xo {
                    23 => (4, false, false, false),
                    55 => (4, false, true, false),
                    87 => (1, false, false, false),
                    119 => (1, false, true, false),
                    279 => (2, false, false, false),
                    311 => (2, false, true, false),
                    343 => (2, true, false, false),
                    375 => (2, true, true, false),
                    534 => (4, false, false, true),
                    _ => (2, false, false, true),
                };
                let ea = if update { self.gpr[ra(insn)].wrapping_add(b) } else { ea };
                let Some(v) = self.read(bus, ea, size) else { return };
                let mut v = v as u32;
                if rev {
                    v = if size == 4 { v.swap_bytes() } else { (v as u16).swap_bytes() as u32 };
                }
                if sign {
                    v = v as i16 as i32 as u32;
                }
                self.gpr[rd(insn)] = v;
                if update {
                    self.gpr[ra(insn)] = ea;
                }
            }
            20 => {
                let Some(v) = self.read(bus, ea, 4) else { return };
                self.gpr[rd(insn)] = v as u32;
                self.reservation = Some(ea & !3);
            }
            150 => {
                let ok = self.reservation == Some(ea & !3);
                if ok && self.write(bus, ea, 4, s as u64).is_none() {
                    return;
                }
                self.reservation = None;
                self.set_cr_field(0, if ok { 2 } else { 0 } | (self.xer() >> 31));
            }
            // stores
            151 | 183 | 215 | 247 | 407 | 439 | 662 | 918 => {
                let (size, update, rev) = match xo {
                    151 => (4, false, false),
                    183 => (4, true, false),
                    215 => (1, false, false),
                    247 => (1, true, false),
                    407 => (2, false, false),
                    439 => (2, true, false),
                    662 => (4, false, true),
                    _ => (2, false, true),
                };
                let ea = if update { self.gpr[ra(insn)].wrapping_add(b) } else { ea };
                let v = if rev {
                    if size == 4 {
                        s.swap_bytes()
                    } else {
                        (s as u16).swap_bytes() as u32
                    }
                } else {
                    s
                };
                if self.write(bus, ea, size, v as u64).is_none() {
                    return;
                }
                if update {
                    self.gpr[ra(insn)] = ea;
                }
            }
            // FP indexed loads/stores
            535 | 567 | 599 | 631 | 663 | 695 | 727 | 759 | 983 => {
                let update = matches!(xo, 567 | 631 | 695 | 759);
                let ea = if update { self.gpr[ra(insn)].wrapping_add(b) } else { ea };
                let f = rd(insn);
                match xo {
                    535 | 567 => {
                        let Some(v) = self.read(bus, ea, 4) else { return };
                        let x = f32::from_bits(v as u32) as f64;
                        self.fpr[f] = [x, x];
                    }
                    599 | 631 => {
                        let Some(v) = self.read(bus, ea, 8) else { return };
                        self.fpr[f][0] = f64::from_bits(v);
                    }
                    663 | 695 => {
                        let v = (self.fpr[f][0] as f32).to_bits();
                        if self.write(bus, ea, 4, v as u64).is_none() {
                            return;
                        }
                    }
                    727 | 759 => {
                        let v = self.fpr[f][0].to_bits();
                        if self.write(bus, ea, 8, v).is_none() {
                            return;
                        }
                    }
                    _ => {
                        let v = self.fpr[f][0].to_bits() as u32;
                        if self.write(bus, ea, 4, v as u64).is_none() {
                            return;
                        }
                    }
                }
                if update {
                    self.gpr[ra(insn)] = ea;
                }
            }
            597 | 725 | 533 | 661 => self.string_op(bus, insn, xo),
            19 => self.gpr[rd(insn)] = self.cr,
            144 => {
                let crm = (insn >> 12) & 0xFF;
                let mut mask = 0;
                for i in 0..8 {
                    if crm & (0x80 >> i) != 0 {
                        mask |= 0xF000_0000 >> (i * 4);
                    }
                }
                self.cr = (self.cr & !mask) | (s & mask);
            }
            512 => {
                let x = self.xer();
                self.set_cr_field(((insn >> 23) & 7) as usize, x >> 28);
                self.spr[SPR_XER] &= 0x0FFF_FFFF;
            }
            83 => self.gpr[rd(insn)] = self.msr,
            146 => {
                let old = self.msr;
                self.msr = s;
                if (old ^ s) & MSR_PR != 0 {
                    self.itlb.flush();
                    self.dtlb.flush();
                }
            }
            339 | 371 => {
                let n = (((insn >> 16) & 0x1F) | ((insn >> 6) & 0x3E0)) as usize;
                self.gpr[rd(insn)] = self.read_spr(n);
            }
            467 => {
                let n = (((insn >> 16) & 0x1F) | ((insn >> 6) & 0x3E0)) as usize;
                self.write_spr(n, s);
            }
            210 => {
                self.sr[((insn >> 16) & 15) as usize] = s;
                self.itlb.flush();
                self.dtlb.flush();
            }
            242 => {
                self.sr[(b >> 28) as usize] = s;
                self.itlb.flush();
                self.dtlb.flush();
            }
            595 => self.gpr[rd(insn)] = self.sr[((insn >> 16) & 15) as usize],
            659 => self.gpr[rd(insn)] = self.sr[(b >> 28) as usize],
            306 => {
                self.itlb.invalidate_page(b);
                self.dtlb.invalidate_page(b);
            }
            370 => {
                self.itlb.flush();
                self.dtlb.flush();
            }
            1014 => {
                let Some(pa) = self.translate(bus, ea & !31, false, true) else { return };
                bus.zero_block(pa);
            }
            982 => {
                if let Some(pa) = self.translate(bus, ea, false, false) {
                    bus.invalidate_icache(pa & !31);
                }
            }
            54 | 86 | 246 | 278 | 470 | 566 | 598 | 854 => {} // cache hints, sync, eieio
            310 | 438 => {}                                // eciwx/ecowx
            _ => self.illegal(insn),
        }
    }

    fn string_op<B: PpcBus>(&mut self, bus: &mut B, insn: u32, xo: u32) {
        let base = if ra(insn) == 0 { 0 } else { self.gpr[ra(insn)] };
        let (ea, n) = match xo {
            597 | 725 => (base, if rb(insn) == 0 { 32 } else { rb(insn) as u32 }),
            _ => (base.wrapping_add(self.gpr[rb(insn)]), self.xer() & 0x7F),
        };
        let load = matches!(xo, 597 | 533);
        let mut r = rd(insn);
        let mut shift = 24i32;
        if load && n > 0 {
            self.gpr[r] = 0;
        }
        for i in 0..n {
            if load {
                let Some(v) = self.read(bus, ea.wrapping_add(i), 1) else { return };
                self.gpr[r] |= (v as u32) << shift;
            } else {
                let v = (self.gpr[r] >> shift) as u8;
                if self.write(bus, ea.wrapping_add(i), 1, v as u64).is_none() {
                    return;
                }
            }
            shift -= 8;
            if shift < 0 && i + 1 < n {
                shift = 24;
                r = (r + 1) % 32;
                if load {
                    self.gpr[r] = 0;
                }
            }
        }
    }

    fn load_store_d<B: PpcBus>(&mut self, bus: &mut B, insn: u32) {
        let op = insn >> 26;
        let update = op & 1 == 1 && op != 46 && op != 47;
        let base = if ra(insn) == 0 && !update { 0 } else { self.gpr[ra(insn)] };
        let ea = base.wrapping_add(simm(insn));
        let d = rd(insn);
        let ok = match op {
            32..=35 | 40..=43 => {
                let size = if op <= 33 {
                    4
                } else if op <= 35 {
                    1
                } else {
                    2
                };
                self.read(bus, ea, size).map(|v| {
                    self.gpr[d] = if op >= 42 { v as u16 as i16 as i32 as u32 } else { v as u32 };
                })
            }
            36..=39 | 44 | 45 => {
                let size = match op {
                    36 | 37 => 4,
                    38 | 39 => 1,
                    _ => 2,
                };
                self.write(bus, ea, size, self.gpr[d] as u64)
            }
            46 => {
                let mut a = ea;
                for r in d..32 {
                    let Some(v) = self.read(bus, a, 4) else { return };
                    self.gpr[r] = v as u32;
                    a = a.wrapping_add(4);
                }
                Some(())
            }
            47 => {
                let mut a = ea;
                for r in d..32 {
                    if self.write(bus, a, 4, self.gpr[r] as u64).is_none() {
                        return;
                    }
                    a = a.wrapping_add(4);
                }
                Some(())
            }
            48 | 49 => self.read(bus, ea, 4).map(|v| {
                let x = f32::from_bits(v as u32) as f64;
                self.fpr[d] = [x, x];
            }),
            50 | 51 => self.read(bus, ea, 8).map(|v| self.fpr[d][0] = f64::from_bits(v)),
            52 | 53 => {
                let v = (self.fpr[d][0] as f32).to_bits();
                self.write(bus, ea, 4, v as u64)
            }
            _ => {
                let v = self.fpr[d][0].to_bits();
                self.write(bus, ea, 8, v)
            }
        };
        if ok.is_some() && update {
            self.gpr[ra(insn)] = ea;
        }
    }

    // --------------------------------------------------------------- FPU

    fn op59(&mut self, insn: u32) {
        let (d, a, b, c) = (rd(insn), ra(insn), rb(insn), rc_reg(insn));
        let (fa, fb, fc) = (self.fpr[a][0], self.fpr[b][0], self.fpr[c][0]);
        let r = match (insn >> 1) & 0x1F {
            18 => fa / fb,
            20 => fa - fb,
            21 => fa + fb,
            24 => 1.0 / fb,
            25 => fa * fc,
            28 => fa.mul_add(fc, -fb),
            29 => fa.mul_add(fc, fb),
            30 => -(fa.mul_add(fc, -fb)),
            31 => -(fa.mul_add(fc, fb)),
            _ => return self.illegal(insn),
        };
        let r = round_single(r);
        self.fpr[d] = [r, r];
        if insn & 1 != 0 {
            self.fp_cr1();
        }
    }

    fn op63(&mut self, insn: u32) {
        let (d, a, b, c) = (rd(insn), ra(insn), rb(insn), rc_reg(insn));
        let (fa, fb, fc) = (self.fpr[a][0], self.fpr[b][0], self.fpr[c][0]);
        let xo5 = (insn >> 1) & 0x1F;
        if xo5 >= 18 && !matches!(xo5, 19 | 22 | 24 | 27) {
            let r = match xo5 {
                18 => fa / fb,
                20 => fa - fb,
                21 => fa + fb,
                23 => {
                    if fa >= 0.0 {
                        fc
                    } else {
                        fb
                    }
                }
                25 => fa * fc,
                26 => 1.0 / fb.sqrt(),
                28 => fa.mul_add(fc, -fb),
                29 => fa.mul_add(fc, fb),
                30 => -(fa.mul_add(fc, -fb)),
                _ => -(fa.mul_add(fc, fb)),
            };
            self.fpr[d][0] = r;
            if insn & 1 != 0 {
                self.fp_cr1();
            }
            return;
        }
        match (insn >> 1) & 0x3FF {
            0 | 32 => self.fcompare(((insn >> 23) & 7) as usize, fa, fb),
            12 => {
                let r = round_single(fb);
                self.fpr[d] = [r, r];
            }
            14 | 15 => {
                let v = if fb.is_nan() {
                    i32::MIN
                } else if (insn >> 1) & 0x3FF == 15 {
                    fb.trunc().clamp(i32::MIN as f64, i32::MAX as f64) as i32
                } else {
                    fb.round_ties_even().clamp(i32::MIN as f64, i32::MAX as f64) as i32
                };
                self.fpr[d][0] = f64::from_bits(0xFFF8_0000_0000_0000 | v as u32 as u64);
            }
            40 => self.fpr[d][0] = -fb,
            72 => self.fpr[d][0] = fb,
            136 => self.fpr[d][0] = -fb.abs(),
            264 => self.fpr[d][0] = fb.abs(),
            38 => self.fpscr |= 0x8000_0000 >> rd(insn),
            70 => self.fpscr &= !(0x8000_0000 >> rd(insn)),
            64 => {
                let v = (self.fpscr >> (28 - ((insn >> 18) & 7) * 4)) & 0xF;
                self.set_cr_field(((insn >> 23) & 7) as usize, v);
            }
            134 => {
                let field = (insn >> 23) & 7;
                let imm = (insn >> 12) & 0xF;
                let sh = 28 - field * 4;
                self.fpscr = (self.fpscr & !(0xF << sh)) | (imm << sh);
            }
            583 => self.fpr[d][0] = f64::from_bits(0xFFF8_0000_0000_0000 | self.fpscr as u64),
            711 => {
                let fm = (insn >> 17) & 0xFF;
                let v = fb.to_bits() as u32;
                for i in 0..8 {
                    if fm & (0x80 >> i) != 0 {
                        let m = 0xF000_0000u32 >> (i * 4);
                        self.fpscr = (self.fpscr & !m) | (v & m);
                    }
                }
            }
            _ => return self.illegal(insn),
        }
        if insn & 1 != 0 {
            self.fp_cr1();
        }
    }

    // ------------------------------------------------- paired singles

    fn op4<B: PpcBus>(&mut self, bus: &mut B, insn: u32) {
        let xo6 = (insn >> 1) & 0x3F;
        if matches!(xo6, 6 | 7 | 38 | 39) {
            return if xo6 & 1 == 0 { self.psq_load(bus, insn, true) } else { self.psq_store(bus, insn, true) };
        }
        let (d, a, b, c) = (rd(insn), ra(insn), rb(insn), rc_reg(insn));
        let (pa, pb, pc) = (self.fpr[a], self.fpr[b], self.fpr[c]);
        let rs = round_single;
        let xo5 = (insn >> 1) & 0x1F;
        let result: [f64; 2] = match xo5 {
            10 => [rs(pa[0] + pb[1]), pc[1]],
            11 => [pc[0], rs(pa[0] + pb[1])],
            12 => [rs(pa[0] * pc[0]), rs(pa[1] * pc[0])],
            13 => [rs(pa[0] * pc[1]), rs(pa[1] * pc[1])],
            14 => [rs(pa[0].mul_add(pc[0], pb[0])), rs(pa[1].mul_add(pc[0], pb[1]))],
            15 => [rs(pa[0].mul_add(pc[1], pb[0])), rs(pa[1].mul_add(pc[1], pb[1]))],
            18 => ps_arith(PsOp::Div, pa, pb),
            20 => ps_arith(PsOp::Sub, pa, pb),
            21 => ps_arith(PsOp::Add, pa, pb),
            23 => [if pa[0] >= 0.0 { pc[0] } else { pb[0] }, if pa[1] >= 0.0 { pc[1] } else { pb[1] }],
            24 => [rs(1.0 / pb[0]), rs(1.0 / pb[1])],
            25 => ps_arith(PsOp::Mul, pa, pc),
            26 => [rs(1.0 / pb[0].sqrt()), rs(1.0 / pb[1].sqrt())],
            28 => [rs(pa[0].mul_add(pc[0], -pb[0])), rs(pa[1].mul_add(pc[1], -pb[1]))],
            29 => [rs(pa[0].mul_add(pc[0], pb[0])), rs(pa[1].mul_add(pc[1], pb[1]))],
            30 => [rs(-pa[0].mul_add(pc[0], -pb[0])), rs(-pa[1].mul_add(pc[1], -pb[1]))],
            31 => [rs(-pa[0].mul_add(pc[0], pb[0])), rs(-pa[1].mul_add(pc[1], pb[1]))],
            _ => {
                match (insn >> 1) & 0x3FF {
                    0 | 32 => self.fcompare(((insn >> 23) & 7) as usize, pa[0], pb[0]),
                    64 | 96 => self.fcompare(((insn >> 23) & 7) as usize, pa[1], pb[1]),
                    40 => self.fpr[d] = [-pb[0], -pb[1]],
                    72 => self.fpr[d] = pb,
                    136 => self.fpr[d] = [-pb[0].abs(), -pb[1].abs()],
                    264 => self.fpr[d] = [pb[0].abs(), pb[1].abs()],
                    528 => self.fpr[d] = [pa[0], rs(pb[0])],
                    560 => self.fpr[d] = [pa[0], pb[1]],
                    592 => self.fpr[d] = [pa[1], rs(pb[0])],
                    624 => self.fpr[d] = [pa[1], pb[1]],
                    1014 => {
                        let base = if a == 0 { 0 } else { self.gpr[a] };
                        let ea = base.wrapping_add(self.gpr[b]) & !31;
                        let Some(p) = self.translate(bus, ea, false, true) else { return };
                        bus.zero_block(p);
                    }
                    _ => return self.illegal(insn),
                }
                if insn & 1 != 0 {
                    self.fp_cr1();
                }
                return;
            }
        };
        self.fpr[d] = result;
        if insn & 1 != 0 {
            self.fp_cr1();
        }
    }

    fn psq_ea(&self, insn: u32, indexed: bool) -> (u32, bool, usize, bool) {
        let a = ra(insn);
        let update = if indexed { (insn >> 1) & 0x3F >= 38 } else { insn >> 26 == 57 || insn >> 26 == 61 };
        let base = if a == 0 && !update { 0 } else { self.gpr[a] };
        if indexed {
            let w = (insn >> 10) & 1 != 0;
            let i = ((insn >> 7) & 7) as usize;
            (base.wrapping_add(self.gpr[rb(insn)]), w, i, update)
        } else {
            let w = (insn >> 15) & 1 != 0;
            let i = ((insn >> 12) & 7) as usize;
            let off = ((insn << 20) as i32 >> 20) as u32;
            (base.wrapping_add(off), w, i, update)
        }
    }

    fn psq_load<B: PpcBus>(&mut self, bus: &mut B, insn: u32, indexed: bool) {
        let (ea, w, i, update) = self.psq_ea(insn, indexed);
        let q = Quant::load(self.gqr(i));
        let size = q.ty.size();
        let Some(r0) = self.read(bus, ea, size) else { return };
        let value = if w {
            [dequantize(r0 as u32, q) as f64, 1.0]
        } else {
            let Some(r1) = self.read(bus, ea.wrapping_add(size), size) else { return };
            dequantize_pair([r0 as u32, r1 as u32], q)
        };
        self.fpr[rd(insn)] = value;
        if update {
            self.gpr[ra(insn)] = ea;
        }
    }

    fn psq_store<B: PpcBus>(&mut self, bus: &mut B, insn: u32, indexed: bool) {
        let (ea, w, i, update) = self.psq_ea(insn, indexed);
        let q = Quant::store(self.gqr(i));
        let size = q.ty.size();
        let ps = self.fpr[rd(insn)];
        if self.write(bus, ea, size, quantize(ps[0] as f32, q) as u64).is_none() {
            return;
        }
        if !w && self.write(bus, ea.wrapping_add(size), size, quantize(ps[1] as f32, q) as u64).is_none() {
            return;
        }
        if update {
            self.gpr[ra(insn)] = ea;
        }
    }
}

#[inline]
fn add3(a: u32, b: u32, c: u32) -> (u32, bool) {
    let wide = a as u64 + b as u64 + c as u64;
    (wide as u32, wide > 0xFFFF_FFFF)
}

#[inline]
fn add_ov(a: u32, b: u32, r: u32) -> bool {
    ((a ^ r) & (b ^ r)) >> 31 != 0
}

impl SaveState for Ppc {
    fn save_state(&self, w: &mut StateWriter) {
        w.u32s(&self.gpr);
        for f in &self.fpr {
            w.f64(f[0]);
            w.f64(f[1]);
        }
        w.u32(self.pc);
        w.u32(self.cr);
        w.u32(self.msr);
        w.u32(self.fpscr);
        w.u32s(&self.spr);
        w.u32s(&self.sr);
        w.u64(self.tb);
        w.u32(self.tb_accum);
        w.bool(self.external_interrupt);
        w.bool(self.dec_pending);
        w.u64(self.cycles);
    }

    fn load_state(&mut self, r: &mut StateReader<'_>) -> StateResult<()> {
        r.u32s_into(&mut self.gpr)?;
        for f in &mut self.fpr {
            *f = [r.f64()?, r.f64()?];
        }
        self.pc = r.u32()?;
        self.cr = r.u32()?;
        self.msr = r.u32()?;
        self.fpscr = r.u32()?;
        r.u32s_into(&mut self.spr)?;
        r.u32s_into(&mut self.sr)?;
        self.tb = r.u64()?;
        self.tb_accum = r.u32()?;
        self.external_interrupt = r.bool()?;
        self.dec_pending = r.bool()?;
        self.cycles = r.u64()?;
        self.reservation = None;
        self.rebuild_bats();
        Ok(())
    }
}
