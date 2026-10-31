//! DS system bus: shared memories, the ARM9 and ARM7 address spaces, IO
//! registers, DMA, timers, the hardware divider / square root unit,
//! interrupts and scanline timing.

use crate::cart::GameCard;
use crate::engine2d::{Engine2d, EngineId, WIDTH};
use crate::gpu3d::Gpu3d;
use crate::ipc::{Ipc, IpcIrqs};
use crate::rtc::Rtc;
use crate::spi::Spi;
use crate::vram::{View, Vram};
use cpu_arm::cp15::Cp15;
use cpu_arm::{Access, Arm, Bus, SwiOutcome};
use kd_core::{EventKind, GuestClock, SaveState, Scheduler, StateReader, StateResult, StateWriter};

/// Master clock = ARM9 cycles (≈ 67 MHz); the ARM7 and timers run at half.
pub const LINE_CYCLES: u64 = 4260;
pub const HBLANK_START: u64 = 3072;
pub const LINES: u16 = 263;
pub const FRAME_CYCLES: u64 = LINE_CYCLES * LINES as u64;

pub const IRQ_VBLANK: u32 = 1 << 0;
pub const IRQ_HBLANK: u32 = 1 << 1;
pub const IRQ_VCOUNT: u32 = 1 << 2;
pub const IRQ_TIMER0: u32 = 1 << 3;
pub const IRQ_DMA0: u32 = 1 << 8;
pub const IRQ_KEYPAD: u32 = 1 << 12;
pub const IRQ_CARD: u32 = 1 << 19;
pub const IRQ_GXFIFO: u32 = 1 << 21;

pub const ARM9: usize = 0;
pub const ARM7: usize = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NdsEvent {
    HBlank,
    LineEnd,
    Timer(u8, u8),
}

impl EventKind for NdsEvent {
    const SLOTS: usize = 10;
    fn slot(self) -> usize {
        match self {
            NdsEvent::HBlank => 0,
            NdsEvent::LineEnd => 1,
            NdsEvent::Timer(cpu, n) => 2 + cpu as usize * 4 + n as usize,
        }
    }
}

impl NdsEvent {
    fn decode(v: u32) -> Option<Self> {
        Some(match v {
            0 => NdsEvent::HBlank,
            1 => NdsEvent::LineEnd,
            2..=9 => NdsEvent::Timer(((v - 2) / 4) as u8, ((v - 2) % 4) as u8),
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
        [0, 6, 8, 10][(self.ctrl & 3) as usize] + 1
    }
    fn running(&self) -> bool {
        self.ctrl & 0x80 != 0
    }
    fn counting(&self, n: usize) -> bool {
        self.running() && !(n > 0 && self.ctrl & 4 != 0)
    }
    fn value(&self, n: usize, now: u64) -> u16 {
        if self.counting(n) {
            (self.counter as u64 + (now.saturating_sub(self.started_at) >> self.shift())).min(0xFFFF) as u16
        } else {
            self.counter
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Dma {
    pub sad: u32,
    pub dad: u32,
    pub cnt: u32,
    pub fill: u32,
    src: u32,
    dst: u32,
    count: u32,
}

/// Per-CPU interrupt, timer and DMA state.
pub struct CpuIo {
    pub ie: u32,
    pub if_: u32,
    pub ime: bool,
    pub dispstat: u16,
    pub keycnt: u16,
    pub timers: [Timer; 4],
    pub dma: [Dma; 4],
    pub halt_request: bool,
    pub intr_wait_active: bool,
    pub postflg: u8,
    shadow: Box<[u16]>,
}

impl Default for CpuIo {
    fn default() -> Self {
        Self {
            ie: 0,
            if_: 0,
            ime: false,
            dispstat: 0,
            keycnt: 0,
            timers: [Timer::default(); 4],
            dma: [Dma::default(); 4],
            halt_request: false,
            intr_wait_active: false,
            postflg: 1,
            shadow: vec![0; 0x400].into_boxed_slice(),
        }
    }
}

pub struct Shared {
    pub sched: Scheduler<NdsEvent>,
    /// Local time of each CPU in master cycles.
    pub time: [u64; 2],
    pub main_ram: Box<[u8]>,
    pub wram: Box<[u8]>,
    pub wram7: Box<[u8]>,
    pub wramcnt: u8,
    pub vram: Vram,
    pub palette: Box<[u8]>,
    pub oam: Box<[u8]>,
    pub itcm: Box<[u8]>,
    pub dtcm: Box<[u8]>,
    pub cp15: Cp15,
    pub bios9: Box<[u8]>,
    pub bios7: Box<[u8]>,
    pub io: [CpuIo; 2],
    pub ipc: Ipc,
    pub card: GameCard,
    pub spi: Spi,
    pub rtc: Rtc,
    pub engine_a: Engine2d,
    pub engine_b: Engine2d,
    pub gpu3d: Gpu3d,
    pub vcount: u16,
    pub in_hblank: bool,
    pub in_vblank: bool,
    pub powcnt1: u16,
    pub powcnt2: u16,
    pub exmemcnt: u16,
    pub keyinput: u16,
    pub extkeyin: u16,
    pub rcnt: u16,
    divcnt: u16,
    div_numer: u64,
    div_denom: u64,
    div_result: u64,
    divrem_result: u64,
    sqrtcnt: u16,
    sqrt_param: u64,
    sqrt_result: u32,
    sound_regs: Box<[u8]>,
    /// 256x384 RGBA8888: top screen above bottom screen.
    pub framebuffer: Vec<u32>,
    pub frame_done: bool,
    pub clock: GuestClock,
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

fn isqrt64(v: u64) -> u32 {
    if v == 0 {
        return 0;
    }
    let mut x = (v as f64).sqrt() as u64;
    while x * x > v {
        x -= 1;
    }
    while (x + 1) * (x + 1) <= v {
        x += 1;
    }
    x as u32
}

impl Shared {
    pub fn new(rom: Vec<u8>, bios9: Vec<u8>, bios7: Vec<u8>) -> Self {
        let pad = |mut v: Vec<u8>, n: usize| {
            v.resize(n, 0);
            v.into_boxed_slice()
        };
        let mut s = Self {
            sched: Scheduler::new(),
            time: [0; 2],
            main_ram: vec![0; 0x40_0000].into_boxed_slice(),
            wram: vec![0; 0x8000].into_boxed_slice(),
            wram7: vec![0; 0x10000].into_boxed_slice(),
            wramcnt: 3,
            vram: Vram::new(),
            palette: vec![0; 0x800].into_boxed_slice(),
            oam: vec![0; 0x800].into_boxed_slice(),
            itcm: vec![0; 0x8000].into_boxed_slice(),
            dtcm: vec![0; 0x4000].into_boxed_slice(),
            cp15: Cp15::new(),
            bios9: pad(bios9, 0x8000),
            bios7: pad(bios7, 0x4000),
            io: [CpuIo::default(), CpuIo::default()],
            ipc: Ipc::default(),
            card: GameCard::new(rom),
            spi: Spi::new(),
            rtc: Rtc::new(),
            engine_a: Engine2d::new(EngineId::A),
            engine_b: Engine2d::new(EngineId::B),
            gpu3d: Gpu3d::new(),
            vcount: 0,
            in_hblank: false,
            in_vblank: false,
            powcnt1: 0x820F,
            powcnt2: 1,
            exmemcnt: 0x6000,
            keyinput: 0x3FF,
            extkeyin: 0x7F,
            rcnt: 0x8000,
            divcnt: 0,
            div_numer: 0,
            div_denom: 0,
            div_result: 0,
            divrem_result: 0,
            sqrtcnt: 0,
            sqrt_param: 0,
            sqrt_result: 0,
            sound_regs: vec![0; 0x120].into_boxed_slice(),
            framebuffer: vec![0xFF00_0000; WIDTH * 384],
            frame_done: false,
            clock: GuestClock::host(),
        };
        s.sched.schedule_at(HBLANK_START, NdsEvent::HBlank);
        s
    }

    pub fn irq_line(&self, cpu: usize) -> bool {
        let io = &self.io[cpu];
        io.ime && io.ie & io.if_ != 0
    }

    pub fn wake_condition(&self, cpu: usize) -> bool {
        let io = &self.io[cpu];
        io.ie & io.if_ != 0
    }

    pub fn request_irq(&mut self, cpu: usize, bits: u32) {
        self.io[cpu].if_ |= bits;
    }

    fn apply_ipc(&mut self, irqs: IpcIrqs) {
        for (cpu, bits) in irqs {
            self.request_irq(cpu, bits);
        }
    }

    fn card_cpu(&self) -> usize {
        if self.exmemcnt & 0x800 != 0 {
            ARM7
        } else {
            ARM9
        }
    }

    // ------------------------------------------------------------ math unit

    fn update_div(&mut self) {
        let mode = self.divcnt & 3;
        let (num, den): (i64, i64) = match mode {
            0 => (self.div_numer as i32 as i64, self.div_denom as i32 as i64),
            1 | 3 => (self.div_numer as i64, self.div_denom as i32 as i64),
            _ => (self.div_numer as i64, self.div_denom as i64),
        };
        self.divcnt = (self.divcnt & !0x4000) | if self.div_denom == 0 { 0x4000 } else { 0 };
        if den == 0 {
            self.div_result = if num < 0 { 1 } else { u64::MAX };
            self.divrem_result = num as u64;
            if mode == 0 {
                self.div_result ^= 0xFFFF_FFFF_0000_0000;
            }
            return;
        }
        if num == i64::MIN && den == -1 {
            self.div_result = num as u64;
            self.divrem_result = 0;
            return;
        }
        self.div_result = (num / den) as u64;
        self.divrem_result = (num % den) as u64;
    }

    fn update_sqrt(&mut self) {
        let v = if self.sqrtcnt & 1 != 0 { self.sqrt_param } else { self.sqrt_param & 0xFFFF_FFFF };
        self.sqrt_result = isqrt64(v);
    }

    // ------------------------------------------------------------- timers

    fn timer_write(&mut self, cpu: usize, n: usize, high: bool, v: u16) {
        if !high {
            self.io[cpu].timers[n].reload = v;
            return;
        }
        let now = self.time[cpu];
        let t = &mut self.io[cpu].timers[n];
        let was = t.running();
        t.counter = t.value(n, now);
        t.started_at = now;
        t.ctrl = v & 0xC7;
        if !was && t.running() {
            t.counter = t.reload;
        }
        self.reschedule_timer(cpu, n);
    }

    fn reschedule_timer(&mut self, cpu: usize, n: usize) {
        let t = self.io[cpu].timers[n];
        let ev = NdsEvent::Timer(cpu as u8, n as u8);
        if t.counting(n) {
            self.sched.schedule_at(t.started_at + ((0x1_0000 - t.counter as u64) << t.shift()), ev);
        } else {
            self.sched.cancel(ev);
        }
    }

    fn timer_overflow(&mut self, cpu: usize, n: usize, at: u64) {
        let t = &mut self.io[cpu].timers[n];
        t.counter = t.reload;
        t.started_at = at;
        if t.ctrl & 0x40 != 0 {
            self.io[cpu].if_ |= IRQ_TIMER0 << n;
        }
        if n < 3 {
            let next = &mut self.io[cpu].timers[n + 1];
            if next.running() && next.ctrl & 4 != 0 {
                if next.counter == 0xFFFF {
                    self.timer_overflow(cpu, n + 1, at);
                } else {
                    next.counter += 1;
                }
            }
        }
        self.reschedule_timer(cpu, n);
    }

    // ------------------------------------------------------------- events

    pub fn handle_event(&mut self, at: u64, ev: NdsEvent) {
        match ev {
            NdsEvent::HBlank => {
                self.in_hblank = true;
                if self.vcount < 192 {
                    self.render_line(self.vcount as usize);
                    run_dma_trigger(self, ARM9, 2);
                }
                for cpu in 0..2 {
                    if self.io[cpu].dispstat & 0x10 != 0 {
                        self.io[cpu].if_ |= IRQ_HBLANK;
                    }
                }
                self.sched.schedule_at(at + (LINE_CYCLES - HBLANK_START), NdsEvent::LineEnd);
            }
            NdsEvent::LineEnd => {
                self.in_hblank = false;
                self.vcount = (self.vcount + 1) % LINES;
                if self.vcount == 192 {
                    self.in_vblank = true;
                    for cpu in 0..2 {
                        if self.io[cpu].dispstat & 0x08 != 0 {
                            self.io[cpu].if_ |= IRQ_VBLANK;
                        }
                    }
                    run_dma_trigger(self, ARM9, 1);
                    run_dma_trigger(self, ARM7, 1);
                    self.gpu3d.on_vblank(&self.vram);
                    self.engine_a.on_vblank();
                    self.engine_b.on_vblank();
                    self.frame_done = true;
                } else if self.vcount == LINES - 1 {
                    self.in_vblank = false;
                }
                for cpu in 0..2 {
                    let ds = self.io[cpu].dispstat;
                    let setting = (ds >> 8) | ((ds & 0x80) << 1);
                    if self.vcount == setting && ds & 0x20 != 0 {
                        self.io[cpu].if_ |= IRQ_VCOUNT;
                    }
                }
                if self.gpu3d.fifo_irq() {
                    self.io[ARM9].if_ |= IRQ_GXFIFO;
                }
                self.sched.schedule_at(at + HBLANK_START, NdsEvent::HBlank);
            }
            NdsEvent::Timer(cpu, n) => self.timer_overflow(cpu as usize, n as usize, at),
        }
    }

    fn render_line(&mut self, y: usize) {
        let mut line = [0u16; WIDTH];
        let mut out = [0u32; WIDTH];
        let top_is_a = self.powcnt1 & 0x8000 != 0;
        for engine in [EngineId::A, EngineId::B] {
            let enabled = match engine {
                EngineId::A => self.powcnt1 & 0x2 != 0,
                EngineId::B => self.powcnt1 & 0x200 != 0,
            };
            let (pal, oam) = match engine {
                EngineId::A => (&self.palette[..0x400], &self.oam[..0x400]),
                EngineId::B => (&self.palette[0x400..], &self.oam[0x400..]),
            };
            let e = match engine {
                EngineId::A => &mut self.engine_a,
                EngineId::B => &mut self.engine_b,
            };
            if enabled {
                let layer3d = (engine == EngineId::A).then(|| &self.gpu3d.framebuffer[y * WIDTH..(y + 1) * WIDTH]);
                e.render_line(y, &self.vram, pal, oam, layer3d, &mut line);
                e.finish_line(&line, &mut out);
            } else {
                out.fill(0xFF00_0000);
            }
            let screen = if (engine == EngineId::A) == top_is_a { 0 } else { 1 };
            let row = (screen * 192 + y) * WIDTH;
            self.framebuffer[row..row + WIDTH].copy_from_slice(&out);
        }
    }

    pub fn set_touch(&mut self, touch: Option<(u16, u16)>) {
        self.spi.touch = touch;
        if touch.is_some() {
            self.extkeyin &= !0x40;
        } else {
            self.extkeyin |= 0x40;
        }
    }

    pub fn set_keys(&mut self, pressed: u16, x: bool, y: bool, lid_closed: bool) {
        self.keyinput = !pressed & 0x3FF;
        self.extkeyin = (self.extkeyin & 0x40) | 0x3C | (!x as u16) | (!y as u16) << 1 | (lid_closed as u16) << 7;
        for cpu in 0..2 {
            let kc = self.io[cpu].keycnt;
            if kc & 0x4000 == 0 {
                continue;
            }
            let want = kc & 0x3FF;
            let hit = if kc & 0x8000 != 0 { pressed & want == want && want != 0 } else { pressed & want != 0 };
            if hit {
                self.io[cpu].if_ |= IRQ_KEYPAD;
            }
        }
    }
}

// ===================================================================== DMA

fn dma_count(cpu: usize, n: usize, cnt: u32) -> u32 {
    let (mask, max) = if cpu == ARM9 {
        (0x1F_FFFF, 0x20_0000)
    } else if n == 3 {
        (0xFFFF, 0x1_0000)
    } else {
        (0x3FFF, 0x4000)
    };
    let c = cnt & mask;
    if c == 0 {
        max
    } else {
        c
    }
}

fn dma_timing(cpu: usize, cnt: u32) -> u32 {
    if cpu == ARM9 {
        (cnt >> 27) & 7
    } else {
        (cnt >> 28) & 3
    }
}

fn dma_write_cnt(s: &mut Shared, cpu: usize, n: usize, v: u32) -> bool {
    let d = &mut s.io[cpu].dma[n];
    let was = d.cnt & 0x8000_0000 != 0;
    d.cnt = v;
    if !was && v & 0x8000_0000 != 0 {
        d.src = d.sad & 0x0FFF_FFFE;
        d.dst = d.dad & 0x0FFF_FFFE;
        d.count = dma_count(cpu, n, v);
        let t = dma_timing(cpu, v);
        return t == 0 || (cpu == ARM9 && t == 7);
    }
    false
}

/// Run every DMA on `cpu` whose start timing matches.
pub fn run_dma_trigger(s: &mut Shared, cpu: usize, timing: u32) {
    for n in 0..4 {
        let c = s.io[cpu].dma[n].cnt;
        if c & 0x8000_0000 != 0 && dma_timing(cpu, c) == timing {
            run_dma(s, cpu, n);
        }
    }
}

pub fn run_dma(s: &mut Shared, cpu: usize, n: usize) {
    let d = s.io[cpu].dma[n];
    let word = d.cnt & (1 << 26) != 0;
    let step: u32 = if word { 4 } else { 2 };
    let src_ctl = (d.cnt >> 23) & 3;
    let dst_ctl = (d.cnt >> 21) & 3;
    let (mut src, mut dst) = (d.src, d.dst);
    let count = d.count;
    let mut access = Access::NonSeq;
    for _ in 0..count {
        if cpu == ARM9 {
            let mut b = Bus9 { s: &mut *s };
            if word {
                let v = b.read32(src & !3, access);
                b.write32(dst & !3, v, access);
            } else {
                let v = b.read16(src & !1, access);
                b.write16(dst & !1, v, access);
            }
        } else {
            let mut b = Bus7 { s: &mut *s };
            if word {
                let v = b.read32(src & !3, access);
                b.write32(dst & !3, v, access);
            } else {
                let v = b.read16(src & !1, access);
                b.write16(dst & !1, v, access);
            }
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
    let timing = dma_timing(cpu, d.cnt);
    let ch = &mut s.io[cpu].dma[n];
    ch.src = src;
    ch.dst = dst;
    let irq = ch.cnt & (1 << 30) != 0;
    if ch.cnt & (1 << 25) != 0 && timing != 0 {
        ch.count = dma_count(cpu, n, ch.cnt);
        if dst_ctl == 3 {
            ch.dst = ch.dad & 0x0FFF_FFFE;
        }
    } else {
        ch.cnt &= !0x8000_0000;
    }
    if irq {
        s.io[cpu].if_ |= IRQ_DMA0 << n;
    }
}

/// Feed card data to any DMA set to the card timing.
fn card_dma(s: &mut Shared) {
    let cpu = s.card_cpu();
    let timing = if cpu == ARM9 { 5 } else { 2 };
    for _ in 0..0x10000 {
        if !s.card.data_ready() {
            break;
        }
        let Some(n) = (0..4).find(|&n| {
            let c = s.io[cpu].dma[n].cnt;
            c & 0x8000_0000 != 0 && dma_timing(cpu, c) == timing
        }) else {
            break;
        };
        run_dma(s, cpu, n);
    }
}

fn card_read(s: &mut Shared) -> u32 {
    let (w, done) = s.card.read_data();
    if done && s.card.irq_enabled() {
        let cpu = s.card_cpu();
        s.request_irq(cpu, IRQ_CARD);
    }
    w
}

fn card_romctrl_write(s: &mut Shared, v: u32) {
    if s.card.write_romctrl(v) && s.card.irq_enabled() {
        let cpu = s.card_cpu();
        s.request_irq(cpu, IRQ_CARD);
    }
    card_dma(s);
}

// ============================================================= common IO

/// Registers with the same layout on both CPUs. Returns `None` if `off`
/// is not one of them.
fn common_read16(s: &mut Shared, cpu: usize, off: u32) -> Option<u16> {
    let now = s.time[cpu];
    Some(match off {
        0x004 => {
            let ds = s.io[cpu].dispstat;
            let setting = (ds >> 8) | ((ds & 0x80) << 1);
            (ds & 0xFFB8) | s.in_vblank as u16 | (s.in_hblank as u16) << 1 | ((s.vcount == setting) as u16) << 2
        }
        0x006 => s.vcount,
        0x0B0..=0x0DF => {
            let n = ((off - 0xB0) / 12) as usize;
            let d = &s.io[cpu].dma[n];
            match (off - 0xB0) % 12 {
                0 => d.sad as u16,
                2 => (d.sad >> 16) as u16,
                4 => d.dad as u16,
                6 => (d.dad >> 16) as u16,
                8 => d.cnt as u16,
                _ => (d.cnt >> 16) as u16,
            }
        }
        0x100..=0x10F => {
            let n = ((off - 0x100) / 4) as usize;
            if off & 2 == 0 {
                s.io[cpu].timers[n].value(n, now)
            } else {
                s.io[cpu].timers[n].ctrl
            }
        }
        0x130 => s.keyinput,
        0x132 => s.io[cpu].keycnt,
        0x180 => s.ipc.read_sync(cpu),
        0x184 => s.ipc.read_fifocnt(cpu),
        0x1A0 => s.card.auxspicnt,
        0x1A2 => s.card.read_auxspidata() as u16,
        0x1A4 => s.card.romctrl as u16,
        0x1A6 => (s.card.romctrl >> 16) as u16,
        0x1A8..=0x1AF => {
            let i = (off - 0x1A8) as usize;
            s.card.command[i] as u16 | (s.card.command[i + 1] as u16) << 8
        }
        0x204 => s.exmemcnt,
        0x208 => s.io[cpu].ime as u16,
        0x210 => s.io[cpu].ie as u16,
        0x212 => (s.io[cpu].ie >> 16) as u16,
        0x214 => s.io[cpu].if_ as u16,
        0x216 => (s.io[cpu].if_ >> 16) as u16,
        _ => return None,
    })
}

fn common_write16(s: &mut Shared, cpu: usize, off: u32, v: u16) -> bool {
    match off {
        0x004 => s.io[cpu].dispstat = v & 0xFFB8,
        0x0B0..=0x0DF => {
            let n = ((off - 0xB0) / 12) as usize;
            let d = &mut s.io[cpu].dma[n];
            match (off - 0xB0) % 12 {
                0 => d.sad = (d.sad & 0xFFFF_0000) | v as u32,
                2 => d.sad = (d.sad & 0xFFFF) | (v as u32) << 16,
                4 => d.dad = (d.dad & 0xFFFF_0000) | v as u32,
                6 => d.dad = (d.dad & 0xFFFF) | (v as u32) << 16,
                8 => d.cnt = (d.cnt & 0xFFFF_0000) | v as u32,
                _ => {
                    let full = (d.cnt & 0xFFFF) | (v as u32) << 16;
                    if dma_write_cnt(s, cpu, n, full) {
                        run_dma(s, cpu, n);
                    }
                }
            }
        }
        0x100..=0x10F => {
            let n = ((off - 0x100) / 4) as usize;
            s.timer_write(cpu, n, off & 2 != 0, v);
        }
        0x132 => s.io[cpu].keycnt = v,
        0x180 => {
            let irqs = s.ipc.write_sync(cpu, v);
            s.apply_ipc(irqs);
        }
        0x184 => {
            let irqs = s.ipc.write_fifocnt(cpu, v);
            s.apply_ipc(irqs);
        }
        0x1A0 => s.card.write_auxspicnt(v),
        0x1A2 => s.card.write_auxspidata(v as u8),
        0x1A4 => {
            let full = (s.card.romctrl & 0xFFFF_0000) | v as u32;
            s.card.romctrl = full;
        }
        0x1A6 => {
            let full = (s.card.romctrl & 0xFFFF) | (v as u32) << 16;
            card_romctrl_write(s, full);
        }
        0x1A8..=0x1AF => {
            let i = (off - 0x1A8) as usize;
            s.card.command[i] = v as u8;
            s.card.command[i + 1] = (v >> 8) as u8;
        }
        0x204 => {
            // The ARM9 owns the slot-access bits; the ARM7 can only change
            // its own GBA-slot timing bits.
            s.exmemcnt = if cpu == ARM9 { v | 0x2000 } else { (s.exmemcnt & 0xFF80) | (v & 0x7F) };
        }
        0x208 => s.io[cpu].ime = v & 1 != 0,
        0x210 => s.io[cpu].ie = (s.io[cpu].ie & 0xFFFF_0000) | v as u32,
        0x212 => s.io[cpu].ie = (s.io[cpu].ie & 0xFFFF) | (v as u32) << 16,
        0x214 => s.io[cpu].if_ &= !(v as u32),
        0x216 => s.io[cpu].if_ &= !((v as u32) << 16),
        _ => return false,
    }
    true
}

/// 32-bit registers that must not be split into halves.
fn common_read32(s: &mut Shared, cpu: usize, off: u32) -> Option<u32> {
    match off {
        0x10_0000 => {
            let (v, irqs) = s.ipc.receive(cpu);
            s.apply_ipc(irqs);
            Some(v)
        }
        0x10_0010 => Some(card_read(s)),
        _ => None,
    }
}

fn common_write32(s: &mut Shared, cpu: usize, off: u32, v: u32) -> bool {
    match off {
        0x188 => {
            let irqs = s.ipc.send(cpu, v);
            s.apply_ipc(irqs);
        }
        0x1A4 => card_romctrl_write(s, v),
        _ => return false,
    }
    true
}

// ============================================================== ARM9 bus

pub struct Bus9<'a> {
    pub s: &'a mut Shared,
}

impl Bus9<'_> {
    fn itcm_hit(&self, addr: u32, write: bool) -> bool {
        let c = &self.s.cp15;
        c.in_itcm(addr) && if write { c.itcm_writable() } else { c.itcm_readable() }
    }
    fn dtcm_hit(&self, addr: u32, write: bool) -> bool {
        let c = &self.s.cp15;
        c.in_dtcm(addr) && if write { c.dtcm_writable() } else { c.dtcm_readable() }
    }

    fn wram9(&self, addr: u32) -> Option<usize> {
        let a = addr as usize;
        match self.s.wramcnt & 3 {
            0 => Some(a & 0x7FFF),
            1 => Some(0x4000 + (a & 0x3FFF)),
            2 => Some(a & 0x3FFF),
            _ => None,
        }
    }

    fn cost(&self, addr: u32) -> u64 {
        match addr >> 24 {
            0x02 => 2,
            0x03..=0x07 => 2,
            0x08..=0x0A => 8,
            _ => 1,
        }
    }

    fn r32(&mut self, addr: u32) -> u32 {
        if self.itcm_hit(addr, false) {
            return rd32(&self.s.itcm, addr as usize & 0x7FFC);
        }
        if self.dtcm_hit(addr, false) {
            return rd32(&self.s.dtcm, (addr - self.s.cp15.dtcm_base) as usize & 0x3FFC);
        }
        match addr >> 24 {
            0x02 => rd32(&self.s.main_ram, addr as usize & 0x3F_FFFC),
            0x03 => self.wram9(addr).map_or(0, |a| rd32(&self.s.wram, a & !3)),
            0x04 => self.io_read32(addr & 0xFF_FFFC),
            0x05 => rd32(&self.s.palette, addr as usize & 0x7FC),
            0x06 => {
                let (v, o) = Vram::arm9_view(addr & !3);
                self.s.vram.read32(v, o)
            }
            0x07 => rd32(&self.s.oam, addr as usize & 0x7FC),
            0x08..=0x0A => 0xFFFF_FFFF,
            0xFF if addr >= 0xFFFF_0000 => rd32(&self.s.bios9, addr as usize & 0x7FFC),
            _ => 0,
        }
    }

    fn r16(&mut self, addr: u32) -> u16 {
        match addr >> 24 {
            0x04 if !self.itcm_hit(addr, false) && !self.dtcm_hit(addr, false) => self.io_read16(addr & 0xFF_FFFE),
            _ => (self.r32(addr & !3) >> ((addr & 2) * 8)) as u16,
        }
    }

    fn r8(&mut self, addr: u32) -> u8 {
        match addr >> 24 {
            0x04 if !self.itcm_hit(addr, false) && !self.dtcm_hit(addr, false) => {
                (self.io_read16(addr & 0xFF_FFFE) >> ((addr & 1) * 8)) as u8
            }
            _ => (self.r32(addr & !3) >> ((addr & 3) * 8)) as u8,
        }
    }

    fn w32(&mut self, addr: u32, v: u32) {
        if self.itcm_hit(addr, true) {
            return wr32(&mut self.s.itcm, addr as usize & 0x7FFC, v);
        }
        if self.dtcm_hit(addr, true) {
            let a = (addr - self.s.cp15.dtcm_base) as usize & 0x3FFC;
            return wr32(&mut self.s.dtcm, a, v);
        }
        match addr >> 24 {
            0x02 => wr32(&mut self.s.main_ram, addr as usize & 0x3F_FFFC, v),
            0x03 => {
                if let Some(a) = self.wram9(addr) {
                    wr32(&mut self.s.wram, a & !3, v);
                }
            }
            0x04 => self.io_write32(addr & 0xFF_FFFC, v),
            0x05 => wr32(&mut self.s.palette, addr as usize & 0x7FC, v),
            0x06 => {
                let (view, o) = Vram::arm9_view(addr & !3);
                self.s.vram.write32(view, o, v);
            }
            0x07 => wr32(&mut self.s.oam, addr as usize & 0x7FC, v),
            _ => {}
        }
    }

    fn w16(&mut self, addr: u32, v: u16) {
        if self.itcm_hit(addr, true) {
            return wr16(&mut self.s.itcm, addr as usize & 0x7FFE, v);
        }
        if self.dtcm_hit(addr, true) {
            let a = (addr - self.s.cp15.dtcm_base) as usize & 0x3FFE;
            return wr16(&mut self.s.dtcm, a, v);
        }
        match addr >> 24 {
            0x02 => wr16(&mut self.s.main_ram, addr as usize & 0x3F_FFFE, v),
            0x03 => {
                if let Some(a) = self.wram9(addr) {
                    wr16(&mut self.s.wram, a & !1, v);
                }
            }
            0x04 => self.io_write16(addr & 0xFF_FFFE, v),
            0x05 => wr16(&mut self.s.palette, addr as usize & 0x7FE, v),
            0x06 => {
                let (view, o) = Vram::arm9_view(addr & !1);
                self.s.vram.write16(view, o, v);
            }
            0x07 => wr16(&mut self.s.oam, addr as usize & 0x7FE, v),
            _ => {}
        }
    }

    fn w8(&mut self, addr: u32, v: u8) {
        if self.itcm_hit(addr, true) {
            self.s.itcm[addr as usize & 0x7FFF] = v;
            return;
        }
        if self.dtcm_hit(addr, true) {
            let a = (addr - self.s.cp15.dtcm_base) as usize & 0x3FFF;
            self.s.dtcm[a] = v;
            return;
        }
        match addr >> 24 {
            0x02 => self.s.main_ram[addr as usize & 0x3F_FFFF] = v,
            0x03 => {
                if let Some(a) = self.wram9(addr) {
                    self.s.wram[a] = v;
                }
            }
            0x04 => self.io_write8(addr & 0xFF_FFFF, v),
            _ => {} // 8-bit writes to VRAM, palette and OAM are ignored
        }
    }

    pub fn io_read16(&mut self, off: u32) -> u16 {
        if let Some(v) = common_read16(self.s, ARM9, off) {
            return v;
        }
        let s = &mut *self.s;
        match off {
            0x060 => s.gpu3d.disp3dcnt,
            0x000..=0x06F => s.engine_a.read16(off),
            0x0E0..=0x0EF => {
                let d = &s.io[ARM9].dma[((off - 0xE0) / 4) as usize];
                (d.fill >> ((off & 2) * 8)) as u16
            }
            0x240..=0x24A => {
                let reg8 = |o: u32| -> u8 {
                    match o {
                        0x240..=0x246 => s.vram.cnt[(o - 0x240) as usize],
                        0x247 => s.wramcnt,
                        0x248 => s.vram.cnt[7],
                        0x249 => s.vram.cnt[8],
                        _ => 0,
                    }
                };
                reg8(off) as u16 | (reg8(off + 1) as u16) << 8
            }
            0x280 => s.divcnt,
            0x290..=0x2AF => {
                let (reg, shift) = ((off - 0x290) / 8, (off & 6) * 8);
                let v = match reg {
                    0 => s.div_numer,
                    1 => s.div_denom,
                    2 => s.div_result,
                    _ => s.divrem_result,
                };
                (v >> shift) as u16
            }
            0x2B0 => s.sqrtcnt,
            0x2B4 => s.sqrt_result as u16,
            0x2B6 => (s.sqrt_result >> 16) as u16,
            0x2B8..=0x2BF => (s.sqrt_param >> ((off & 6) * 8)) as u16,
            0x300 => s.io[ARM9].postflg as u16,
            0x304 => s.powcnt1,
            0x320..=0x6A3 => s.gpu3d.read16(off),
            0x1000..=0x106F => s.engine_b.read16(off - 0x1000),
            _ => 0,
        }
    }

    fn io_read32(&mut self, off: u32) -> u32 {
        if let Some(v) = common_read32(self.s, ARM9, off) {
            return v;
        }
        if (0x600..=0x6A3).contains(&off) {
            return self.s.gpu3d.read32(off);
        }
        self.io_read16(off) as u32 | (self.io_read16(off + 2) as u32) << 16
    }

    pub fn io_write16(&mut self, off: u32, v: u16) {
        self.s.io[ARM9].shadow[(off as usize >> 1) & 0x3FF] = v;
        if common_write16(self.s, ARM9, off, v) {
            return;
        }
        let s = &mut *self.s;
        match off {
            0x060 => s.gpu3d.write16(0x060, v),
            0x000..=0x06F => s.engine_a.write16(off, v),
            0x0E0..=0x0EF => {
                let d = &mut s.io[ARM9].dma[((off - 0xE0) / 4) as usize];
                let sh = (off & 2) * 8;
                d.fill = (d.fill & !(0xFFFF << sh)) | (v as u32) << sh;
            }
            0x240..=0x24A => {
                self.io_write8(off, v as u8);
                self.io_write8(off + 1, (v >> 8) as u8);
            }
            0x280 => {
                s.divcnt = v & 3;
                s.update_div();
            }
            0x290..=0x29F => {
                let (reg, shift) = ((off - 0x290) / 8, (off & 6) * 8);
                let target = if reg == 0 { &mut s.div_numer } else { &mut s.div_denom };
                *target = (*target & !(0xFFFF << shift)) | (v as u64) << shift;
                s.update_div();
            }
            0x2B0 => {
                s.sqrtcnt = v & 1;
                s.update_sqrt();
            }
            0x2B8..=0x2BF => {
                let shift = (off & 6) * 8;
                s.sqrt_param = (s.sqrt_param & !(0xFFFF << shift)) | (v as u64) << shift;
                s.update_sqrt();
            }
            0x300 => s.io[ARM9].postflg = (s.io[ARM9].postflg & 1) | (v as u8 & 2),
            0x304 => s.powcnt1 = v & 0x820F,
            0x320..=0x6A3 => s.gpu3d.write16(off, v),
            0x1000..=0x106F => s.engine_b.write16(off - 0x1000, v),
            _ => {}
        }
    }

    fn io_write32(&mut self, off: u32, v: u32) {
        if common_write32(self.s, ARM9, off, v) {
            return;
        }
        match off {
            0x400..=0x5FF => {
                self.s.gpu3d.write32(off, v);
            }
            0x0B8 | 0x0C4 | 0x0D0 | 0x0DC => {
                let n = ((off - 0xB8) / 12) as usize;
                if dma_write_cnt(self.s, ARM9, n, v) {
                    run_dma(self.s, ARM9, n);
                }
            }
            _ => {
                self.io_write16(off, v as u16);
                self.io_write16(off + 2, (v >> 16) as u16);
            }
        }
    }

    fn io_write8(&mut self, off: u32, v: u8) {
        let s = &mut *self.s;
        match off {
            0x240..=0x246 => s.vram.set_cnt((off - 0x240) as usize, v),
            0x247 => s.wramcnt = v & 3,
            0x248 => s.vram.set_cnt(7, v),
            0x249 => s.vram.set_cnt(8, v),
            0x1A8..=0x1AF => s.card.command[(off - 0x1A8) as usize] = v,
            0x214..=0x217 => s.io[ARM9].if_ &= !((v as u32) << ((off - 0x214) * 8)),
            0x300 => s.io[ARM9].postflg = v & 3,
            _ => {
                let idx = (off as usize >> 1) & 0x3FF;
                let cur = s.io[ARM9].shadow[idx];
                let merged = if off & 1 == 0 { (cur & 0xFF00) | v as u16 } else { (cur & 0xFF) | (v as u16) << 8 };
                self.io_write16(off & !1, merged);
            }
        }
    }
}

impl Bus for Bus9<'_> {
    fn read8(&mut self, addr: u32, _: Access) -> u8 {
        self.s.time[ARM9] += self.cost(addr);
        self.r8(addr)
    }
    fn read16(&mut self, addr: u32, _: Access) -> u16 {
        self.s.time[ARM9] += self.cost(addr);
        self.r16(addr)
    }
    fn read32(&mut self, addr: u32, _: Access) -> u32 {
        self.s.time[ARM9] += self.cost(addr);
        self.r32(addr)
    }
    fn write8(&mut self, addr: u32, v: u8, _: Access) {
        self.s.time[ARM9] += self.cost(addr);
        self.w8(addr, v)
    }
    fn write16(&mut self, addr: u32, v: u16, _: Access) {
        self.s.time[ARM9] += self.cost(addr);
        self.w16(addr, v)
    }
    fn write32(&mut self, addr: u32, v: u32, _: Access) {
        self.s.time[ARM9] += self.cost(addr);
        self.w32(addr, v)
    }
    fn fetch16(&mut self, addr: u32, _: Access) -> u16 {
        self.s.time[ARM9] += 1;
        self.r16(addr)
    }
    fn fetch32(&mut self, addr: u32, _: Access) -> u32 {
        self.s.time[ARM9] += 1;
        self.r32(addr)
    }
    fn idle(&mut self, cycles: u32) {
        self.s.time[ARM9] += cycles as u64;
    }
    fn hle_swi(&mut self, cpu: &mut Arm, comment: u32) -> SwiOutcome {
        let n = if cpu.thumb() { comment & 0xFF } else { (comment >> 16) & 0xFF };
        crate::bios::swi9(self, cpu, n as u8)
    }
    fn cp_read(&mut self, cp: u8, op1: u8, crn: u8, crm: u8, op2: u8) -> Option<u32> {
        if cp != 15 {
            return None;
        }
        self.s.cp15.read(op1, crn, crm, op2)
    }
    fn cp_write(&mut self, cp: u8, op1: u8, crn: u8, crm: u8, op2: u8, value: u32) -> bool {
        if cp != 15 {
            return false;
        }
        let ok = self.s.cp15.write(op1, crn, crm, op2, value);
        if self.s.cp15.halt_requested {
            self.s.cp15.halt_requested = false;
            self.s.io[ARM9].halt_request = true;
        }
        ok
    }
    fn exception_base(&self) -> u32 {
        self.s.cp15.exception_base()
    }
}

// ============================================================== ARM7 bus

pub struct Bus7<'a> {
    pub s: &'a mut Shared,
}

impl Bus7<'_> {
    fn shared_wram7(&self, addr: u32) -> Option<usize> {
        let a = addr as usize;
        match self.s.wramcnt & 3 {
            0 => None,
            1 => Some(a & 0x3FFF),
            2 => Some(0x4000 + (a & 0x3FFF)),
            _ => Some(a & 0x7FFF),
        }
    }

    fn cost(&self, addr: u32) -> u64 {
        match addr >> 24 {
            0x02 => 4,
            0x08..=0x0A => 16,
            _ => 2,
        }
    }

    fn mem32(&mut self, addr: u32) -> u32 {
        match addr >> 24 {
            0x00 if addr < 0x4000 => rd32(&self.s.bios7, addr as usize & 0x3FFC),
            0x02 => rd32(&self.s.main_ram, addr as usize & 0x3F_FFFC),
            0x03 => {
                if addr < 0x0380_0000 {
                    match self.shared_wram7(addr) {
                        Some(a) => rd32(&self.s.wram, a & !3),
                        None => rd32(&self.s.wram7, addr as usize & 0xFFFC),
                    }
                } else {
                    rd32(&self.s.wram7, addr as usize & 0xFFFC)
                }
            }
            0x04 => self.io_read32(addr & 0xFF_FFFC),
            0x06 => self.s.vram.read32(View::Arm7, addr & 0x3FFFC),
            0x08..=0x0A => 0xFFFF_FFFF,
            _ => 0,
        }
    }

    fn mem_write(&mut self, addr: u32, v: u32, width: u32) {
        let a = addr as usize;
        let put = |m: &mut [u8], at: usize| match width {
            1 => m[at] = v as u8,
            2 => wr16(m, at & !1, v as u16),
            _ => wr32(m, at & !3, v),
        };
        match addr >> 24 {
            0x02 => put(&mut self.s.main_ram, a & 0x3F_FFFF),
            0x03 => {
                if addr < 0x0380_0000 {
                    match self.shared_wram7(addr) {
                        Some(i) => put(&mut self.s.wram, i),
                        None => put(&mut self.s.wram7, a & 0xFFFF),
                    }
                } else {
                    put(&mut self.s.wram7, a & 0xFFFF);
                }
            }
            0x04 => match width {
                1 => self.io_write8(addr & 0xFF_FFFF, v as u8),
                2 => self.io_write16(addr & 0xFF_FFFE, v as u16),
                _ => self.io_write32(addr & 0xFF_FFFC, v),
            },
            0x06 => match width {
                1 => self.s.vram.write8(View::Arm7, addr & 0x3FFFF, v as u8),
                2 => self.s.vram.write16(View::Arm7, addr & 0x3FFFE, v as u16),
                _ => self.s.vram.write32(View::Arm7, addr & 0x3FFFC, v),
            },
            _ => {}
        }
    }

    pub fn io_read16(&mut self, off: u32) -> u16 {
        if let Some(v) = common_read16(self.s, ARM7, off) {
            return v;
        }
        let s = &mut *self.s;
        match off {
            0x134 => s.rcnt,
            0x136 => s.extkeyin,
            0x138 => s.rtc.read(),
            0x1C0 => s.spi.read_cnt(),
            0x1C2 => s.spi.read_data() as u16,
            0x240 => s.vram.arm7_stat() as u16 | (s.wramcnt as u16) << 8,
            0x300 => s.io[ARM7].postflg as u16,
            0x304 => s.powcnt2,
            0x400..=0x51F => {
                let i = (off - 0x400) as usize;
                rd16(&s.sound_regs, i)
            }
            _ => 0,
        }
    }

    fn io_read32(&mut self, off: u32) -> u32 {
        if let Some(v) = common_read32(self.s, ARM7, off) {
            return v;
        }
        if off >= 0x80_0000 {
            return 0; // wireless
        }
        self.io_read16(off) as u32 | (self.io_read16(off + 2) as u32) << 16
    }

    pub fn io_write16(&mut self, off: u32, v: u16) {
        self.s.io[ARM7].shadow[(off as usize >> 1) & 0x3FF] = v;
        if common_write16(self.s, ARM7, off, v) {
            return;
        }
        let s = &mut *self.s;
        match off {
            0x134 => s.rcnt = v,
            0x138 => {
                let clock = s.clock;
                s.rtc.write(v, &clock);
            }
            0x1C0 => s.spi.write_cnt(v),
            0x1C2 => s.spi.write_data(v as u8),
            0x300 => {
                s.io[ARM7].postflg = v as u8 & 1;
                if v & 0xC000 == 0x8000 {
                    s.io[ARM7].halt_request = true;
                }
            }
            0x304 => s.powcnt2 = v & 3,
            0x400..=0x51F => {
                let i = (off - 0x400) as usize;
                wr16(&mut s.sound_regs, i, v);
            }
            _ => {}
        }
    }

    fn io_write32(&mut self, off: u32, v: u32) {
        if common_write32(self.s, ARM7, off, v) {
            return;
        }
        match off {
            0x0B8 | 0x0C4 | 0x0D0 | 0x0DC => {
                let n = ((off - 0xB8) / 12) as usize;
                if dma_write_cnt(self.s, ARM7, n, v) {
                    run_dma(self.s, ARM7, n);
                }
            }
            _ => {
                self.io_write16(off, v as u16);
                self.io_write16(off + 2, (v >> 16) as u16);
            }
        }
    }

    fn io_write8(&mut self, off: u32, v: u8) {
        let s = &mut *self.s;
        match off {
            0x1A8..=0x1AF => s.card.command[(off - 0x1A8) as usize] = v,
            0x214..=0x217 => s.io[ARM7].if_ &= !((v as u32) << ((off - 0x214) * 8)),
            0x300 => s.io[ARM7].postflg = v & 1,
            0x301 => {
                if v & 0xC0 == 0x80 {
                    s.io[ARM7].halt_request = true;
                }
            }
            _ => {
                let idx = (off as usize >> 1) & 0x3FF;
                let cur = s.io[ARM7].shadow[idx];
                let merged = if off & 1 == 0 { (cur & 0xFF00) | v as u16 } else { (cur & 0xFF) | (v as u16) << 8 };
                self.io_write16(off & !1, merged);
            }
        }
    }
}

impl Bus for Bus7<'_> {
    fn read8(&mut self, addr: u32, _: Access) -> u8 {
        self.s.time[ARM7] += self.cost(addr);
        if addr >> 24 == 0x04 {
            return (self.io_read16(addr & 0xFF_FFFE) >> ((addr & 1) * 8)) as u8;
        }
        (self.mem32(addr & !3) >> ((addr & 3) * 8)) as u8
    }
    fn read16(&mut self, addr: u32, _: Access) -> u16 {
        self.s.time[ARM7] += self.cost(addr);
        if addr >> 24 == 0x04 && addr < 0x0480_0000 {
            return self.io_read16(addr & 0xFF_FFFE);
        }
        (self.mem32(addr & !3) >> ((addr & 2) * 8)) as u16
    }
    fn read32(&mut self, addr: u32, _: Access) -> u32 {
        self.s.time[ARM7] += self.cost(addr);
        self.mem32(addr)
    }
    fn write8(&mut self, addr: u32, v: u8, _: Access) {
        self.s.time[ARM7] += self.cost(addr);
        self.mem_write(addr, v as u32, 1)
    }
    fn write16(&mut self, addr: u32, v: u16, _: Access) {
        self.s.time[ARM7] += self.cost(addr);
        self.mem_write(addr, v as u32, 2)
    }
    fn write32(&mut self, addr: u32, v: u32, _: Access) {
        self.s.time[ARM7] += self.cost(addr);
        self.mem_write(addr, v, 4)
    }
    fn idle(&mut self, cycles: u32) {
        self.s.time[ARM7] += cycles as u64 * 2;
    }
    fn hle_swi(&mut self, cpu: &mut Arm, comment: u32) -> SwiOutcome {
        let n = if cpu.thumb() { comment & 0xFF } else { (comment >> 16) & 0xFF };
        crate::bios::swi7(self, cpu, n as u8)
    }
}

// ========================================================== save states

impl SaveState for Shared {
    fn save_state(&self, w: &mut StateWriter) {
        w.u64(self.sched.now());
        let events = self.sched.snapshot();
        w.u32(events.len() as u32);
        for (at, ev) in events {
            w.u64(at);
            w.u32(ev.slot() as u32);
        }
        w.u64(self.time[0]);
        w.u64(self.time[1]);
        w.bytes(&self.main_ram);
        w.bytes(&self.wram);
        w.bytes(&self.wram7);
        w.u8(self.wramcnt);
        w.section(*b"VRAM", &self.vram);
        w.bytes(&self.palette);
        w.bytes(&self.oam);
        w.bytes(&self.itcm);
        w.bytes(&self.dtcm);
        w.section(*b"CP15", &self.cp15);
        for io in &self.io {
            w.u32(io.ie);
            w.u32(io.if_);
            w.bool(io.ime);
            w.u16(io.dispstat);
            w.u16(io.keycnt);
            for t in &io.timers {
                w.u16(t.reload);
                w.u16(t.ctrl);
                w.u16(t.counter);
                w.u64(t.started_at);
            }
            for d in &io.dma {
                w.u32s(&[d.sad, d.dad, d.cnt, d.fill, d.src, d.dst, d.count]);
            }
            w.bool(io.intr_wait_active);
            w.u8(io.postflg);
            w.u16s(&io.shadow);
        }
        w.section(*b"IPC0", &self.ipc);
        w.section(*b"CARD", &self.card);
        w.section(*b"SPI0", &self.spi);
        w.section(*b"RTC0", &self.rtc);
        w.section(*b"ENGA", &self.engine_a);
        w.section(*b"ENGB", &self.engine_b);
        w.section(*b"GX3D", &self.gpu3d);
        w.u16(self.vcount);
        w.bool(self.in_hblank);
        w.bool(self.in_vblank);
        w.u16(self.powcnt1);
        w.u16(self.powcnt2);
        w.u16(self.exmemcnt);
        w.u16(self.rcnt);
        w.u16(self.divcnt);
        w.u64(self.div_numer);
        w.u64(self.div_denom);
        w.u16(self.sqrtcnt);
        w.u64(self.sqrt_param);
        w.bytes(&self.sound_regs);
        w.i64(self.clock.offset_secs);
    }

    fn load_state(&mut self, r: &mut StateReader<'_>) -> StateResult<()> {
        let now = r.u64()?;
        let n = r.u32()? as usize;
        let mut events = Vec::with_capacity(n);
        for _ in 0..n {
            let at = r.u64()?;
            let ev = NdsEvent::decode(r.u32()?).ok_or(kd_core::StateError::Invalid("bad NDS event"))?;
            events.push((at, ev));
        }
        self.sched.restore(now, &events);
        self.time = [r.u64()?, r.u64()?];
        r.bytes_into(&mut self.main_ram)?;
        r.bytes_into(&mut self.wram)?;
        r.bytes_into(&mut self.wram7)?;
        self.wramcnt = r.u8()?;
        r.section(*b"VRAM", &mut self.vram)?;
        r.bytes_into(&mut self.palette)?;
        r.bytes_into(&mut self.oam)?;
        r.bytes_into(&mut self.itcm)?;
        r.bytes_into(&mut self.dtcm)?;
        r.section(*b"CP15", &mut self.cp15)?;
        for io in &mut self.io {
            io.ie = r.u32()?;
            io.if_ = r.u32()?;
            io.ime = r.bool()?;
            io.dispstat = r.u16()?;
            io.keycnt = r.u16()?;
            for t in &mut io.timers {
                t.reload = r.u16()?;
                t.ctrl = r.u16()?;
                t.counter = r.u16()?;
                t.started_at = r.u64()?;
            }
            for d in &mut io.dma {
                let mut v = [0u32; 7];
                r.u32s_into(&mut v)?;
                [d.sad, d.dad, d.cnt, d.fill, d.src, d.dst, d.count] = v;
            }
            io.intr_wait_active = r.bool()?;
            io.postflg = r.u8()?;
            r.u16s_into(&mut io.shadow)?;
        }
        r.section(*b"IPC0", &mut self.ipc)?;
        r.section(*b"CARD", &mut self.card)?;
        r.section(*b"SPI0", &mut self.spi)?;
        r.section(*b"RTC0", &mut self.rtc)?;
        r.section(*b"ENGA", &mut self.engine_a)?;
        r.section(*b"ENGB", &mut self.engine_b)?;
        r.section(*b"GX3D", &mut self.gpu3d)?;
        self.vcount = r.u16()?;
        self.in_hblank = r.bool()?;
        self.in_vblank = r.bool()?;
        self.powcnt1 = r.u16()?;
        self.powcnt2 = r.u16()?;
        self.exmemcnt = r.u16()?;
        self.rcnt = r.u16()?;
        self.divcnt = r.u16()?;
        self.div_numer = r.u64()?;
        self.div_denom = r.u64()?;
        self.sqrtcnt = r.u16()?;
        self.sqrt_param = r.u64()?;
        r.bytes_into(&mut self.sound_regs)?;
        self.clock.offset_secs = r.i64()?;
        self.update_div();
        self.update_sqrt();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn divider_modes() {
        let mut s = Shared::new(vec![0; 0x200], vec![], vec![]);
        s.div_numer = (-100i64) as u64;
        s.div_denom = 7;
        s.divcnt = 0;
        s.update_div();
        assert_eq!(s.div_result as i64, -14);
        assert_eq!(s.divrem_result as i64, -2);
        s.div_denom = 0;
        s.update_div();
        assert_ne!(s.divcnt & 0x4000, 0);
        s.sqrt_param = 1 << 40;
        s.sqrtcnt = 1;
        s.update_sqrt();
        assert_eq!(s.sqrt_result, 1 << 20);
    }

    #[test]
    fn wramcnt_splits_shared_wram() {
        let mut s = Shared::new(vec![0; 0x200], vec![], vec![]);
        s.wramcnt = 1; // ARM9: second half, ARM7: first half
        Bus9 { s: &mut s }.w32(0x0300_0000, 0x9999_9999);
        Bus7 { s: &mut s }.mem_write(0x0300_0000, 0x7777_7777, 4);
        assert_eq!(rd32(&s.wram, 0x4000), 0x9999_9999);
        assert_eq!(rd32(&s.wram, 0), 0x7777_7777);
        s.wramcnt = 0; // ARM7 falls back to its own WRAM
        Bus7 { s: &mut s }.mem_write(0x0300_0010, 0x1234, 4);
        assert_eq!(rd32(&s.wram7, 0x10), 0x1234);
    }
}
