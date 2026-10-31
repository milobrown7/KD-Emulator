//! GBA system bus: 28-bit memory map, wait states, IO registers, DMA,
//! timers, interrupts, keypad and the event loop glue.

use crate::apu::{Apu, CYCLES_PER_FRAME_STEP, CYCLES_PER_SAMPLE};
use crate::cart::Cartridge;
use crate::ppu::Ppu;
use crate::sio::JoyBus;
use cpu_arm::{Access, Arm, Bus, SwiOutcome};
use kd_core::{EventKind, GuestClock, SaveState, Scheduler, StateReader, StateResult, StateWriter};

pub const CYCLES_PER_LINE: u64 = 1232;
pub const HDRAW_CYCLES: u64 = 960;
pub const LINES: u16 = 228;
pub const CYCLES_PER_FRAME: u64 = CYCLES_PER_LINE * LINES as u64;
pub const CPU_HZ: u64 = 16_777_216;

pub const IRQ_VBLANK: u16 = 1 << 0;
pub const IRQ_HBLANK: u16 = 1 << 1;
pub const IRQ_VCOUNT: u16 = 1 << 2;
pub const IRQ_TIMER0: u16 = 1 << 3;
pub const IRQ_SERIAL: u16 = 1 << 7;
pub const IRQ_DMA0: u16 = 1 << 8;
pub const IRQ_KEYPAD: u16 = 1 << 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GbaEvent {
    HBlank,
    LineEnd,
    Timer(u8),
    ApuSample,
    ApuFrame,
}

impl EventKind for GbaEvent {
    const SLOTS: usize = 8;
    fn slot(self) -> usize {
        match self {
            GbaEvent::HBlank => 0,
            GbaEvent::LineEnd => 1,
            GbaEvent::Timer(n) => 2 + n as usize,
            GbaEvent::ApuSample => 6,
            GbaEvent::ApuFrame => 7,
        }
    }
}

impl GbaEvent {
    fn encode(self) -> u32 {
        self.slot() as u32
    }
    fn decode(v: u32) -> Option<Self> {
        Some(match v {
            0 => GbaEvent::HBlank,
            1 => GbaEvent::LineEnd,
            2..=5 => GbaEvent::Timer((v - 2) as u8),
            6 => GbaEvent::ApuSample,
            7 => GbaEvent::ApuFrame,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Timer {
    pub reload: u16,
    pub ctrl: u16,
    pub counter: u16,
    pub started_at: u64,
}

impl Timer {
    fn shift(&self) -> u32 {
        [0, 6, 8, 10][(self.ctrl & 3) as usize]
    }
    fn running(&self) -> bool {
        self.ctrl & 0x80 != 0
    }
    fn counting_cycles(&self, n: usize) -> bool {
        self.running() && !(n > 0 && self.ctrl & 4 != 0)
    }
    fn value(&self, n: usize, now: u64) -> u16 {
        if self.counting_cycles(n) {
            let ticks = (now.saturating_sub(self.started_at)) >> self.shift();
            (self.counter as u64 + ticks).min(0xFFFF) as u16
        } else {
            self.counter
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DmaChannel {
    pub sad: u32,
    pub dad: u32,
    pub cnt_l: u16,
    pub cnt_h: u16,
    src: u32,
    dst: u32,
    count: u32,
}

pub struct GbaBus {
    pub sched: Scheduler<GbaEvent>,
    next_event: u64,
    pub bios: Box<[u8]>,
    pub ewram: Box<[u8]>,
    pub iwram: Box<[u8]>,
    pub ppu: Ppu,
    pub apu: Apu,
    pub cart: Cartridge,
    pub sio: JoyBus,
    pub timers: [Timer; 4],
    pub dma: [DmaChannel; 4],
    pub ie: u16,
    pub if_: u16,
    pub ime: bool,
    pub waitcnt: u16,
    pub keyinput: u16,
    pub keycnt: u16,
    pub postflg: u8,
    pub halt_request: bool,
    /// An IntrWait/VBlankIntrWait is in progress (the SWI is being repeated).
    pub intr_wait_active: bool,
    pub frame_done: bool,
    pub clock: GuestClock,
    /// Last value written to each IO halfword (for byte writes).
    io_shadow: Box<[u16]>,
    cycles16: [[u32; 2]; 16],
    cycles32: [[u32; 2]; 16],
    in_bios: bool,
    pub(crate) bios_latch: u32,
    open_bus: u32,
    dma_len_hint: Option<u32>,
    dma_running: bool,
}

#[inline]
fn rd16(m: &[u8], a: usize) -> u16 {
    u16::from_le_bytes([m[a], m[a + 1]])
}
#[inline]
fn rd32(m: &[u8], a: usize) -> u32 {
    u32::from_le_bytes([m[a], m[a + 1], m[a + 2], m[a + 3]])
}
#[inline]
fn wr16(m: &mut [u8], a: usize, v: u16) {
    m[a..a + 2].copy_from_slice(&v.to_le_bytes());
}
#[inline]
fn wr32(m: &mut [u8], a: usize, v: u32) {
    m[a..a + 4].copy_from_slice(&v.to_le_bytes());
}

#[inline]
fn vram_offset(addr: u32) -> usize {
    let a = addr as usize & 0x1FFFF;
    if a >= 0x18000 {
        a - 0x8000
    } else {
        a
    }
}

impl GbaBus {
    pub fn new(cart: Cartridge, bios: Vec<u8>) -> Self {
        let mut b = Self {
            sched: Scheduler::new(),
            next_event: 0,
            bios: {
                let mut v = bios;
                v.resize(0x4000, 0);
                v.into_boxed_slice()
            },
            ewram: vec![0; 0x40000].into_boxed_slice(),
            iwram: vec![0; 0x8000].into_boxed_slice(),
            ppu: Ppu::new(),
            apu: Apu::new(),
            cart,
            sio: JoyBus::default(),
            timers: [Timer::default(); 4],
            dma: [DmaChannel::default(); 4],
            ie: 0,
            if_: 0,
            ime: false,
            waitcnt: 0,
            keyinput: 0x3FF,
            keycnt: 0,
            postflg: 0,
            halt_request: false,
            intr_wait_active: false,
            frame_done: false,
            clock: GuestClock::host(),
            io_shadow: vec![0; 0x200].into_boxed_slice(),
            cycles16: [[1; 2]; 16],
            cycles32: [[1; 2]; 16],
            in_bios: false,
            bios_latch: 0xE129_F000,
            open_bus: 0,
            dma_len_hint: None,
            dma_running: false,
        };
        b.update_waitstates();
        b.schedule(HDRAW_CYCLES, GbaEvent::HBlank);
        b.schedule(CYCLES_PER_SAMPLE, GbaEvent::ApuSample);
        b.schedule(CYCLES_PER_FRAME_STEP, GbaEvent::ApuFrame);
        b
    }

    #[inline]
    pub fn now(&self) -> u64 {
        self.sched.now()
    }

    #[inline]
    fn tick(&mut self, n: u32) {
        self.sched.advance(n as u64);
    }

    pub fn schedule(&mut self, at: u64, ev: GbaEvent) {
        self.sched.schedule_at(at, ev);
        self.next_event = self.next_event.min(at);
    }

    pub fn irq_pending(&self) -> bool {
        self.ime && self.ie & self.if_ & 0x3FFF != 0
    }

    pub fn wake_condition(&self) -> bool {
        self.ie & self.if_ & 0x3FFF != 0
    }

    pub fn request_irq(&mut self, bits: u16) {
        self.if_ |= bits;
    }

    fn update_waitstates(&mut self) {
        let w = self.waitcnt;
        let n = [4u32, 3, 2, 8];
        let sram = n[(w & 3) as usize];
        let ws = [
            (n[((w >> 2) & 3) as usize], [2u32, 1][((w >> 4) & 1) as usize]),
            (n[((w >> 5) & 3) as usize], [4u32, 1][((w >> 7) & 1) as usize]),
            (n[((w >> 8) & 3) as usize], [8u32, 1][((w >> 10) & 1) as usize]),
        ];
        let mut c16 = [[1u32; 2]; 16];
        let mut c32 = [[1u32; 2]; 16];
        c16[2] = [3, 3];
        c32[2] = [6, 6];
        c32[5] = [2, 2];
        c32[6] = [2, 2];
        for (i, &(wn, wsq)) in ws.iter().enumerate() {
            for r in [8 + i * 2, 9 + i * 2] {
                c16[r] = [1 + wn, 1 + wsq];
                c32[r] = [(1 + wn) + (1 + wsq), 2 * (1 + wsq)];
            }
        }
        c16[0xE] = [1 + sram; 2];
        c16[0xF] = [1 + sram; 2];
        c32[0xE] = [1 + sram; 2];
        c32[0xF] = [1 + sram; 2];
        self.cycles16 = c16;
        self.cycles32 = c32;
    }

    #[inline]
    fn wait16(&self, addr: u32, access: Access) -> u32 {
        self.cycles16[((addr >> 24) & 0xF) as usize][(access == Access::Seq) as usize]
    }

    #[inline]
    fn wait32(&self, addr: u32, access: Access) -> u32 {
        self.cycles32[((addr >> 24) & 0xF) as usize][(access == Access::Seq) as usize]
    }

    fn prefetch_on(&self) -> bool {
        self.waitcnt & 0x4000 != 0
    }

    // ---------------------------------------------------------------- memory

    fn read8_raw(&mut self, addr: u32) -> u8 {
        match addr >> 24 {
            0x00 => {
                if addr < 0x4000 {
                    if self.in_bios {
                        self.bios[addr as usize]
                    } else {
                        (self.bios_latch >> ((addr & 3) * 8)) as u8
                    }
                } else {
                    (self.open_bus >> ((addr & 3) * 8)) as u8
                }
            }
            0x02 => self.ewram[addr as usize & 0x3FFFF],
            0x03 => self.iwram[addr as usize & 0x7FFF],
            0x04 => {
                let v = self.io_read16(addr & 0x3FE);
                (v >> ((addr & 1) * 8)) as u8
            }
            0x05 => self.ppu.palette[addr as usize & 0x3FF],
            0x06 => self.ppu.vram[vram_offset(addr)],
            0x07 => self.ppu.oam[addr as usize & 0x3FF],
            0x08..=0x0D => {
                if self.cart.is_eeprom_addr(addr) {
                    return self.cart.backup.eeprom_read_bit() as u8;
                }
                let v = self.rom16(addr & !1);
                (v >> ((addr & 1) * 8)) as u8
            }
            0x0E | 0x0F => {
                if self.cart.tilt.enabled && (addr & 0xFFFF) >= 0x8000 {
                    self.cart.tilt.read(addr)
                } else {
                    self.cart.backup.read8(addr)
                }
            }
            _ => (self.open_bus >> ((addr & 3) * 8)) as u8,
        }
    }

    fn rom16(&mut self, addr: u32) -> u16 {
        if self.cart.gpio.present() && (0x0800_00C4..=0x0800_00C9).contains(&addr) {
            if let Some(v) = self.cart.gpio.read(addr & 0xFF) {
                return v;
            }
        }
        let off = addr as usize & 0x01FF_FFFF;
        if off >= self.cart.rom.len() {
            // Out-of-range ROM reads return the address bus value.
            return (addr >> 1) as u16;
        }
        self.cart.rom_read16(addr)
    }

    fn read16_raw(&mut self, addr: u32) -> u16 {
        match addr >> 24 {
            0x02 => rd16(&self.ewram, addr as usize & 0x3FFFE),
            0x03 => rd16(&self.iwram, addr as usize & 0x7FFE),
            0x04 => self.io_read16(addr & 0x3FE),
            0x05 => rd16(&self.ppu.palette, addr as usize & 0x3FE),
            0x06 => rd16(&self.ppu.vram, vram_offset(addr & !1)),
            0x07 => rd16(&self.ppu.oam, addr as usize & 0x3FE),
            0x08..=0x0D => {
                if self.cart.is_eeprom_addr(addr) {
                    return self.cart.backup.eeprom_read_bit();
                }
                self.rom16(addr)
            }
            0x0E | 0x0F => {
                let b = self.read8_raw(addr) as u16;
                b * 0x0101
            }
            _ => {
                let lo = self.read8_raw(addr) as u16;
                let hi = self.read8_raw(addr | 1) as u16;
                lo | hi << 8
            }
        }
    }

    fn read32_raw(&mut self, addr: u32) -> u32 {
        match addr >> 24 {
            0x02 => rd32(&self.ewram, addr as usize & 0x3FFFC),
            0x03 => rd32(&self.iwram, addr as usize & 0x7FFC),
            0x05 => rd32(&self.ppu.palette, addr as usize & 0x3FC),
            0x06 => rd32(&self.ppu.vram, vram_offset(addr & !3)),
            0x07 => rd32(&self.ppu.oam, addr as usize & 0x3FC),
            0x0E | 0x0F => (self.read8_raw(addr) as u32) * 0x0101_0101,
            _ => self.read16_raw(addr) as u32 | (self.read16_raw(addr + 2) as u32) << 16,
        }
    }

    fn write8_raw(&mut self, addr: u32, v: u8) {
        match addr >> 24 {
            0x02 => self.ewram[addr as usize & 0x3FFFF] = v,
            0x03 => self.iwram[addr as usize & 0x7FFF] = v,
            0x04 => self.io_write8(addr & 0x3FF, v),
            // 8-bit writes to palette and BG VRAM write the byte to both halves.
            0x05 => wr16(&mut self.ppu.palette, addr as usize & 0x3FE, v as u16 * 0x0101),
            0x06 => {
                let bitmap = self.ppu.dispcnt() & 7 >= 3;
                let limit = if bitmap { 0x14000 } else { 0x10000 };
                let off = vram_offset(addr & !1);
                if off < limit {
                    wr16(&mut self.ppu.vram, off, v as u16 * 0x0101);
                }
            }
            0x07 => {} // OAM ignores byte writes
            0x08..=0x0D => self.rom_write16(addr & !1, v as u16),
            0x0E | 0x0F => {
                if self.cart.tilt.enabled && (addr & 0xFFFF) >= 0x8000 {
                    self.cart.tilt.write(addr, v);
                } else {
                    self.cart.backup.write8(addr, v);
                }
            }
            _ => {}
        }
    }

    fn rom_write16(&mut self, addr: u32, v: u16) {
        if self.cart.is_eeprom_addr(addr) {
            let hint = self.dma_len_hint;
            self.cart.backup.eeprom_write_bit(v, hint);
        } else if self.cart.gpio.present() && (0x0800_00C4..=0x0800_00C9).contains(&addr) {
            let clock = self.clock;
            self.cart.gpio.write(addr & 0xFF, v, &clock);
        }
    }

    fn write16_raw(&mut self, addr: u32, v: u16) {
        match addr >> 24 {
            0x02 => wr16(&mut self.ewram, addr as usize & 0x3FFFE, v),
            0x03 => wr16(&mut self.iwram, addr as usize & 0x7FFE, v),
            0x04 => self.io_write16(addr & 0x3FE, v),
            0x05 => wr16(&mut self.ppu.palette, addr as usize & 0x3FE, v),
            0x06 => wr16(&mut self.ppu.vram, vram_offset(addr & !1), v),
            0x07 => wr16(&mut self.ppu.oam, addr as usize & 0x3FE, v),
            0x08..=0x0D => self.rom_write16(addr, v),
            0x0E | 0x0F => self.write8_raw(addr, (v >> ((addr & 1) * 8)) as u8),
            _ => {}
        }
    }

    fn write32_raw(&mut self, addr: u32, v: u32) {
        match addr >> 24 {
            0x02 => wr32(&mut self.ewram, addr as usize & 0x3FFFC, v),
            0x03 => wr32(&mut self.iwram, addr as usize & 0x7FFC, v),
            0x05 => wr32(&mut self.ppu.palette, addr as usize & 0x3FC, v),
            0x06 => wr32(&mut self.ppu.vram, vram_offset(addr & !3), v),
            0x07 => wr32(&mut self.ppu.oam, addr as usize & 0x3FC, v),
            0x0E | 0x0F => self.write8_raw(addr, (v >> ((addr & 3) * 8)) as u8),
            _ => {
                self.write16_raw(addr, v as u16);
                self.write16_raw(addr + 2, (v >> 16) as u16);
            }
        }
    }

    // -------------------------------------------------------------------- IO

    pub fn io_read16(&mut self, off: u32) -> u16 {
        match off {
            0x000..=0x05F => self.ppu.read16(off),
            0x060..=0x0AF => self.apu.read16(off),
            0x0B0..=0x0DF => {
                let n = ((off - 0xB0) / 12) as usize;
                match (off - 0xB0) % 12 {
                    10 => self.dma[n].cnt_h,
                    _ => 0,
                }
            }
            0x100..=0x10F => {
                let n = ((off - 0x100) / 4) as usize;
                if off & 2 == 0 {
                    self.timers[n].value(n, self.now())
                } else {
                    self.timers[n].ctrl
                }
            }
            0x130 => self.keyinput,
            0x132 => self.keycnt,
            0x120..=0x15F => self.sio.read16(off),
            0x200 => self.ie,
            0x202 => self.if_,
            0x204 => self.waitcnt,
            0x208 => self.ime as u16,
            0x300 => self.postflg as u16,
            _ => 0,
        }
    }

    pub fn io_write16(&mut self, off: u32, v: u16) {
        self.io_shadow[(off >> 1) as usize & 0x1FF] = v;
        match off {
            0x000..=0x05F => self.ppu.write16(off, v),
            0x0A0..=0x0A7 => {
                let n = ((off - 0xA0) / 4) as usize;
                self.apu.fifo_write(n, &v.to_le_bytes());
            }
            0x060..=0x09F => self.apu.write16(off, v),
            0x0B0..=0x0DF => self.dma_write(off, v),
            0x100..=0x10F => self.timer_write(off, v),
            0x132 => {
                self.keycnt = v;
                self.check_keypad_irq();
            }
            0x120..=0x15F => self.sio.write16(off, v),
            0x200 => self.ie = v,
            0x202 => self.if_ &= !v,
            0x204 => {
                self.waitcnt = v & 0x5FFF;
                self.update_waitstates();
            }
            0x208 => self.ime = v & 1 != 0,
            0x300 => {
                self.postflg = v as u8 & 1;
                self.halt_request = true;
            }
            _ => {}
        }
    }

    fn io_write8(&mut self, off: u32, v: u8) {
        match off {
            0x0A0..=0x0A7 => self.apu.fifo_write(((off - 0xA0) / 4) as usize, &[v]),
            0x202 => self.if_ &= !(v as u16),
            0x203 => self.if_ &= !((v as u16) << 8),
            0x300 => self.postflg = v & 1,
            0x301 => self.halt_request = true,
            _ => {
                let idx = (off >> 1) as usize & 0x1FF;
                let cur = self.io_shadow[idx];
                let merged = if off & 1 == 0 { (cur & 0xFF00) | v as u16 } else { (cur & 0x00FF) | (v as u16) << 8 };
                self.io_write16(off & !1, merged);
            }
        }
    }

    pub fn set_keys(&mut self, pressed_mask: u16) {
        self.keyinput = !pressed_mask & 0x3FF;
        self.check_keypad_irq();
    }

    fn check_keypad_irq(&mut self) {
        if self.keycnt & 0x4000 == 0 {
            return;
        }
        let want = self.keycnt & 0x3FF;
        let pressed = !self.keyinput & 0x3FF;
        let hit = if self.keycnt & 0x8000 != 0 { pressed & want == want && want != 0 } else { pressed & want != 0 };
        if hit {
            self.request_irq(IRQ_KEYPAD);
        }
    }

    // ---------------------------------------------------------------- timers

    fn timer_write(&mut self, off: u32, v: u16) {
        let n = ((off - 0x100) / 4) as usize;
        if off & 2 == 0 {
            self.timers[n].reload = v;
            return;
        }
        let now = self.now();
        let t = &mut self.timers[n];
        let was_running = t.running();
        t.counter = t.value(n, now);
        t.started_at = now;
        t.ctrl = v & 0xC7;
        if !was_running && t.running() {
            t.counter = t.reload;
            // The timer starts counting two cycles after the write.
            t.started_at = now + 2;
        }
        self.reschedule_timer(n);
    }

    fn reschedule_timer(&mut self, n: usize) {
        let t = self.timers[n];
        if t.counting_cycles(n) {
            let at = t.started_at + ((0x1_0000 - t.counter as u64) << t.shift());
            self.schedule(at, GbaEvent::Timer(n as u8));
        } else {
            self.sched.cancel(GbaEvent::Timer(n as u8));
        }
    }

    fn timer_overflow(&mut self, n: usize, at: u64) {
        let t = &mut self.timers[n];
        t.counter = t.reload;
        t.started_at = at;
        if t.ctrl & 0x40 != 0 {
            self.if_ |= IRQ_TIMER0 << n;
        }
        if n < 2 {
            let refill = self.apu.timer_overflow(n);
            for fifo in 0..2 {
                if refill & (1 << fifo) != 0 {
                    self.fifo_dma(fifo);
                }
            }
        }
        if n < 3 {
            let next = &mut self.timers[n + 1];
            if next.running() && next.ctrl & 4 != 0 {
                if next.counter == 0xFFFF {
                    self.timer_overflow(n + 1, at);
                } else {
                    next.counter += 1;
                }
            }
        }
        self.reschedule_timer(n);
    }

    // ------------------------------------------------------------------- DMA

    fn dma_write(&mut self, off: u32, v: u16) {
        let n = ((off - 0xB0) / 12) as usize;
        let d = &mut self.dma[n];
        match (off - 0xB0) % 12 {
            0 => d.sad = (d.sad & 0xFFFF_0000) | v as u32,
            2 => d.sad = (d.sad & 0xFFFF) | (v as u32) << 16,
            4 => d.dad = (d.dad & 0xFFFF_0000) | v as u32,
            6 => d.dad = (d.dad & 0xFFFF) | (v as u32) << 16,
            8 => d.cnt_l = v,
            _ => {
                let was = d.cnt_h & 0x8000 != 0;
                d.cnt_h = v & if n == 3 { 0xFFE0 } else { 0xF7E0 };
                if !was && v & 0x8000 != 0 {
                    let src_mask = if n == 0 { 0x07FF_FFFF } else { 0x0FFF_FFFF };
                    let dst_mask = if n == 3 { 0x0FFF_FFFF } else { 0x07FF_FFFF };
                    d.src = d.sad & src_mask;
                    d.dst = d.dad & dst_mask;
                    d.count = Self::dma_count(n, d.cnt_l);
                    if (v >> 12) & 3 == 0 {
                        self.run_dma(n);
                    }
                }
            }
        }
    }

    fn dma_count(n: usize, cnt_l: u16) -> u32 {
        if cnt_l == 0 {
            if n == 3 {
                0x10000
            } else {
                0x4000
            }
        } else if n == 3 {
            cnt_l as u32
        } else {
            (cnt_l & 0x3FFF) as u32
        }
    }

    fn run_dma(&mut self, n: usize) {
        if self.dma_running {
            return;
        }
        self.dma_running = true;
        let d = self.dma[n];
        let timing = (d.cnt_h >> 12) & 3;
        let fifo = timing == 3 && (n == 1 || n == 2);
        let word = fifo || d.cnt_h & 0x400 != 0;
        let count = if fifo { 4 } else { d.count };
        let step: u32 = if word { 4 } else { 2 };
        let src_ctl = (d.cnt_h >> 7) & 3;
        let dst_ctl = if fifo { 2 } else { (d.cnt_h >> 5) & 3 };
        let (mut src, mut dst) = (d.src, d.dst);
        self.dma_len_hint = Some(count);
        let mut access = Access::NonSeq;
        for _ in 0..count {
            if word {
                let v = self.read32(src & !3, access);
                self.write32(dst & !3, v, access);
            } else {
                let v = self.read16(src & !1, access);
                self.write16(dst & !1, v, access);
            }
            access = Access::Seq;
            src = match src_ctl {
                1 => src.wrapping_sub(step),
                2 => src,
                _ => src.wrapping_add(step),
            };
            dst = match dst_ctl {
                1 => dst.wrapping_sub(step),
                2 => dst,
                _ => dst.wrapping_add(step),
            };
        }
        self.tick(2);
        self.dma_len_hint = None;
        let ch = &mut self.dma[n];
        ch.src = src;
        ch.dst = dst;
        if ch.cnt_h & 0x4000 != 0 {
            self.if_ |= IRQ_DMA0 << n;
        }
        let ch = &mut self.dma[n];
        if ch.cnt_h & 0x200 != 0 && timing != 0 {
            ch.count = Self::dma_count(n, ch.cnt_l);
            if dst_ctl == 3 {
                let dst_mask = if n == 3 { 0x0FFF_FFFF } else { 0x07FF_FFFF };
                ch.dst = ch.dad & dst_mask;
            }
        } else {
            ch.cnt_h &= !0x8000;
        }
        self.dma_running = false;
    }

    fn trigger_dma(&mut self, timing: u16) {
        for n in 0..4 {
            let c = self.dma[n].cnt_h;
            if c & 0x8000 != 0 && (c >> 12) & 3 == timing {
                self.run_dma(n);
            }
        }
    }

    fn fifo_dma(&mut self, fifo: usize) {
        let addr = 0x0400_00A0 + fifo as u32 * 4;
        for n in 1..=2 {
            let d = self.dma[n];
            if d.cnt_h & 0x8000 != 0 && (d.cnt_h >> 12) & 3 == 3 && d.dad & 0x0FFF_FFFF == addr {
                self.run_dma(n);
            }
        }
    }

    // ---------------------------------------------------------------- events

    /// Dispatch every event that is due.
    pub fn process_events(&mut self) {
        if self.now() < self.next_event {
            return;
        }
        while let Some((at, ev)) = self.sched.pop_due() {
            self.handle_event(at, ev);
        }
        self.next_event = self.sched.next_deadline().unwrap_or(u64::MAX);
        if self.sio.irq {
            self.sio.irq = false;
            self.if_ |= IRQ_SERIAL;
        }
    }

    /// Jump the clock to the next event (CPU halted).
    pub fn skip_to_next_event(&mut self) {
        if let Some(t) = self.sched.next_deadline() {
            self.sched.advance_to(t);
        }
    }

    fn handle_event(&mut self, at: u64, ev: GbaEvent) {
        match ev {
            GbaEvent::HBlank => {
                self.ppu.in_hblank = true;
                let line = self.ppu.vcount;
                if line < 160 {
                    self.ppu.render_line(line as usize);
                    self.trigger_dma(2);
                }
                if self.ppu.dispstat & 0x10 != 0 {
                    self.if_ |= IRQ_HBLANK;
                }
                self.schedule(at + (CYCLES_PER_LINE - HDRAW_CYCLES), GbaEvent::LineEnd);
            }
            GbaEvent::LineEnd => {
                self.ppu.in_hblank = false;
                self.ppu.vcount = (self.ppu.vcount + 1) % LINES;
                match self.ppu.vcount {
                    160 => {
                        self.ppu.in_vblank = true;
                        self.ppu.on_vblank();
                        if self.ppu.dispstat & 0x8 != 0 {
                            self.if_ |= IRQ_VBLANK;
                        }
                        self.trigger_dma(1);
                        self.frame_done = true;
                    }
                    227 => self.ppu.in_vblank = false,
                    _ => {}
                }
                if self.ppu.vcount == self.ppu.dispstat >> 8 && self.ppu.dispstat & 0x20 != 0 {
                    self.if_ |= IRQ_VCOUNT;
                }
                self.schedule(at + HDRAW_CYCLES, GbaEvent::HBlank);
            }
            GbaEvent::Timer(n) => self.timer_overflow(n as usize, at),
            GbaEvent::ApuSample => {
                self.apu.sample();
                self.schedule(at + CYCLES_PER_SAMPLE, GbaEvent::ApuSample);
            }
            GbaEvent::ApuFrame => {
                self.apu.frame_sequencer();
                self.schedule(at + CYCLES_PER_FRAME_STEP, GbaEvent::ApuFrame);
            }
        }
    }

    /// Restore state previously saved with [`SaveState`]; the scheduler and
    /// event cache are rebuilt.
    fn restore_events(&mut self, now: u64, events: &[(u64, GbaEvent)]) {
        self.sched.restore(now, events);
        self.next_event = self.sched.next_deadline().unwrap_or(u64::MAX);
    }
}

impl Bus for GbaBus {
    fn read8(&mut self, addr: u32, access: Access) -> u8 {
        self.tick(self.wait16(addr, access));
        self.read8_raw(addr)
    }
    fn read16(&mut self, addr: u32, access: Access) -> u16 {
        self.tick(self.wait16(addr, access));
        self.read16_raw(addr)
    }
    fn read32(&mut self, addr: u32, access: Access) -> u32 {
        self.tick(self.wait32(addr, access));
        self.read32_raw(addr)
    }
    fn write8(&mut self, addr: u32, v: u8, access: Access) {
        self.tick(self.wait16(addr, access));
        self.write8_raw(addr, v);
    }
    fn write16(&mut self, addr: u32, v: u16, access: Access) {
        // Only the 8-bit save chip sees the low address bit.
        let addr = if addr >> 25 == 0x07 { addr } else { addr & !1 };
        self.tick(self.wait16(addr, access));
        self.write16_raw(addr, v);
    }
    fn write32(&mut self, addr: u32, v: u32, access: Access) {
        let addr = if addr >> 25 == 0x07 { addr } else { addr & !3 };
        self.tick(self.wait32(addr, access));
        self.write32_raw(addr, v);
    }

    fn fetch16(&mut self, addr: u32, access: Access) -> u16 {
        let rom = (0x08..=0x0D).contains(&(addr >> 24));
        let cycles = if rom && access == Access::Seq && self.prefetch_on() { 1 } else { self.wait16(addr, access) };
        self.tick(cycles);
        self.in_bios = addr < 0x4000;
        let v = if self.in_bios { rd16(&self.bios, addr as usize) } else { self.read16_raw(addr) };
        if self.in_bios {
            self.bios_latch = v as u32 * 0x1_0001;
        }
        self.open_bus = v as u32 * 0x1_0001;
        v
    }

    fn fetch32(&mut self, addr: u32, access: Access) -> u32 {
        let rom = (0x08..=0x0D).contains(&(addr >> 24));
        let cycles = if rom && access == Access::Seq && self.prefetch_on() { 2 } else { self.wait32(addr, access) };
        self.tick(cycles);
        self.in_bios = addr < 0x4000;
        let v = if self.in_bios { rd32(&self.bios, addr as usize) } else { self.read32_raw(addr) };
        if self.in_bios {
            self.bios_latch = v;
        }
        self.open_bus = v;
        v
    }

    fn idle(&mut self, cycles: u32) {
        self.tick(cycles);
    }

    fn hle_swi(&mut self, cpu: &mut Arm, comment: u32) -> SwiOutcome {
        let n = if cpu.thumb() { comment & 0xFF } else { (comment >> 16) & 0xFF };
        let out = crate::bios::swi(self, cpu, n as u8);
        self.bios_latch = 0xE3A0_2004;
        out
    }
}

impl SaveState for GbaBus {
    fn save_state(&self, w: &mut StateWriter) {
        w.u64(self.now());
        let events = self.sched.snapshot();
        w.u32(events.len() as u32);
        for (at, ev) in events {
            w.u64(at);
            w.u32(ev.encode());
        }
        w.bytes(&self.ewram);
        w.bytes(&self.iwram);
        w.section(*b"PPU0", &self.ppu);
        w.section(*b"APU0", &self.apu);
        w.section(*b"CART", &self.cart);
        w.section(*b"SIO0", &self.sio);
        for t in &self.timers {
            w.u16(t.reload);
            w.u16(t.ctrl);
            w.u16(t.counter);
            w.u64(t.started_at);
        }
        for d in &self.dma {
            w.u32(d.sad);
            w.u32(d.dad);
            w.u16(d.cnt_l);
            w.u16(d.cnt_h);
            w.u32(d.src);
            w.u32(d.dst);
            w.u32(d.count);
        }
        w.u16(self.ie);
        w.u16(self.if_);
        w.bool(self.ime);
        w.u16(self.waitcnt);
        w.u16(self.keycnt);
        w.u8(self.postflg);
        w.u16s(&self.io_shadow);
        w.u32(self.bios_latch);
        w.bool(self.intr_wait_active);
        w.i64(self.clock.offset_secs);
    }

    fn load_state(&mut self, r: &mut StateReader<'_>) -> StateResult<()> {
        let now = r.u64()?;
        let n = r.u32()? as usize;
        let mut events = Vec::with_capacity(n);
        for _ in 0..n {
            let at = r.u64()?;
            let ev = GbaEvent::decode(r.u32()?).ok_or(kd_core::StateError::Invalid("bad GBA event"))?;
            events.push((at, ev));
        }
        r.bytes_into(&mut self.ewram)?;
        r.bytes_into(&mut self.iwram)?;
        r.section(*b"PPU0", &mut self.ppu)?;
        r.section(*b"APU0", &mut self.apu)?;
        r.section(*b"CART", &mut self.cart)?;
        r.section(*b"SIO0", &mut self.sio)?;
        for t in &mut self.timers {
            t.reload = r.u16()?;
            t.ctrl = r.u16()?;
            t.counter = r.u16()?;
            t.started_at = r.u64()?;
        }
        for d in &mut self.dma {
            d.sad = r.u32()?;
            d.dad = r.u32()?;
            d.cnt_l = r.u16()?;
            d.cnt_h = r.u16()?;
            d.src = r.u32()?;
            d.dst = r.u32()?;
            d.count = r.u32()?;
        }
        self.ie = r.u16()?;
        self.if_ = r.u16()?;
        self.ime = r.bool()?;
        self.waitcnt = r.u16()?;
        self.keycnt = r.u16()?;
        self.postflg = r.u8()?;
        r.u16s_into(&mut self.io_shadow)?;
        self.bios_latch = r.u32()?;
        self.intr_wait_active = r.bool()?;
        self.clock.offset_secs = r.i64()?;
        self.update_waitstates();
        self.restore_events(now, &events);
        Ok(())
    }
}
