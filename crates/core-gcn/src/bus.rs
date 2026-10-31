//! Flipper (and Hollywood-compatible) system bus.
//!
//! Physical map: MEM1 (24 MiB at 0), EFB at 0x08000000, MMIO at
//! 0x0C000000 (CP 0000, PE 1000, VI 2000, PI 3000, MI 4000, DSP 5000,
//! DI 6000, SI 6400, EXI 6800, AI 6C00, GX gather pipe 8000), locked L1
//! cache at 0xE0000000. Wii-only regions (Hollywood registers at
//! 0x0D000000 and MEM2 at 0x10000000) are delegated to a
//! [`HollywoodHooks`] implementation supplied by `core-wii`.

use crate::disc::Disc;
use crate::exi::ExiChannel;
use crate::si::SerialInterface;
use cpu_ppc::PpcBus;
use dsp_gc::{DspInterface, MainMemory};
use gpu_gx_vk::{Gx, GxEvent, GxMemory};
use kd_core::arena::PageAwareArena;
use kd_core::GuestClock;

pub const MEM1_SIZE: usize = 24 * 1024 * 1024;
pub const LOCKED_CACHE_SIZE: usize = 16 * 1024;

pub const PI_DI: u32 = 1 << 2;
pub const PI_SI: u32 = 1 << 3;
pub const PI_EXI: u32 = 1 << 4;
pub const PI_AI: u32 = 1 << 5;
pub const PI_DSP: u32 = 1 << 6;
pub const PI_VI: u32 = 1 << 8;
pub const PI_PE_TOKEN: u32 = 1 << 9;
pub const PI_PE_FINISH: u32 = 1 << 10;
pub const PI_CP: u32 = 1 << 11;
pub const PI_HOLLYWOOD: u32 = 1 << 14;

/// Wii extension points (Hollywood registers, MEM2, IOS IPC).
pub trait HollywoodHooks: Send {
    fn read32(&mut self, off: u32, mem1: &mut [u8]) -> u32;
    fn write32(&mut self, off: u32, v: u32, mem1: &mut [u8]);
    fn mem2(&mut self) -> &mut [u8];
    /// Interrupt towards the PPC (PI bit 14).
    fn interrupt(&self) -> bool;
}

/// Video Interface registers (16-bit, some accessed as 32-bit pairs).
pub struct VideoInterface {
    pub regs: [u16; 0x40],
    /// Current line (1-based within the field).
    pub line: u16,
}

impl Default for VideoInterface {
    fn default() -> Self {
        let mut regs = [0u16; 0x40];
        regs[0] = 0x0F06; // VTR: 240 active lines, 6 equalisation
        regs[1] = 0x0001; // DCR enabled
        regs[0x24] = 640 / 16; // HSW: stride 640 pixels
        Self { regs, line: 1 }
    }
}

impl VideoInterface {
    pub const LINES_PER_FIELD: u16 = 263;

    fn reg32(&self, off: usize) -> u32 {
        (self.regs[off / 2] as u32) << 16 | self.regs[off / 2 + 1] as u32
    }

    /// Top-field framebuffer address.
    pub fn xfb_address(&self) -> u32 {
        let v = self.reg32(0x1C);
        let addr = v & 0x00FF_FFFF;
        if v & (1 << 28) != 0 {
            addr << 5
        } else {
            addr
        }
    }

    pub fn interrupt(&self) -> bool {
        (0..4).any(|i| {
            let di = self.reg32(0x30 + i * 4);
            di & (1 << 31) != 0 && di & (1 << 28) != 0
        })
    }

    /// Advance one line; raise display interrupts on match.
    pub fn next_line(&mut self) {
        self.line = if self.line >= Self::LINES_PER_FIELD { 1 } else { self.line + 1 };
        for i in 0..4 {
            let off = 0x30 + i * 4;
            let di = self.reg32(off);
            let vct = ((di >> 16) & 0x3FF) as u16;
            if di & (1 << 28) != 0 && vct == self.line {
                self.regs[off / 2] |= 0x8000;
            }
        }
    }

    pub fn read16(&self, off: u32) -> u16 {
        match off {
            0x2C => self.line,
            0x2E => 1,
            _ => self.regs[(off / 2) as usize & 0x3F],
        }
    }

    pub fn write16(&mut self, off: u32, v: u16) {
        let i = (off / 2) as usize & 0x3F;
        self.regs[i] = v;
    }
}

pub struct FlipperBus {
    pub mem1: PageAwareArena,
    pub locked_cache: Vec<u8>,
    pub gx: Gx,
    pub dsp: DspInterface,
    pub si: SerialInterface,
    pub exi: [ExiChannel; 3],
    pub vi: VideoInterface,
    pub disc: Option<Box<dyn Disc>>,
    pub clock: GuestClock,
    pub hollywood: Option<Box<dyn HollywoodHooks>>,
    pub pi_intmr: u32,
    pi_intsr_edge: u32,
    pub pi_fifo: [u32; 3],
    pub cp_regs: [u16; 0x40],
    pub pe_intrctrl: u16,
    pub pe_token: u16,
    pub di_regs: [u32; 10],
    pub ai_regs: [u32; 4],
    gather: Vec<u8>,
    /// Last EFB→XFB copy (address, width, height) for presentation.
    pub last_xfb: Option<(u32, u16, u16)>,
    pub xfb_copies: u64,
}

struct Ram<'a>(&'a mut [u8]);

impl GxMemory for Ram<'_> {
    fn read(&self, addr: u32, len: usize) -> Vec<u8> {
        let a = (addr & 0x01FF_FFFF) as usize;
        if a + len <= self.0.len() {
            self.0[a..a + len].to_vec()
        } else {
            vec![0; len]
        }
    }
    fn write(&mut self, addr: u32, data: &[u8]) {
        let a = (addr & 0x01FF_FFFF) as usize;
        if a + data.len() <= self.0.len() {
            self.0[a..a + data.len()].copy_from_slice(data);
        }
    }
}

impl MainMemory for Ram<'_> {
    fn read8(&self, a: u32) -> u8 {
        self.0.get((a & 0x01FF_FFFF) as usize).copied().unwrap_or(0)
    }
    fn write8(&mut self, a: u32, v: u8) {
        if let Some(s) = self.0.get_mut((a & 0x01FF_FFFF) as usize) {
            *s = v;
        }
    }
}

impl FlipperBus {
    pub fn new() -> std::io::Result<Self> {
        Ok(Self {
            mem1: PageAwareArena::new(c"kd_gcn_mem1", MEM1_SIZE)?,
            locked_cache: vec![0; LOCKED_CACHE_SIZE],
            gx: Gx::new(),
            dsp: DspInterface::new(),
            si: SerialInterface::default(),
            exi: Default::default(),
            vi: VideoInterface::default(),
            disc: None,
            clock: GuestClock::host(),
            hollywood: None,
            pi_intmr: 0,
            pi_intsr_edge: 0,
            pi_fifo: [0; 3],
            cp_regs: [0; 0x40],
            pe_intrctrl: 0,
            pe_token: 0,
            di_regs: [0; 10],
            ai_regs: [0; 4],
            gather: Vec::with_capacity(64),
            last_xfb: None,
            xfb_copies: 0,
        })
    }

    pub fn ram(&mut self) -> &mut [u8] {
        self.mem1.as_mut_slice()
    }

    /// Big-endian word at a physical address (debugger / tests).
    pub fn read_word(&mut self, pa: u32) -> u32 {
        PpcBus::read32(self, pa)
    }

    /// PI interrupt cause register, assembled from device state.
    pub fn intsr(&self) -> u32 {
        let mut v = self.pi_intsr_edge;
        if self.di_regs[0] & 0x14 == 0x14 || self.di_regs[0] & 0x6 == 0x6 {
            v |= PI_DI;
        }
        if self.si.interrupt() {
            v |= PI_SI;
        }
        if self.exi.iter().any(|c| c.interrupt()) {
            v |= PI_EXI;
        }
        if self.ai_regs[0] & 0xC == 0xC {
            v |= PI_AI;
        }
        if self.dsp.interrupt() {
            v |= PI_DSP;
        }
        if self.vi.interrupt() {
            v |= PI_VI;
        }
        if self.pe_intrctrl & 0x5 == 0x5 {
            v |= PI_PE_TOKEN;
        }
        if self.pe_intrctrl & 0xA == 0xA {
            v |= PI_PE_FINISH;
        }
        if self.hollywood.as_ref().is_some_and(|h| h.interrupt()) {
            v |= PI_HOLLYWOOD;
        }
        v | (1 << 16) // reset switch released
    }

    /// External interrupt line to the CPU.
    pub fn cpu_interrupt(&self) -> bool {
        self.intsr() & self.pi_intmr & 0x3FFF != 0
    }

    fn gather_write(&mut self, bytes: &[u8]) {
        self.gather.extend_from_slice(bytes);
        if self.gather.len() >= 32 {
            self.flush_gather();
        }
    }

    /// Hand gathered FIFO bytes to GX and react to its events.
    pub fn flush_gather(&mut self) {
        if self.gather.is_empty() {
            return;
        }
        let data = std::mem::take(&mut self.gather);
        let mut ram = Ram(self.mem1.as_mut_slice());
        self.gx.write_fifo(&data, &mut ram);
        self.pi_fifo[2] = self.pi_fifo[2].wrapping_add(data.len() as u32);
        for ev in std::mem::take(&mut self.gx.events) {
            match ev {
                GxEvent::Finish => self.pe_intrctrl |= 0x8,
                GxEvent::Token(t) => self.pe_token = t,
                GxEvent::TokenInterrupt(t) => {
                    self.pe_token = t;
                    self.pe_intrctrl |= 0x4;
                }
                GxEvent::XfbCopy { addr, width, height } => {
                    self.last_xfb = Some((addr, width, height));
                    self.xfb_copies += 1;
                }
            }
        }
    }

    // ------------------------------------------------------------- DVD

    fn di_command(&mut self) {
        let cmd = self.di_regs[2] >> 24;
        match cmd {
            0xA8 => {
                let offset = (self.di_regs[3] as u64) << 2;
                let len = self.di_regs[6] as usize;
                let dst = self.di_regs[5] & 0x03FF_FFE0;
                let mut buf = vec![0u8; len];
                if let Some(d) = self.disc.as_mut() {
                    if let Err(e) = d.read(offset, &mut buf) {
                        tracing::warn!("disc read failed: {e}");
                    }
                }
                let mut ram = Ram(self.mem1.as_mut_slice());
                GxMemory::write(&mut ram, dst, &buf);
            }
            0x12 => {
                let dst = self.di_regs[5] & 0x03FF_FFE0;
                let mut info = [0u8; 0x20];
                info[0..4].copy_from_slice(&0x0000_0002u32.to_be_bytes());
                info[4..8].copy_from_slice(&0x2000_0610u32.to_be_bytes());
                let mut ram = Ram(self.mem1.as_mut_slice());
                GxMemory::write(&mut ram, dst, &info);
            }
            0xE0 => self.di_regs[8] = 0, // no error
            0xAB | 0xE3 | 0xE4 => {}
            c => tracing::warn!("unknown DI command {c:#04x}"),
        }
        self.di_regs[6] = 0; // DILENGTH
        self.di_regs[7] &= !1; // DICR TSTART
        self.di_regs[0] |= 0x10; // TCINT
    }

    // ---------------------------------------------------------- MMIO

    fn mmio_read32(&mut self, pa: u32) -> u32 {
        let off = pa & 0xFFFF;
        match off & 0xFF00 {
            0x0000 => (self.mmio_read16(pa) as u32) << 16 | self.mmio_read16(pa + 2) as u32,
            0x1000 | 0x2000 | 0x5000 => (self.mmio_read16(pa) as u32) << 16 | self.mmio_read16(pa + 2) as u32,
            0x3000 => match off & 0xFF {
                0x00 => self.intsr(),
                0x04 => self.pi_intmr,
                0x0C => self.pi_fifo[0],
                0x10 => self.pi_fifo[1],
                0x14 => self.pi_fifo[2],
                0x2C => 0x2465_00B1, // Flipper revision
                _ => 0,
            },
            0x6000 => {
                let i = ((off & 0xFF) / 4) as usize;
                match off & 0xFF {
                    0x00..=0x24 => self.di_regs.get(i).copied().unwrap_or(0),
                    _ => 0,
                }
            }
            0x6400..=0x64FF => self.si.read32(off & 0xFF),
            0x6800..=0x68FF => {
                let ch = ((off & 0xFF) / 0x14) as usize;
                if ch >= 3 {
                    return 0;
                }
                let c = &self.exi[ch];
                match (off & 0xFF) % 0x14 {
                    0x00 => c.read_csr(),
                    0x04 => c.mar,
                    0x08 => c.length,
                    0x0C => c.cr,
                    _ => c.data,
                }
            }
            0x6C00 => match off & 0xFF {
                0x00 => self.ai_regs[0],
                0x04 => self.ai_regs[1],
                0x08 => self.ai_regs[2],
                0x0C => self.ai_regs[3],
                _ => 0,
            },
            _ => 0,
        }
    }

    fn mmio_read16(&mut self, pa: u32) -> u16 {
        let off = pa & 0xFFFF;
        match off & 0xF000 {
            0x0000 => match off & 0xFF {
                0x00 => 0x0C, // CP status: read idle, command idle
                o => self.cp_regs[(o / 2) as usize & 0x3F],
            },
            0x1000 => match off & 0xFF {
                0x0A => self.pe_intrctrl,
                0x0E => self.pe_token,
                _ => 0,
            },
            0x2000 => self.vi.read16(off & 0xFF),
            0x5000 => self.dsp.read16(off & 0xFF),
            _ => {
                let w = self.mmio_read32(pa & !3);
                (w >> if pa & 2 == 0 { 16 } else { 0 }) as u16
            }
        }
    }

    fn mmio_write32(&mut self, pa: u32, v: u32) {
        let off = pa & 0xFFFF;
        if off >= 0x8000 {
            self.gather_write(&v.to_be_bytes());
            return;
        }
        match off & 0xFF00 {
            0x0000 | 0x1000 | 0x2000 | 0x5000 => {
                self.mmio_write16(pa, (v >> 16) as u16);
                self.mmio_write16(pa + 2, v as u16);
            }
            0x3000 => match off & 0xFF {
                0x00 => self.pi_intsr_edge &= !v,
                0x04 => self.pi_intmr = v,
                0x0C => self.pi_fifo[0] = v,
                0x10 => self.pi_fifo[1] = v,
                0x14 => self.pi_fifo[2] = v,
                _ => {}
            },
            0x6000 => {
                let i = ((off & 0xFF) / 4) as usize;
                match off & 0xFF {
                    0x00 => {
                        // DISR: interrupt status bits are write-1-to-clear
                        let status = self.di_regs[0] & 0x54 & !(v & 0x54);
                        self.di_regs[0] = (v & !0x54) | status;
                    }
                    0x1C => {
                        self.di_regs[7] = v;
                        if v & 1 != 0 {
                            self.di_command();
                        }
                    }
                    _ if i < self.di_regs.len() => self.di_regs[i] = v,
                    _ => {}
                }
            }
            0x6400 => self.si.write32(off & 0xFF, v),
            0x6800 => {
                let ch = ((off & 0xFF) / 0x14) as usize;
                if ch >= 3 {
                    return;
                }
                let clock = self.clock;
                let ram = self.mem1.as_mut_slice();
                let c = &mut self.exi[ch];
                match (off & 0xFF) % 0x14 {
                    0x00 => c.write_csr(v),
                    0x04 => c.mar = v,
                    0x08 => c.length = v,
                    0x0C => c.write_cr(v, ram, &clock),
                    _ => c.data = v,
                }
            }
            0x6C00 => match off & 0xFF {
                0x00 => {
                    // AIINT is write-1-to-clear
                    let status = self.ai_regs[0] & 0x8 & !(v & 0x8);
                    self.ai_regs[0] = (v & !0x8) | status;
                    if v & 0x20 != 0 {
                        self.ai_regs[2] = 0;
                    }
                }
                o => self.ai_regs[(o / 4) as usize & 3] = v,
            },
            _ => {}
        }
    }

    fn mmio_write16(&mut self, pa: u32, v: u16) {
        let off = pa & 0xFFFF;
        if off >= 0x8000 {
            self.gather_write(&v.to_be_bytes());
            return;
        }
        match off & 0xF000 {
            0x0000 => self.cp_regs[((off & 0xFF) / 2) as usize & 0x3F] = v,
            0x1000 => match off & 0xFF {
                0x0A => {
                    let status = self.pe_intrctrl & 0xC & !(v & 0xC);
                    self.pe_intrctrl = (v & 0x3) | status;
                }
                0x0E => self.pe_token = v,
                _ => {}
            },
            0x2000 => {
                // Writing a display-interrupt word with bit 15 clear
                // acknowledges it.
                self.vi.write16(off & 0xFF, v);
            }
            0x5000 => {
                let mut ram = Ram(self.mem1.as_mut_slice());
                self.dsp.write16(off & 0xFF, v, &mut ram);
            }
            _ => {
                let cur = self.mmio_read32(pa & !3);
                let w = if pa & 2 == 0 { (cur & 0xFFFF) | (v as u32) << 16 } else { (cur & 0xFFFF_0000) | v as u32 };
                self.mmio_write32(pa & !3, w);
            }
        }
    }
}

impl PpcBus for FlipperBus {
    fn read8(&mut self, pa: u32) -> u8 {
        match pa >> 24 {
            0x00 | 0x01 => self.mem1.as_slice().get(pa as usize).copied().unwrap_or(0),
            0x10..=0x13 => match self.hollywood.as_mut() {
                Some(h) => h.mem2().get((pa & 0x03FF_FFFF) as usize).copied().unwrap_or(0),
                None => 0,
            },
            _ => (self.read32(pa & !3) >> ((3 - (pa & 3)) * 8)) as u8,
        }
    }

    fn read16(&mut self, pa: u32) -> u16 {
        match pa >> 24 {
            0x0C => self.mmio_read16(pa),
            _ => (self.read8(pa) as u16) << 8 | self.read8(pa + 1) as u16,
        }
    }

    fn read32(&mut self, pa: u32) -> u32 {
        match pa >> 24 {
            0x00 | 0x01 => {
                let a = pa as usize;
                self.mem1.as_slice().get(a..a + 4).map_or(0, |b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
            }
            0x08 => {
                // EFB peek: x/y encoded in the address.
                let x = ((pa >> 2) & 0x3FF) as usize;
                let y = ((pa >> 12) & 0x3FF) as usize;
                if x < 640 && y < 528 {
                    let p = self.gx.efb_pixel(x, y);
                    (p >> 24) << 24 | (p & 0xFF) << 16 | ((p >> 8) & 0xFF) << 8 | (p >> 16) & 0xFF
                } else {
                    0
                }
            }
            0x0C => self.mmio_read32(pa),
            // Wii: the legacy DI/SI/EXI/AI block is also mapped at 0x0D006000.
            0x0D if pa & 0x00FF_F000 == 0x6000 => self.mmio_read32(pa & 0xFFFF),
            0x0D => match self.hollywood.as_mut() {
                Some(h) => h.read32(pa & 0x00FF_FFFF, self.mem1.as_mut_slice()),
                None => 0,
            },
            0x10..=0x13 => (self.read16(pa) as u32) << 16 | self.read16(pa + 2) as u32,
            0xE0 => {
                let a = (pa as usize) & (LOCKED_CACHE_SIZE - 1);
                u32::from_be_bytes(self.locked_cache[a..a + 4].try_into().unwrap())
            }
            _ => 0,
        }
    }

    fn write8(&mut self, pa: u32, v: u8) {
        match pa >> 24 {
            0x00 | 0x01 => {
                if let Some(s) = self.mem1.as_mut_slice().get_mut(pa as usize) {
                    *s = v;
                }
            }
            0x0C if pa & 0xFFFF >= 0x8000 => self.gather_write(&[v]),
            0x10..=0x13 => {
                if let Some(h) = self.hollywood.as_mut() {
                    if let Some(s) = h.mem2().get_mut((pa & 0x03FF_FFFF) as usize) {
                        *s = v;
                    }
                }
            }
            0xE0 => self.locked_cache[pa as usize & (LOCKED_CACHE_SIZE - 1)] = v,
            _ => tracing::warn!("8-bit write {v:#04x} to {pa:#010x}"),
        }
    }

    fn write16(&mut self, pa: u32, v: u16) {
        match pa >> 24 {
            0x0C => self.mmio_write16(pa, v),
            _ => {
                self.write8(pa, (v >> 8) as u8);
                self.write8(pa + 1, v as u8);
            }
        }
    }

    fn write32(&mut self, pa: u32, v: u32) {
        match pa >> 24 {
            0x00 | 0x01 => {
                let a = pa as usize;
                if let Some(s) = self.mem1.as_mut_slice().get_mut(a..a + 4) {
                    s.copy_from_slice(&v.to_be_bytes());
                }
            }
            0x0C => self.mmio_write32(pa, v),
            0x0D if pa & 0x00FF_F000 == 0x6000 => self.mmio_write32(pa & 0xFFFF, v),
            0x0D => {
                if let Some(h) = self.hollywood.as_mut() {
                    h.write32(pa & 0x00FF_FFFF, v, self.mem1.as_mut_slice());
                }
            }
            0x10..=0x13 | 0xE0 => {
                for (i, b) in v.to_be_bytes().iter().enumerate() {
                    self.write8(pa + i as u32, *b);
                }
            }
            _ => tracing::warn!("32-bit write {v:#010x} to unmapped {pa:#010x}"),
        }
    }

    fn zero_block(&mut self, pa: u32) {
        for i in 0..8 {
            self.write32(pa + i * 4, 0);
        }
    }
}
