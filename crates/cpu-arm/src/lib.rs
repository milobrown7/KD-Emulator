//! # cpu-arm
//!
//! Interpreter for the two ARM cores found in Nintendo handhelds:
//!
//! * **ARM7TDMI** (ARMv4T) — GBA main CPU, NDS sound/IO CPU.
//! * **ARM946E-S** (ARMv5TE) — NDS main CPU, adds `BLX`, `CLZ`, the DSP
//!   saturating/halfword multiplies, `LDRD/STRD`, interworking loads and the
//!   CP15 system control coprocessor ([`cp15::Cp15`]) with ITCM/DTCM.
//!
//! The 3-stage pipeline is modelled with a two-entry prefetch queue, so `PC`
//! reads as the current instruction + 8 (ARM) / + 4 (Thumb) and stores into
//! the next two instructions are not seen until a pipeline flush.
//!
//! Decoding uses compile-time lookup tables: 4096 entries for ARM (bits
//! 27–20 and 7–4) and 1024 entries for Thumb (bits 15–6).

mod arm;
pub mod asm;
pub mod cp15;
mod cpu;
pub mod hle;
mod thumb;

pub use cpu::*;

/// Bus cycle type, used by the memory system to apply wait states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    NonSeq,
    Seq,
}

/// Which ARM core is being emulated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuModel {
    /// ARMv4T: GBA, NDS7.
    Arm7Tdmi,
    /// ARMv5TE: NDS9.
    Arm946Es,
}

/// Result of a high-level-emulated software interrupt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwiOutcome {
    /// Not intercepted: take the SWI exception normally.
    NotHandled,
    /// Completed; continue after the SWI instruction.
    Handled,
    /// Halt the CPU and execute the same SWI again after the next interrupt
    /// (`IntrWait` / `VBlankIntrWait`).
    HaltAndRepeat,
}

/// Memory/IO interface seen by the CPU.
///
/// Addresses passed to 16/32-bit accessors are already aligned; the CPU
/// applies the architecture's rotation rules for unaligned loads. The bus
/// accumulates its own cycle count (wait states, internal cycles).
pub trait Bus {
    fn read8(&mut self, addr: u32, access: Access) -> u8;
    fn read16(&mut self, addr: u32, access: Access) -> u16;
    fn read32(&mut self, addr: u32, access: Access) -> u32;
    fn write8(&mut self, addr: u32, value: u8, access: Access);
    /// Halfword/word stores pass the address unaligned, as it appears on the
    /// bus: implementations align it themselves, except for devices that see
    /// the low bits (the GBA's 8-bit save chip).
    fn write16(&mut self, addr: u32, value: u16, access: Access);
    fn write32(&mut self, addr: u32, value: u32, access: Access);

    /// Opcode fetch (lets the bus model prefetch buffers or BIOS protection).
    fn fetch16(&mut self, addr: u32, access: Access) -> u16 {
        self.read16(addr, access)
    }
    fn fetch32(&mut self, addr: u32, access: Access) -> u32 {
        self.read32(addr, access)
    }

    /// Internal (I) cycles.
    fn idle(&mut self, cycles: u32);

    /// High-level BIOS hook, called for every `SWI` before the exception is
    /// taken. `comment` is the 24-bit (ARM) or 8-bit (Thumb) immediate.
    fn hle_swi(&mut self, _cpu: &mut Arm, _comment: u32) -> SwiOutcome {
        SwiOutcome::NotHandled
    }

    /// `MRC`: return `None` if the coprocessor is absent (undefined trap).
    fn cp_read(&mut self, _cp: u8, _op1: u8, _crn: u8, _crm: u8, _op2: u8) -> Option<u32> {
        None
    }
    /// `MCR`: return `false` if the coprocessor is absent.
    fn cp_write(&mut self, _cp: u8, _op1: u8, _crn: u8, _crm: u8, _op2: u8, _value: u32) -> bool {
        false
    }

    /// Exception vector base: `0x00000000`, or `0xFFFF0000` when the ARM9's
    /// CP15 high-vectors bit is set.
    fn exception_base(&self) -> u32 {
        0
    }
}
