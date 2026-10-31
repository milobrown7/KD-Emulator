//! Audio: two square channels (one with sweep), wave channel, noise channel
//! and the two DirectSound FIFOs fed by timer-driven DMA. Mixed to stereo at
//! 32768 Hz (one sample every 512 CPU cycles).

use kd_core::{SaveState, StateReader, StateResult, StateWriter, StereoFrame};

pub const SAMPLE_RATE: u32 = 32768;
pub const CYCLES_PER_SAMPLE: u64 = 512;
/// The PSG frame sequencer runs at 512 Hz.
pub const CYCLES_PER_FRAME_STEP: u64 = 16_777_216 / 512;

const DUTY: [[u8; 8]; 4] = [
    [0, 0, 0, 0, 0, 0, 0, 1],
    [1, 0, 0, 0, 0, 0, 0, 1],
    [1, 0, 0, 0, 0, 1, 1, 1],
    [0, 1, 1, 1, 1, 1, 1, 0],
];

#[derive(Debug, Clone, Default)]
struct Envelope {
    initial: u8,
    increase: bool,
    period: u8,
    volume: u8,
    timer: u8,
}

impl Envelope {
    fn load(&mut self, reg: u16) {
        self.period = ((reg >> 8) & 7) as u8;
        self.increase = reg & 0x800 != 0;
        self.initial = (reg >> 12) as u8;
    }
    fn trigger(&mut self) {
        self.volume = self.initial;
        self.timer = self.period;
    }
    fn step(&mut self) {
        if self.period == 0 {
            return;
        }
        if self.timer > 0 {
            self.timer -= 1;
        }
        if self.timer == 0 {
            self.timer = self.period;
            if self.increase && self.volume < 15 {
                self.volume += 1;
            } else if !self.increase && self.volume > 0 {
                self.volume -= 1;
            }
        }
    }
    fn dac_on(&self) -> bool {
        self.initial != 0 || self.increase
    }
}

#[derive(Debug, Clone, Default)]
struct Square {
    enabled: bool,
    duty: usize,
    step: usize,
    freq: u16,
    timer: i64,
    length: u16,
    length_enable: bool,
    env: Envelope,
    // sweep (channel 1 only)
    sweep_period: u8,
    sweep_negate: bool,
    sweep_shift: u8,
    sweep_timer: u8,
    sweep_enabled: bool,
    shadow_freq: u16,
}

impl Square {
    fn period(&self) -> i64 {
        (2048 - self.freq as i64) * 16
    }
    fn advance(&mut self, cycles: i64) {
        if !self.enabled {
            return;
        }
        self.timer -= cycles;
        while self.timer <= 0 {
            self.timer += self.period();
            self.step = (self.step + 1) & 7;
        }
    }
    fn output(&self) -> i32 {
        if !self.enabled || !self.env.dac_on() {
            return 0;
        }
        let v = self.env.volume as i32;
        if DUTY[self.duty][self.step] != 0 {
            v
        } else {
            -v
        }
    }
    fn sweep_calc(&mut self) -> u16 {
        let delta = self.shadow_freq >> self.sweep_shift;
        let f = if self.sweep_negate { self.shadow_freq.wrapping_sub(delta) } else { self.shadow_freq + delta };
        if f > 2047 {
            self.enabled = false;
        }
        f
    }
    fn sweep_step(&mut self) {
        if self.sweep_timer > 0 {
            self.sweep_timer -= 1;
        }
        if self.sweep_timer == 0 {
            self.sweep_timer = if self.sweep_period == 0 { 8 } else { self.sweep_period };
            if self.sweep_enabled && self.sweep_period != 0 {
                let f = self.sweep_calc();
                if f <= 2047 && self.sweep_shift != 0 {
                    self.shadow_freq = f;
                    self.freq = f;
                    self.sweep_calc();
                }
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
struct Wave {
    dac: bool,
    enabled: bool,
    two_banks: bool,
    bank: usize,
    freq: u16,
    timer: i64,
    pos: usize,
    length: u16,
    length_enable: bool,
    volume: u16,
    force75: bool,
    ram: [u8; 32], // two banks of 16 bytes (32 nibbles each)
}

impl Wave {
    fn advance(&mut self, cycles: i64) {
        if !self.enabled {
            return;
        }
        self.timer -= cycles;
        let period = (2048 - self.freq as i64) * 8;
        let span = if self.two_banks { 64 } else { 32 };
        while self.timer <= 0 {
            self.timer += period;
            self.pos = (self.pos + 1) % span;
            if self.two_banks && self.pos == 32 {
                self.bank ^= 1;
            }
            if self.two_banks && self.pos == 0 {
                self.bank ^= 1;
            }
        }
    }
    fn output(&self) -> i32 {
        if !self.enabled || !self.dac {
            return 0;
        }
        let nib_index = self.pos & 31;
        let byte = self.ram[self.bank * 16 + nib_index / 2];
        let sample = if nib_index & 1 == 0 { byte >> 4 } else { byte & 0xF } as i32;
        let centered = sample * 2 - 15; // -15..15
        if self.force75 {
            return centered * 3 / 4;
        }
        match self.volume {
            0 => 0,
            1 => centered,
            2 => centered / 2,
            _ => centered / 4,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct Noise {
    enabled: bool,
    lfsr: u16,
    narrow: bool,
    divisor: u8,
    shift: u8,
    timer: i64,
    length: u16,
    length_enable: bool,
    env: Envelope,
}

impl Noise {
    fn period(&self) -> i64 {
        let d = if self.divisor == 0 { 8 } else { self.divisor as i64 * 16 };
        (d << self.shift) * 4
    }
    fn advance(&mut self, cycles: i64) {
        if !self.enabled {
            return;
        }
        self.timer -= cycles;
        while self.timer <= 0 {
            self.timer += self.period().max(1);
            let bit = (self.lfsr ^ (self.lfsr >> 1)) & 1;
            self.lfsr = (self.lfsr >> 1) | (bit << 14);
            if self.narrow {
                self.lfsr = (self.lfsr & !(1 << 6)) | (bit << 6);
            }
        }
    }
    fn output(&self) -> i32 {
        if !self.enabled || !self.env.dac_on() {
            return 0;
        }
        let v = self.env.volume as i32;
        if self.lfsr & 1 == 0 {
            v
        } else {
            -v
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Fifo {
    buf: [i8; 32],
    read: usize,
    len: usize,
    pub sample: i8,
}

impl Fifo {
    pub fn push(&mut self, v: i8) {
        if self.len < 32 {
            self.buf[(self.read + self.len) & 31] = v;
            self.len += 1;
        }
    }
    fn pop(&mut self) {
        if self.len > 0 {
            self.sample = self.buf[self.read];
            self.read = (self.read + 1) & 31;
            self.len -= 1;
        }
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    fn reset(&mut self) {
        self.read = 0;
        self.len = 0;
    }
}

pub struct Apu {
    sq1: Square,
    sq2: Square,
    wave: Wave,
    noise: Noise,
    pub fifo: [Fifo; 2],
    /// SOUNDCNT_L, SOUNDCNT_H, SOUNDCNT_X, SOUNDBIAS and the raw channel
    /// registers (halfword indexed from 0x60).
    regs: [u16; 0x18],
    frame_step: u8,
    pub out: Vec<StereoFrame>,
}

impl Default for Apu {
    fn default() -> Self {
        Self::new()
    }
}

impl Apu {
    pub fn new() -> Self {
        let mut a = Self {
            sq1: Square::default(),
            sq2: Square::default(),
            wave: Wave::default(),
            noise: Noise::default(),
            fifo: [Fifo::default(), Fifo::default()],
            regs: [0; 0x18],
            frame_step: 0,
            out: Vec::with_capacity(1024),
        };
        a.regs[(0x88 - 0x60) / 2] = 0x200;
        a
    }

    fn master_on(&self) -> bool {
        self.reg(0x84) & 0x80 != 0
    }

    #[inline]
    fn reg(&self, off: u32) -> u16 {
        self.regs[((off - 0x60) / 2) as usize]
    }

    pub fn read16(&self, off: u32) -> u16 {
        match off {
            0x60 => self.reg(0x60) & 0x7F,
            0x62 => self.reg(0x62) & 0xFFC0,
            0x64 => self.reg(0x64) & 0x4000,
            0x68 => self.reg(0x68) & 0xFFC0,
            0x6C => self.reg(0x6C) & 0x4000,
            0x70 => self.reg(0x70) & 0xE0,
            0x72 => self.reg(0x72) & 0xE000,
            0x74 => self.reg(0x74) & 0x4000,
            0x78 => self.reg(0x78) & 0xFF00,
            0x7C => self.reg(0x7C) & 0x40FF,
            0x80 => self.reg(0x80) & 0xFF77,
            0x82 => self.reg(0x82) & 0x770F,
            0x84 => {
                (self.reg(0x84) & 0x80)
                    | self.sq1.enabled as u16
                    | (self.sq2.enabled as u16) << 1
                    | (self.wave.enabled as u16) << 2
                    | (self.noise.enabled as u16) << 3
            }
            0x88 => self.reg(0x88),
            0x90..=0x9F => {
                // CPU sees the bank not selected for playback.
                let bank = if self.reg(0x70) & 0x40 != 0 { 0 } else { 1 };
                let i = bank * 16 + (off as usize - 0x90);
                u16::from_le_bytes([self.wave.ram[i], self.wave.ram[i + 1]])
            }
            _ => 0,
        }
    }

    pub fn write16(&mut self, off: u32, v: u16) {
        if (0x90..=0x9F).contains(&off) {
            let bank = if self.reg(0x70) & 0x40 != 0 { 0 } else { 1 };
            let i = bank * 16 + (off as usize - 0x90);
            self.wave.ram[i..i + 2].copy_from_slice(&v.to_le_bytes());
            return;
        }
        if !(0x60..0x90).contains(&off) {
            return;
        }
        if !self.master_on() && off != 0x84 && off != 0x88 && off != 0x82 {
            return;
        }
        self.regs[((off - 0x60) / 2) as usize] = v;
        match off {
            0x60 => {
                self.sq1.sweep_shift = (v & 7) as u8;
                self.sq1.sweep_negate = v & 8 != 0;
                self.sq1.sweep_period = ((v >> 4) & 7) as u8;
            }
            0x62 => {
                self.sq1.length = 64 - (v & 0x3F);
                self.sq1.duty = ((v >> 6) & 3) as usize;
                self.sq1.env.load(v);
                if !self.sq1.env.dac_on() {
                    self.sq1.enabled = false;
                }
            }
            0x64 => {
                self.sq1.freq = v & 0x7FF;
                self.sq1.length_enable = v & 0x4000 != 0;
                if v & 0x8000 != 0 {
                    let s = &mut self.sq1;
                    s.enabled = s.env.dac_on();
                    s.timer = s.period();
                    s.env.trigger();
                    if s.length == 0 {
                        s.length = 64;
                    }
                    s.shadow_freq = s.freq;
                    s.sweep_timer = if s.sweep_period == 0 { 8 } else { s.sweep_period };
                    s.sweep_enabled = s.sweep_period != 0 || s.sweep_shift != 0;
                    if s.sweep_shift != 0 {
                        s.sweep_calc();
                    }
                }
            }
            0x68 => {
                self.sq2.length = 64 - (v & 0x3F);
                self.sq2.duty = ((v >> 6) & 3) as usize;
                self.sq2.env.load(v);
                if !self.sq2.env.dac_on() {
                    self.sq2.enabled = false;
                }
            }
            0x6C => {
                self.sq2.freq = v & 0x7FF;
                self.sq2.length_enable = v & 0x4000 != 0;
                if v & 0x8000 != 0 {
                    let s = &mut self.sq2;
                    s.enabled = s.env.dac_on();
                    s.timer = s.period();
                    s.env.trigger();
                    if s.length == 0 {
                        s.length = 64;
                    }
                }
            }
            0x70 => {
                self.wave.two_banks = v & 0x20 != 0;
                self.wave.dac = v & 0x80 != 0;
                if !self.wave.dac {
                    self.wave.enabled = false;
                }
            }
            0x72 => {
                self.wave.length = 256 - (v & 0xFF);
                self.wave.volume = (v >> 13) & 3;
                self.wave.force75 = v & 0x8000 != 0;
            }
            0x74 => {
                self.wave.freq = v & 0x7FF;
                self.wave.length_enable = v & 0x4000 != 0;
                if v & 0x8000 != 0 {
                    let w = &mut self.wave;
                    w.enabled = w.dac;
                    w.timer = (2048 - w.freq as i64) * 8;
                    w.pos = 0;
                    w.bank = ((self.regs[(0x70 - 0x60) / 2] >> 6) & 1) as usize;
                    if w.length == 0 {
                        w.length = 256;
                    }
                }
            }
            0x78 => {
                self.noise.length = 64 - (v & 0x3F);
                self.noise.env.load(v);
                if !self.noise.env.dac_on() {
                    self.noise.enabled = false;
                }
            }
            0x7C => {
                self.noise.divisor = (v & 7) as u8;
                self.noise.narrow = v & 8 != 0;
                self.noise.shift = ((v >> 4) & 0xF) as u8;
                self.noise.length_enable = v & 0x4000 != 0;
                if v & 0x8000 != 0 {
                    let n = &mut self.noise;
                    n.enabled = n.env.dac_on();
                    n.lfsr = 0x7FFF;
                    n.timer = n.period();
                    n.env.trigger();
                    if n.length == 0 {
                        n.length = 64;
                    }
                }
            }
            0x82 => {
                if v & 0x0800 != 0 {
                    self.fifo[0].reset();
                }
                if v & 0x8000 != 0 {
                    self.fifo[1].reset();
                }
                self.regs[(0x82 - 0x60) / 2] = v & 0x770F;
            }
            // Master off clears all PSG registers.
            0x84 if v & 0x80 == 0 => {
                for r in self.regs[..(0x80 - 0x60) / 2 + 1].iter_mut() {
                    *r = 0;
                }
                self.sq1.enabled = false;
                self.sq2.enabled = false;
                self.wave.enabled = false;
                self.noise.enabled = false;
            }
            _ => {}
        }
    }

    /// Write to FIFO A (0) or B (1).
    pub fn fifo_write(&mut self, n: usize, bytes: &[u8]) {
        for &b in bytes {
            self.fifo[n].push(b as i8);
        }
    }

    /// A timer overflowed; returns which FIFOs want a DMA refill
    /// (bit 0 = A, bit 1 = B).
    pub fn timer_overflow(&mut self, timer: usize) -> u8 {
        let cnt = self.reg(0x82);
        let mut refill = 0;
        for n in 0..2 {
            let sel = (cnt >> (10 + n * 4)) & 1;
            if sel as usize == timer {
                self.fifo[n].pop();
                if self.fifo[n].len() <= 16 {
                    refill |= 1 << n;
                }
            }
        }
        refill
    }

    /// 512 Hz frame sequencer step.
    pub fn frame_sequencer(&mut self) {
        let step = self.frame_step;
        self.frame_step = (self.frame_step + 1) & 7;
        if step % 2 == 0 {
            let length = |enabled: &mut bool, len: &mut u16, en: bool| {
                if en && *len > 0 {
                    *len -= 1;
                    if *len == 0 {
                        *enabled = false;
                    }
                }
            };
            length(&mut self.sq1.enabled, &mut self.sq1.length, self.sq1.length_enable);
            length(&mut self.sq2.enabled, &mut self.sq2.length, self.sq2.length_enable);
            length(&mut self.wave.enabled, &mut self.wave.length, self.wave.length_enable);
            length(&mut self.noise.enabled, &mut self.noise.length, self.noise.length_enable);
        }
        if step == 2 || step == 6 {
            self.sq1.sweep_step();
        }
        if step == 7 {
            self.sq1.env.step();
            self.sq2.env.step();
            self.noise.env.step();
        }
    }

    /// Produce one output sample covering the last 512 cycles.
    pub fn sample(&mut self) {
        let c = CYCLES_PER_SAMPLE as i64;
        self.sq1.advance(c);
        self.sq2.advance(c);
        self.wave.advance(c);
        self.noise.advance(c);
        if !self.master_on() {
            self.out.push(StereoFrame::SILENCE);
            return;
        }
        let l_r = self.reg(0x80);
        let outs = [self.sq1.output(), self.sq2.output(), self.wave.output(), self.noise.output()];
        let mut left = 0;
        let mut right = 0;
        for (i, &o) in outs.iter().enumerate() {
            if l_r & (0x100 << i) != 0 {
                right += o;
            }
            if l_r & (0x1000 << i) != 0 {
                left += o;
            }
        }
        right *= (l_r & 7) as i32 + 1;
        left *= ((l_r >> 4) & 7) as i32 + 1;
        let cnt = self.reg(0x82);
        let psg_shift = match cnt & 3 {
            0 => 2,
            1 => 1,
            _ => 0,
        };
        left >>= psg_shift;
        right >>= psg_shift;
        for n in 0..2 {
            let s = self.fifo[n].sample as i32;
            let full = cnt & (4 << n) != 0;
            let v = if full { s * 4 } else { s * 2 };
            if cnt & (0x100 << (n * 4)) != 0 {
                right += v;
            }
            if cnt & (0x200 << (n * 4)) != 0 {
                left += v;
            }
        }
        let scale = |v: i32| (v.clamp(-512, 511) * 64) as i16;
        self.out.push(StereoFrame::new(scale(left), scale(right)));
    }
}

impl SaveState for Apu {
    fn save_state(&self, w: &mut StateWriter) {
        w.u16s(&self.regs);
        w.bytes(&self.wave.ram);
        for f in &self.fifo {
            let bytes: Vec<u8> = (0..f.len).map(|i| f.buf[(f.read + i) & 31] as u8).collect();
            w.bytes(&bytes);
            w.u8(f.sample as u8);
        }
        w.u8(self.frame_step);
    }
    fn load_state(&mut self, r: &mut StateReader<'_>) -> StateResult<()> {
        let mut regs = [0u16; 0x18];
        r.u16s_into(&mut regs)?;
        r.bytes_into(&mut self.wave.ram)?;
        for f in &mut self.fifo {
            let bytes = r.bytes()?;
            f.reset();
            for &b in bytes {
                f.push(b as i8);
            }
            f.sample = r.u8()? as i8;
        }
        self.frame_step = r.u8()?;
        // Re-apply registers so channel parameters match (without retriggering).
        self.regs = regs;
        for off in [0x60, 0x62, 0x68, 0x70, 0x72, 0x78] {
            let v = self.reg(off);
            self.write16(off, v);
        }
        for off in [0x64u32, 0x6C, 0x74, 0x7C] {
            let v = self.reg(off) & 0x7FFF;
            self.write16(off, v);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn square_wave_produces_tone() {
        let mut a = Apu::new();
        a.write16(0x84, 0x80);
        a.write16(0x80, 0x1177); // ch1 L+R, full volume
        a.write16(0x82, 0x0002); // PSG 100%
        a.write16(0x62, 0xF080); // vol 15, duty 50%
        a.write16(0x64, 0x8000 | 1750); // ~440 Hz, trigger
        for _ in 0..512 {
            a.sample();
        }
        let distinct: std::collections::HashSet<i16> = a.out.iter().map(|f| f.left).collect();
        assert!(distinct.len() >= 2, "expected a waveform, got {distinct:?}");
        assert!(a.out.iter().any(|f| f.left > 0) && a.out.iter().any(|f| f.left < 0));
    }

    #[test]
    fn fifo_pops_on_selected_timer() {
        let mut a = Apu::new();
        a.write16(0x84, 0x80);
        a.write16(0x82, 0x0B04 | 0x4000); // A: 100%, L+R, timer 0; B: timer 1
        a.fifo_write(0, &[10, 20, 30]);
        a.fifo_write(1, &[99]);
        assert_eq!(a.timer_overflow(0) & 1, 1, "FIFO A wants a refill");
        assert_eq!(a.fifo[0].sample, 10);
        assert_eq!(a.fifo[1].sample, 0, "B uses timer 1");
        a.timer_overflow(1);
        assert_eq!(a.fifo[1].sample, 99);
        a.sample();
        assert_eq!(a.out.last().unwrap().left, (10 * 4 * 64) as i16);
    }

    #[test]
    fn length_counter_silences_channel() {
        let mut a = Apu::new();
        a.write16(0x84, 0x80);
        a.write16(0x68, 0xF03F); // length 1
        a.write16(0x6C, 0xC000 | 1000); // length enable + trigger
        assert_eq!(a.read16(0x84) & 2, 2);
        a.frame_sequencer();
        assert_eq!(a.read16(0x84) & 2, 0);
    }
}
