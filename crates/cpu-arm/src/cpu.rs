//! Register file, modes, exceptions and the fetch/execute loop.

use crate::{arm, thumb, Access, Bus, CpuModel};
use kd_core::{SaveState, StateReader, StateResult, StateWriter};

pub const MODE_USR: u32 = 0x10;
pub const MODE_FIQ: u32 = 0x11;
pub const MODE_IRQ: u32 = 0x12;
pub const MODE_SVC: u32 = 0x13;
pub const MODE_ABT: u32 = 0x17;
pub const MODE_UND: u32 = 0x1B;
pub const MODE_SYS: u32 = 0x1F;

pub const FLAG_N: u32 = 1 << 31;
pub const FLAG_Z: u32 = 1 << 30;
pub const FLAG_C: u32 = 1 << 29;
pub const FLAG_V: u32 = 1 << 28;
pub const FLAG_Q: u32 = 1 << 27;
pub const FLAG_I: u32 = 1 << 7;
pub const FLAG_F: u32 = 1 << 6;
pub const FLAG_T: u32 = 1 << 5;

/// Exception kinds with their vector offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exception {
    Reset,
    Undefined,
    Swi,
    PrefetchAbort,
    DataAbort,
    Irq,
    Fiq,
}

impl Exception {
    fn vector(self) -> u32 {
        match self {
            Exception::Reset => 0x00,
            Exception::Undefined => 0x04,
            Exception::Swi => 0x08,
            Exception::PrefetchAbort => 0x0C,
            Exception::DataAbort => 0x10,
            Exception::Irq => 0x18,
            Exception::Fiq => 0x1C,
        }
    }
    fn mode(self) -> u32 {
        match self {
            Exception::Reset | Exception::Swi => MODE_SVC,
            Exception::Undefined => MODE_UND,
            Exception::PrefetchAbort | Exception::DataAbort => MODE_ABT,
            Exception::Irq => MODE_IRQ,
            Exception::Fiq => MODE_FIQ,
        }
    }
}

/// Register bank index for a mode: usr/sys share bank 0.
#[inline]
pub(crate) fn bank_of(mode: u32) -> usize {
    match mode & 0x1F {
        MODE_FIQ => 1,
        MODE_IRQ => 2,
        MODE_SVC => 3,
        MODE_ABT => 4,
        MODE_UND => 5,
        _ => 0,
    }
}

/// Condition-code table: bit `nzcv` of `COND_TABLE[cond]` says whether
/// condition `cond` passes for flags `nzcv`.
const COND_TABLE: [u16; 16] = {
    let mut t = [0u16; 16];
    let mut cond = 0;
    while cond < 16 {
        let mut flags = 0;
        while flags < 16 {
            let n = flags & 8 != 0;
            let z = flags & 4 != 0;
            let c = flags & 2 != 0;
            let v = flags & 1 != 0;
            let pass = match cond {
                0 => z,
                1 => !z,
                2 => c,
                3 => !c,
                4 => n,
                5 => !n,
                6 => v,
                7 => !v,
                8 => c && !z,
                9 => !c || z,
                10 => n == v,
                11 => n != v,
                12 => !z && n == v,
                13 => z || n != v,
                14 => true,
                _ => false, // NV (handled specially on ARMv5)
            };
            if pass {
                t[cond] |= 1 << flags;
            }
            flags += 1;
        }
        cond += 1;
    }
    t
};

/// An ARM7TDMI or ARM946E-S core.
#[derive(Debug, Clone)]
pub struct Arm {
    pub model: CpuModel,
    /// Active registers. `r[15]` is the fetch address (current + 8/4).
    pub r: [u32; 16],
    pub cpsr: u32,
    /// SPSR per bank (index 0 = usr/sys, unused).
    pub(crate) spsr_bank: [u32; 6],
    /// Banked r13/r14 per bank.
    pub(crate) sp_lr_bank: [[u32; 2]; 6],
    /// r8–r12 for non-FIQ modes when FIQ is active, and vice versa.
    pub(crate) r8_12_usr: [u32; 5],
    pub(crate) r8_12_fiq: [u32; 5],
    pub(crate) pipeline: [u32; 2],
    pub(crate) flushed: bool,
    pub(crate) next_fetch: Access,
    /// CPU is in low-power halt until an interrupt is requested.
    pub halted: bool,
    /// Level of the IRQ input line (IE & IF & IME, computed by the system).
    pub irq_line: bool,
    pub fiq_line: bool,
    /// Instructions retired (for the debugger / profiling).
    pub retired: u64,
}

impl Arm {
    pub fn new(model: CpuModel) -> Self {
        Self {
            model,
            r: [0; 16],
            cpsr: MODE_SVC | FLAG_I | FLAG_F,
            spsr_bank: [0; 6],
            sp_lr_bank: [[0; 2]; 6],
            r8_12_usr: [0; 5],
            r8_12_fiq: [0; 5],
            pipeline: [0; 2],
            flushed: false,
            next_fetch: Access::NonSeq,
            halted: false,
            irq_line: false,
            fiq_line: false,
            retired: 0,
        }
    }

    #[inline]
    pub fn is_v5(&self) -> bool {
        self.model == CpuModel::Arm946Es
    }

    #[inline]
    pub fn thumb(&self) -> bool {
        self.cpsr & FLAG_T != 0
    }

    #[inline]
    pub fn mode(&self) -> u32 {
        self.cpsr & 0x1F
    }

    /// Address of the instruction that will execute next.
    pub fn pc(&self) -> u32 {
        self.r[15].wrapping_sub(if self.thumb() { 4 } else { 8 })
    }

    pub fn spsr(&self) -> u32 {
        let b = bank_of(self.mode());
        if b == 0 {
            self.cpsr
        } else {
            self.spsr_bank[b]
        }
    }

    pub fn set_spsr(&mut self, v: u32) {
        let b = bank_of(self.mode());
        if b != 0 {
            self.spsr_bank[b] = v;
        }
    }

    /// Read r13/r14 of another mode (used by HLE and debugger).
    pub fn banked_sp_lr(&self, mode: u32) -> [u32; 2] {
        if bank_of(mode) == bank_of(self.mode()) {
            [self.r[13], self.r[14]]
        } else {
            self.sp_lr_bank[bank_of(mode)]
        }
    }

    pub fn set_banked_sp(&mut self, mode: u32, sp: u32) {
        if bank_of(mode) == bank_of(self.mode()) {
            self.r[13] = sp;
        } else {
            self.sp_lr_bank[bank_of(mode)][0] = sp;
        }
    }

    /// Switch processor mode, swapping banked registers.
    pub fn switch_mode(&mut self, new_mode: u32) {
        let old = self.mode();
        let new_mode = new_mode & 0x1F;
        if old == new_mode {
            return;
        }
        let ob = bank_of(old);
        let nb = bank_of(new_mode);
        if ob != nb {
            self.sp_lr_bank[ob] = [self.r[13], self.r[14]];
            [self.r[13], self.r[14]] = self.sp_lr_bank[nb];
        }
        let old_fiq = old == MODE_FIQ;
        let new_fiq = new_mode == MODE_FIQ;
        if old_fiq != new_fiq {
            let (save, load) = if new_fiq {
                (&mut self.r8_12_usr, &self.r8_12_fiq)
            } else {
                (&mut self.r8_12_fiq, &self.r8_12_usr)
            };
            save.copy_from_slice(&self.r[8..13]);
            self.r[8..13].copy_from_slice(load);
        }
        self.cpsr = (self.cpsr & !0x1F) | new_mode;
    }

    /// Write the whole CPSR (MSR, exception return), handling mode changes.
    pub fn set_cpsr(&mut self, v: u32) {
        self.switch_mode(v & 0x1F);
        self.cpsr = v;
    }

    /// User-bank register (for `LDM/STM` with the S bit).
    pub(crate) fn user_reg(&self, n: usize) -> u32 {
        let mode = self.mode();
        match n {
            8..=12 if mode == MODE_FIQ => self.r8_12_usr[n - 8],
            13 | 14 if bank_of(mode) != 0 => self.sp_lr_bank[0][n - 13],
            _ => self.r[n],
        }
    }

    pub(crate) fn set_user_reg(&mut self, n: usize, v: u32) {
        let mode = self.mode();
        match n {
            8..=12 if mode == MODE_FIQ => self.r8_12_usr[n - 8] = v,
            13 | 14 if bank_of(mode) != 0 => self.sp_lr_bank[0][n - 13] = v,
            _ => self.r[n] = v,
        }
    }

    #[inline]
    pub(crate) fn cond_passes(&self, cond: u32) -> bool {
        COND_TABLE[cond as usize] & (1 << (self.cpsr >> 28)) != 0
    }

    #[inline]
    pub(crate) fn set_nz(&mut self, v: u32) {
        self.cpsr = (self.cpsr & !(FLAG_N | FLAG_Z)) | (v & FLAG_N) | if v == 0 { FLAG_Z } else { 0 };
    }

    #[inline]
    pub(crate) fn set_flag(&mut self, flag: u32, on: bool) {
        if on {
            self.cpsr |= flag;
        } else {
            self.cpsr &= !flag;
        }
    }

    #[inline]
    pub(crate) fn carry(&self) -> bool {
        self.cpsr & FLAG_C != 0
    }

    /// `a + b + carry_in` with full NZCV.
    #[inline]
    pub(crate) fn add_flags(&mut self, a: u32, b: u32, carry_in: bool) -> u32 {
        let wide = a as u64 + b as u64 + carry_in as u64;
        let res = wide as u32;
        self.set_nz(res);
        self.set_flag(FLAG_C, wide > 0xFFFF_FFFF);
        self.set_flag(FLAG_V, (!(a ^ b) & (a ^ res)) >> 31 != 0);
        res
    }

    /// `a - b - !carry_in` with full NZCV (C = no borrow).
    #[inline]
    pub(crate) fn sub_flags(&mut self, a: u32, b: u32, carry_in: bool) -> u32 {
        let borrow = !carry_in as u64;
        let res = (a as u64).wrapping_sub(b as u64).wrapping_sub(borrow) as u32;
        self.set_nz(res);
        self.set_flag(FLAG_C, a as u64 >= b as u64 + borrow);
        self.set_flag(FLAG_V, ((a ^ b) & (a ^ res)) >> 31 != 0);
        res
    }

    /// Jump to `addr` in the current instruction set and refill the
    /// pipeline (2 fetches: N then S).
    pub fn branch_to<B: Bus>(&mut self, bus: &mut B, addr: u32) {
        if self.thumb() {
            let a = addr & !1;
            self.pipeline[0] = bus.fetch16(a, Access::NonSeq) as u32;
            self.pipeline[1] = bus.fetch16(a.wrapping_add(2), Access::Seq) as u32;
            self.r[15] = a.wrapping_add(4);
        } else {
            let a = addr & !3;
            self.pipeline[0] = bus.fetch32(a, Access::NonSeq);
            self.pipeline[1] = bus.fetch32(a.wrapping_add(4), Access::Seq);
            self.r[15] = a.wrapping_add(8);
        }
        self.next_fetch = Access::Seq;
        self.flushed = true;
    }

    /// Branch with exchange: bit 0 selects Thumb.
    pub(crate) fn branch_exchange<B: Bus>(&mut self, bus: &mut B, addr: u32) {
        self.set_flag(FLAG_T, addr & 1 != 0);
        self.branch_to(bus, addr);
    }

    /// Put the CPU at `entry` in `mode` (used by HLE boot / direct boot).
    pub fn boot<B: Bus>(&mut self, bus: &mut B, entry: u32, mode: u32, thumb: bool) {
        self.switch_mode(mode);
        self.cpsr = (self.cpsr & !(FLAG_T | FLAG_I | FLAG_F | 0x1F)) | mode | if thumb { FLAG_T } else { 0 };
        self.halted = false;
        self.branch_to(bus, entry);
        self.flushed = false;
    }

    /// Hardware reset: SVC mode, IRQ/FIQ masked, jump to the reset vector.
    pub fn reset<B: Bus>(&mut self, bus: &mut B) {
        let model = self.model;
        *self = Self::new(model);
        self.branch_to(bus, bus.exception_base());
        self.flushed = false;
    }

    /// Enter an exception. `return_addr` is the value placed in the new
    /// mode's LR.
    pub fn exception<B: Bus>(&mut self, bus: &mut B, ex: Exception, return_addr: u32) {
        let old_cpsr = self.cpsr;
        self.switch_mode(ex.mode());
        self.spsr_bank[bank_of(ex.mode())] = old_cpsr;
        self.r[14] = return_addr;
        self.cpsr &= !FLAG_T;
        self.cpsr |= FLAG_I;
        if matches!(ex, Exception::Reset | Exception::Fiq) {
            self.cpsr |= FLAG_F;
        }
        let base = bus.exception_base();
        self.branch_to(bus, base.wrapping_add(ex.vector()));
    }

    /// Software interrupt taken from the instruction just executed.
    pub(crate) fn software_interrupt<B: Bus>(&mut self, bus: &mut B, comment: u32) {
        let insn_addr = self.pc();
        let next = insn_addr.wrapping_add(if self.thumb() { 2 } else { 4 });
        match bus.hle_swi(self, comment) {
            crate::SwiOutcome::Handled => {}
            crate::SwiOutcome::HaltAndRepeat => {
                self.halted = true;
                self.branch_to(bus, insn_addr);
            }
            crate::SwiOutcome::NotHandled => self.exception(bus, Exception::Swi, next),
        }
    }

    pub(crate) fn undefined<B: Bus>(&mut self, bus: &mut B, insn: u32) {
        tracing::warn!(
            "undefined {} instruction {insn:#010x} at {:#010x}",
            if self.thumb() { "Thumb" } else { "ARM" },
            self.pc()
        );
        let next = self.pc().wrapping_add(if self.thumb() { 2 } else { 4 });
        self.exception(bus, Exception::Undefined, next);
    }

    /// Execute one instruction (or take a pending interrupt). Does nothing
    /// while halted.
    pub fn step<B: Bus>(&mut self, bus: &mut B) {
        if self.halted {
            return;
        }
        if self.fiq_line && self.cpsr & FLAG_F == 0 {
            let ret = self.pc().wrapping_add(4);
            self.exception(bus, Exception::Fiq, ret);
            return;
        }
        if self.irq_line && self.cpsr & FLAG_I == 0 {
            let ret = self.pc().wrapping_add(4);
            self.exception(bus, Exception::Irq, ret);
            return;
        }
        self.flushed = false;
        self.retired += 1;
        if self.thumb() {
            let insn = self.pipeline[0] as u16;
            self.pipeline[0] = self.pipeline[1];
            self.pipeline[1] = bus.fetch16(self.r[15] & !1, self.next_fetch) as u32;
            self.next_fetch = Access::Seq;
            thumb::execute(self, bus, insn);
            if !self.flushed {
                self.r[15] = self.r[15].wrapping_add(2);
            }
        } else {
            let insn = self.pipeline[0];
            self.pipeline[0] = self.pipeline[1];
            self.pipeline[1] = bus.fetch32(self.r[15] & !3, self.next_fetch);
            self.next_fetch = Access::Seq;
            arm::execute(self, bus, insn);
            if !self.flushed {
                self.r[15] = self.r[15].wrapping_add(4);
            }
        }
    }

    /// Wake from halt (the system calls this when `IE & IF != 0`).
    pub fn wake(&mut self) {
        self.halted = false;
    }
}

impl SaveState for Arm {
    fn save_state(&self, w: &mut StateWriter) {
        w.u32s(&self.r);
        w.u32(self.cpsr);
        w.u32s(&self.spsr_bank);
        for b in &self.sp_lr_bank {
            w.u32s(b);
        }
        w.u32s(&self.r8_12_usr);
        w.u32s(&self.r8_12_fiq);
        w.u32s(&self.pipeline);
        w.bool(self.next_fetch == Access::Seq);
        w.bool(self.halted);
        w.bool(self.irq_line);
        w.bool(self.fiq_line);
        w.u64(self.retired);
    }

    fn load_state(&mut self, r: &mut StateReader<'_>) -> StateResult<()> {
        r.u32s_into(&mut self.r)?;
        self.cpsr = r.u32()?;
        r.u32s_into(&mut self.spsr_bank)?;
        for b in &mut self.sp_lr_bank {
            r.u32s_into(b)?;
        }
        r.u32s_into(&mut self.r8_12_usr)?;
        r.u32s_into(&mut self.r8_12_fiq)?;
        r.u32s_into(&mut self.pipeline)?;
        self.next_fetch = if r.bool()? { Access::Seq } else { Access::NonSeq };
        self.halted = r.bool()?;
        self.irq_line = r.bool()?;
        self.fiq_line = r.bool()?;
        self.retired = r.u64()?;
        Ok(())
    }
}
