//! # dsp-gc
//!
//! The GameCube/Wii audio subsystem:
//!
//! * [`DspInterface`] — the CPU-visible DSP registers at 0xCC005000:
//!   mailboxes, DSPCR (reset/halt/interrupts), ARAM DMA and audio (AI) DMA.
//! * 16 MiB ARAM (GameCube; Wii keeps the same interface).
//! * [`adpcm`] — DSP-ADPCM decoding.
//! * [`ax`] — high-level AX voice mixing (default; no firmware needed).
//! * [`lle`] — low-level Macronix DSP interpreter (needs user-supplied
//!   IROM/DROM dumps).
//!
//! The thermal policy in `kd-core` switches to HLE automatically at
//! `THERMAL_STATUS_MODERATE`.

pub mod adpcm;
pub mod ax;
pub mod lle;

use kd_core::thermal::DspMode;
use lle::{DspCore, DspMainMemory};

pub const ARAM_SIZE: usize = 16 * 1024 * 1024;

pub const DSPCR_RESET: u16 = 0x0001;
pub const DSPCR_PIINT: u16 = 0x0002;
pub const DSPCR_HALT: u16 = 0x0004;
pub const DSPCR_AIDINT: u16 = 0x0008;
pub const DSPCR_AIDINTMSK: u16 = 0x0010;
pub const DSPCR_ARINT: u16 = 0x0020;
pub const DSPCR_ARINTMSK: u16 = 0x0040;
pub const DSPCR_DSPINT: u16 = 0x0080;
pub const DSPCR_DSPINTMSK: u16 = 0x0100;
pub const DSPCR_DSPDMA: u16 = 0x0200;
pub const DSPCR_RESETINIT: u16 = 0x0800;

/// Main RAM access for ARAM and AI DMA.
pub trait MainMemory {
    fn read8(&self, addr: u32) -> u8;
    fn write8(&mut self, addr: u32, v: u8);
}

struct Adapter<'a>(&'a mut dyn MainMemory);
impl DspMainMemory for Adapter<'_> {
    fn read8(&self, a: u32) -> u8 {
        self.0.read8(a)
    }
    fn write8(&mut self, a: u32, v: u8) {
        self.0.write8(a, v)
    }
}

pub struct DspInterface {
    pub mode: DspMode,
    pub core: DspCore,
    pub aram: Vec<u8>,
    pub dspcr: u16,
    ar_mmaddr: u32,
    ar_araddr: u32,
    ar_cnt: u32,
    pub ar_size: u16,
    pub ar_mode: u16,
    pub ar_refresh: u16,
    ai_start: u32,
    ai_ctrl: u16,
    ai_left: u16,
    /// Stereo samples pulled by AI DMA (big-endian PCM16 in RAM).
    pub ai_samples: Vec<[i16; 2]>,
}

impl Default for DspInterface {
    fn default() -> Self {
        Self::new()
    }
}

impl DspInterface {
    pub fn new() -> Self {
        let mut core = DspCore::new();
        // The IPL ROM greets the CPU with this mail after reset.
        core.dsp_mail = 0x8071_FEED;
        Self {
            mode: DspMode::Hle,
            core,
            aram: vec![0; ARAM_SIZE],
            dspcr: 0,
            ar_mmaddr: 0,
            ar_araddr: 0,
            ar_cnt: 0,
            ar_size: 0,
            ar_mode: 1,
            ar_refresh: 156,
            ai_start: 0,
            ai_ctrl: 0,
            ai_left: 0,
            ai_samples: Vec::new(),
        }
    }

    /// Interrupt line towards the CPU (Processor Interface DSP bit).
    pub fn interrupt(&self) -> bool {
        let c = self.dspcr;
        (c & DSPCR_AIDINT != 0 && c & DSPCR_AIDINTMSK != 0)
            || (c & DSPCR_ARINT != 0 && c & DSPCR_ARINTMSK != 0)
            || (c & DSPCR_DSPINT != 0 && c & DSPCR_DSPINTMSK != 0)
    }

    pub fn read16(&mut self, off: u32) -> u16 {
        match off & 0x3E {
            0x00 => (self.core.cpu_mail >> 16) as u16,
            0x02 => self.core.cpu_mail as u16,
            0x04 => (self.core.dsp_mail >> 16) as u16,
            0x06 => {
                let v = self.core.dsp_mail as u16;
                self.core.dsp_mail &= 0x7FFF_FFFF;
                v
            }
            0x0A => self.dspcr,
            0x12 => self.ar_size,
            0x16 => self.ar_mode,
            0x1A => self.ar_refresh,
            0x20 => (self.ar_mmaddr >> 16) as u16,
            0x22 => self.ar_mmaddr as u16,
            0x24 => (self.ar_araddr >> 16) as u16,
            0x26 => self.ar_araddr as u16,
            0x28 => (self.ar_cnt >> 16) as u16,
            0x2A => self.ar_cnt as u16,
            0x30 => (self.ai_start >> 16) as u16,
            0x32 => self.ai_start as u16,
            0x36 => self.ai_ctrl,
            0x3A => self.ai_left,
            _ => 0,
        }
    }

    pub fn write16(&mut self, off: u32, v: u16, mem: &mut dyn MainMemory) {
        match off & 0x3E {
            0x00 => self.core.cpu_mail = (self.core.cpu_mail & 0xFFFF) | (v as u32) << 16,
            0x02 => {
                let mail = (self.core.cpu_mail & 0x7FFF_0000) | v as u32;
                self.core.cpu_write_mail(mail);
            }
            0x0A => {
                // Interrupt status bits are write-1-to-clear.
                let ints = DSPCR_AIDINT | DSPCR_ARINT | DSPCR_DSPINT;
                let pending = self.dspcr & ints & !(v & ints);
                self.dspcr = pending | (v & !(ints | DSPCR_RESET));
                if v & DSPCR_RESET != 0 {
                    let from_iram = v & DSPCR_RESETINIT == 0;
                    self.core.reset(from_iram);
                }
                self.core.halted = v & DSPCR_HALT != 0;
            }
            0x12 => self.ar_size = v,
            0x16 => self.ar_mode = v,
            0x1A => self.ar_refresh = v,
            0x20 => self.ar_mmaddr = (self.ar_mmaddr & 0xFFFF) | (v as u32) << 16,
            0x22 => self.ar_mmaddr = (self.ar_mmaddr & 0xFFFF_0000) | (v & !31) as u32,
            0x24 => self.ar_araddr = (self.ar_araddr & 0xFFFF) | (v as u32) << 16,
            0x26 => self.ar_araddr = (self.ar_araddr & 0xFFFF_0000) | (v & !31) as u32,
            0x28 => self.ar_cnt = (self.ar_cnt & 0xFFFF) | (v as u32) << 16,
            0x2A => {
                self.ar_cnt = (self.ar_cnt & 0xFFFF_0000) | (v & !31) as u32;
                self.aram_dma(mem);
            }
            0x30 => self.ai_start = (self.ai_start & 0xFFFF) | ((v as u32 & 0x03FF) << 16),
            0x32 => self.ai_start = (self.ai_start & 0xFFFF_0000) | (v & !31) as u32,
            0x36 => {
                self.ai_ctrl = v;
                self.ai_left = v & 0x7FFF;
            }
            _ => {}
        }
    }

    /// ARAM ↔ main RAM DMA (bit 31 of the count selects ARAM → RAM).
    fn aram_dma(&mut self, mem: &mut dyn MainMemory) {
        let to_ram = self.ar_cnt & 0x8000_0000 != 0;
        let len = (self.ar_cnt & 0x7FFF_FFFF) as usize;
        for i in 0..len {
            let m = self.ar_mmaddr + i as u32;
            let a = (self.ar_araddr as usize + i) % ARAM_SIZE;
            if to_ram {
                mem.write8(m, self.aram[a]);
            } else {
                self.aram[a] = mem.read8(m);
            }
        }
        self.ar_mmaddr += len as u32;
        self.ar_araddr += len as u32;
        self.ar_cnt &= 0x8000_0000;
        self.dspcr |= DSPCR_ARINT;
    }

    /// Pull one 32-byte block of audio through AI DMA (called by the AI at
    /// its sample rate). Returns false when the transfer is idle.
    pub fn ai_dma_block(&mut self, mem: &dyn MainMemory) -> bool {
        if self.ai_ctrl & 0x8000 == 0 {
            return false;
        }
        let blocks_total = (self.ai_ctrl & 0x7FFF) as u32;
        let done = blocks_total - self.ai_left as u32;
        let base = self.ai_start + done * 32;
        for f in 0..8 {
            let a = base + f * 4;
            let l = i16::from_be_bytes([mem.read8(a), mem.read8(a + 1)]);
            let r = i16::from_be_bytes([mem.read8(a + 2), mem.read8(a + 3)]);
            self.ai_samples.push([l, r]);
        }
        self.ai_left = self.ai_left.saturating_sub(1);
        if self.ai_left == 0 {
            self.ai_left = self.ai_ctrl & 0x7FFF; // restarts from the start address
            self.dspcr |= DSPCR_AIDINT;
        }
        true
    }

    /// Run the LLE core for `cycles` (no-op in HLE mode).
    pub fn run_lle(&mut self, cycles: u64, mem: &mut dyn MainMemory) {
        if self.mode != DspMode::Lle {
            return;
        }
        self.core.run(&mut Adapter(mem), cycles);
        if std::mem::take(&mut self.core.interrupt_cpu) {
            self.dspcr |= DSPCR_DSPINT;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Ram(Vec<u8>);
    impl MainMemory for Ram {
        fn read8(&self, a: u32) -> u8 {
            self.0[a as usize]
        }
        fn write8(&mut self, a: u32, v: u8) {
            self.0[a as usize] = v;
        }
    }

    #[test]
    fn boot_mail_and_mailbox_ack() {
        let mut d = DspInterface::new();
        assert_eq!(d.read16(0x04), 0x8071);
        assert_eq!(d.read16(0x06), 0xFEED);
        assert_eq!(d.read16(0x04) & 0x8000, 0, "reading the low half acknowledges");
        let mut ram = Ram(vec![0; 64]);
        d.write16(0x00, 0x1234, &mut ram);
        d.write16(0x02, 0x5678, &mut ram);
        assert_eq!(d.read16(0x00), 0x9234, "pending bit set");
    }

    #[test]
    fn aram_dma_both_directions_with_interrupt() {
        let mut d = DspInterface::new();
        let mut ram = Ram(vec![0; 0x1000]);
        ram.0[0x100..0x120].copy_from_slice(&[7u8; 32]);
        d.write16(0x0A, DSPCR_ARINTMSK, &mut ram);
        for (off, v) in [(0x20, 0), (0x22, 0x100), (0x24, 0), (0x26, 0x40), (0x28, 0), (0x2A, 32)] {
            d.write16(off, v, &mut ram);
        }
        assert_eq!(&d.aram[0x40..0x60], &[7u8; 32]);
        assert!(d.interrupt());
        d.write16(0x0A, DSPCR_ARINT | DSPCR_ARINTMSK, &mut ram);
        assert!(!d.interrupt(), "acknowledged");
        for (off, v) in [(0x20, 0), (0x22, 0x200), (0x24, 0), (0x26, 0x40), (0x28, 0x8000), (0x2A, 32)] {
            d.write16(off, v, &mut ram);
        }
        assert_eq!(&ram.0[0x200..0x220], &[7u8; 32]);
    }

    #[test]
    fn ai_dma_reads_pcm_blocks() {
        let mut d = DspInterface::new();
        let mut ram = Ram(vec![0; 0x1000]);
        ram.0[0x400..0x404].copy_from_slice(&[0x12, 0x34, 0xFF, 0xFE]);
        d.write16(0x30, 0, &mut ram);
        d.write16(0x32, 0x400, &mut ram);
        d.write16(0x36, 0x8000 | 2, &mut ram);
        assert!(d.ai_dma_block(&ram));
        assert_eq!(d.ai_samples[0], [0x1234, -2]);
        assert!(d.ai_dma_block(&ram));
        assert_ne!(d.dspcr & DSPCR_AIDINT, 0, "buffer finished");
        assert_eq!(d.ai_samples.len(), 16);
    }
}
