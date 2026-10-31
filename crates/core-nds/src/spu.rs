//! DS sound processor (ARM7 `0x04000400..0x0400051F`).
//!
//! * 16 channels: PCM8, PCM16 and IMA-ADPCM on all of them, PSG square waves
//!   on channels 8–13 and white noise on 14–15.
//! * Per-channel volume (multiplier + divider) and panning, master volume,
//!   the channel 1/3 mixer bypass bits and both capture units (mixer or
//!   channel 0/2 source, PCM8/PCM16, loop or one-shot).
//! * Clocked once per output sample, every [`SAMPLE_CYCLES`] master cycles
//!   (67.03 MHz / 2048 = 32 728 Hz). Channel timers tick at ARM7/2
//!   (16.76 MHz), i.e. 512 ticks per output sample, and step to the next
//!   source sample on every overflow — nearest-sample playback, as the
//!   hardware does.

use kd_core::{SaveState, StateReader, StateResult, StateWriter, StereoFrame};

/// Master (ARM9) cycles between output samples.
pub const SAMPLE_CYCLES: u64 = 2048;
/// Output rate: 67 027 964 Hz / 2048.
pub const OUTPUT_RATE: u32 = 32_728;
/// Channel timer ticks per output sample.
const TICKS_PER_SAMPLE: u32 = 512;

const ADPCM_INDEX: [i8; 8] = [-1, -1, -1, -1, 2, 4, 6, 8];
const ADPCM_STEP: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66, 73, 80, 88, 97, 107,
    118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449, 494, 544, 598, 658, 724, 796, 876, 963,
    1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272, 2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894,
    6484, 7132, 7845, 8630, 9493, 10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794,
    32767,
];

/// Decode one IMA-ADPCM nibble (GBATEK formulation, clamped to ±0x7FFF).
pub fn adpcm_step(value: &mut i16, index: &mut u8, nibble: u8) {
    let step = ADPCM_STEP[*index as usize];
    let mut diff = step / 8;
    if nibble & 1 != 0 {
        diff += step / 4;
    }
    if nibble & 2 != 0 {
        diff += step / 2;
    }
    if nibble & 4 != 0 {
        diff += step;
    }
    let v = if nibble & 8 == 0 { (*value as i32 + diff).min(0x7FFF) } else { (*value as i32 - diff).max(-0x7FFF) };
    *value = v as i16;
    *index = (*index as i32 + ADPCM_INDEX[(nibble & 7) as usize] as i32).clamp(0, 88) as u8;
}

/// ARM7-visible memory the SPU fetches samples from and captures into.
pub struct SpuMem<'a> {
    pub main_ram: &'a mut [u8],
    pub wram: &'a mut [u8],
    pub wram7: &'a mut [u8],
    pub wramcnt: u8,
}

impl SpuMem<'_> {
    fn slot(&mut self, addr: u32) -> Option<(&mut [u8], usize)> {
        let a = addr as usize;
        match addr >> 24 {
            0x02 => Some((&mut *self.main_ram, a & 0x3F_FFFF)),
            0x03 if addr < 0x0380_0000 => match self.wramcnt & 3 {
                0 => Some((&mut *self.wram7, a & 0xFFFF)),
                1 => Some((&mut *self.wram, a & 0x3FFF)),
                2 => Some((&mut *self.wram, 0x4000 + (a & 0x3FFF))),
                _ => Some((&mut *self.wram, a & 0x7FFF)),
            },
            0x03 => Some((&mut *self.wram7, a & 0xFFFF)),
            _ => None,
        }
    }

    pub fn read8(&mut self, addr: u32) -> u8 {
        self.slot(addr).map_or(0, |(m, i)| m[i])
    }

    pub fn read16(&mut self, addr: u32) -> u16 {
        u16::from_le_bytes([self.read8(addr & !1), self.read8((addr & !1) + 1)])
    }

    pub fn read32(&mut self, addr: u32) -> u32 {
        self.read16(addr & !3) as u32 | (self.read16((addr & !3) + 2) as u32) << 16
    }

    pub fn write8(&mut self, addr: u32, v: u8) {
        if let Some((m, i)) = self.slot(addr) {
            m[i] = v;
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Channel {
    pub cnt: u32,
    pub sad: u32,
    pub tmr: u16,
    pub pnt: u16,
    pub len: u32,
    counter: u32,
    /// Index of the current source sample; -1 before the first fetch.
    pos: i32,
    sample: i16,
    adpcm: (i16, u8),
    adpcm_loop: (i16, u8),
    psg_step: u8,
    lfsr: u16,
}

impl Channel {
    pub fn running(&self) -> bool {
        self.cnt & 0x8000_0000 != 0
    }
    fn format(&self) -> u32 {
        (self.cnt >> 29) & 3
    }
    fn repeat(&self) -> u32 {
        (self.cnt >> 27) & 3
    }
    fn start(&mut self) {
        self.counter = self.tmr as u32;
        self.pos = -1;
        self.sample = 0;
        self.psg_step = 0;
        self.lfsr = 0x7FFF;
    }
    /// (loop start, end) in source samples.
    fn bounds(&self) -> (i32, i32) {
        let (pnt, len) = (self.pnt as i32, self.len as i32);
        match self.format() {
            0 => (pnt * 4, (pnt + len) * 4),
            1 => (pnt * 2, (pnt + len) * 2),
            _ => ((pnt - 1).max(0) * 8, (pnt + len - 1).max(0) * 8),
        }
    }

    /// Advance to the next source sample.
    fn advance(&mut self, idx: usize, mem: &mut SpuMem<'_>) {
        if self.format() == 3 {
            if (8..14).contains(&idx) {
                self.psg_step = (self.psg_step + 1) & 7;
                let duty = (self.cnt >> 24) & 7;
                self.sample = if self.psg_step as u32 >= 7 - duty { 0x7FFF } else { -0x7FFF };
            } else if idx >= 14 {
                if self.lfsr & 1 != 0 {
                    self.lfsr = (self.lfsr >> 1) ^ 0x6000;
                    self.sample = -0x7FFF;
                } else {
                    self.lfsr >>= 1;
                    self.sample = 0x7FFF;
                }
            }
            return;
        }
        let (loop_start, end) = self.bounds();
        if self.pos < 0 && self.format() == 2 {
            let header = mem.read32(self.sad);
            self.adpcm = (header as i16, ((header >> 16) & 0x7F).min(88) as u8);
            self.adpcm_loop = self.adpcm;
        }
        self.pos += 1;
        if self.pos >= end {
            if self.repeat() == 2 || end == 0 {
                self.cnt &= !0x8000_0000;
                self.sample = 0;
                return;
            }
            self.pos = loop_start;
            if self.format() == 2 {
                self.adpcm = self.adpcm_loop;
            }
        }
        let pos = self.pos as u32;
        self.sample = match self.format() {
            0 => ((mem.read8(self.sad.wrapping_add(pos)) as i8) as i16) << 8,
            1 => mem.read16(self.sad.wrapping_add(pos * 2)) as i16,
            _ => {
                if self.pos == loop_start {
                    self.adpcm_loop = self.adpcm;
                }
                let byte = mem.read8(self.sad.wrapping_add(4 + pos / 2));
                let nibble = if pos & 1 == 0 { byte & 0xF } else { byte >> 4 };
                let (mut v, mut i) = self.adpcm;
                adpcm_step(&mut v, &mut i, nibble);
                self.adpcm = (v, i);
                v
            }
        };
    }

    /// Run the channel timer for one output sample.
    fn clock(&mut self, idx: usize, mem: &mut SpuMem<'_>) {
        let mut c = self.counter + TICKS_PER_SAMPLE;
        while c >= 0x1_0000 && self.running() {
            c = c - 0x1_0000 + self.tmr as u32;
            self.advance(idx, mem);
        }
        self.counter = c;
    }

    /// Output after volume and divider (16-bit units, before panning).
    fn output(&self) -> i32 {
        if !self.running() {
            return 0;
        }
        let vol = (self.cnt & 0x7F) as i32;
        let shift = [0, 1, 2, 4][((self.cnt >> 8) & 3) as usize];
        ((self.sample as i32 * vol) >> 7) >> shift
    }

    fn pan(&self) -> i32 {
        ((self.cnt >> 16) & 0x7F) as i32
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Capture {
    pub cnt: u8,
    pub dad: u32,
    pub len: u16,
    counter: u32,
    pos: u32,
}

impl Capture {
    fn running(&self) -> bool {
        self.cnt & 0x80 != 0
    }
}

pub struct Spu {
    pub ch: [Channel; 16],
    pub soundcnt: u16,
    pub soundbias: u16,
    pub cap: [Capture; 2],
    /// Samples produced since the last drain.
    pub out: Vec<StereoFrame>,
}

impl Default for Spu {
    fn default() -> Self {
        Self::new()
    }
}

impl Spu {
    pub fn new() -> Self {
        Self {
            ch: [Channel::default(); 16],
            soundcnt: 0,
            soundbias: 0x200,
            cap: [Capture::default(); 2],
            out: Vec::new(),
        }
    }

    /// Register read; `off` is relative to `0x04000400`.
    pub fn read16(&self, off: u32) -> u16 {
        match off {
            0x000..=0x0FF => {
                let c = &self.ch[(off >> 4) as usize];
                match off & 0xF {
                    0x0 => c.cnt as u16,
                    0x2 => (c.cnt >> 16) as u16,
                    0x4 => c.sad as u16,
                    0x6 => (c.sad >> 16) as u16,
                    0x8 => c.tmr,
                    0xA => c.pnt,
                    0xC => c.len as u16,
                    _ => (c.len >> 16) as u16,
                }
            }
            0x100 => self.soundcnt,
            0x104 => self.soundbias,
            0x108 => self.cap[0].cnt as u16 | (self.cap[1].cnt as u16) << 8,
            0x110 => self.cap[0].dad as u16,
            0x112 => (self.cap[0].dad >> 16) as u16,
            0x114 => self.cap[0].len,
            0x118 => self.cap[1].dad as u16,
            0x11A => (self.cap[1].dad >> 16) as u16,
            0x11C => self.cap[1].len,
            _ => 0,
        }
    }

    /// Register write; `off` is relative to `0x04000400`.
    pub fn write16(&mut self, off: u32, v: u16) {
        match off {
            0x000..=0x0FF => {
                let c = &mut self.ch[(off >> 4) as usize];
                let v32 = v as u32;
                match off & 0xF {
                    0x0 => c.cnt = (c.cnt & 0xFFFF_0000) | (v32 & 0x837F),
                    0x2 => {
                        let was = c.running();
                        c.cnt = (c.cnt & 0xFFFF) | (v32 & 0xFF7F) << 16;
                        if !was && c.running() {
                            c.start();
                        }
                    }
                    0x4 => c.sad = (c.sad & 0xFFFF_0000) | (v32 & 0xFFFC),
                    0x6 => c.sad = (c.sad & 0xFFFF) | (v32 & 0x07FF) << 16,
                    0x8 => c.tmr = v,
                    0xA => c.pnt = v,
                    0xC => c.len = (c.len & 0xFFFF_0000) | v32,
                    _ => c.len = (c.len & 0xFFFF) | (v32 & 0x3F) << 16,
                }
            }
            0x100 => self.soundcnt = v & 0xBF7F,
            0x104 => self.soundbias = v & 0x3FF,
            0x108 => {
                for (i, byte) in [v as u8, (v >> 8) as u8].into_iter().enumerate() {
                    let cap = &mut self.cap[i];
                    let was = cap.running();
                    cap.cnt = byte & 0x8F;
                    if !was && cap.running() {
                        cap.pos = 0;
                        cap.counter = self.ch[1 + 2 * i].tmr as u32;
                    }
                }
            }
            0x110 => self.cap[0].dad = (self.cap[0].dad & 0xFFFF_0000) | (v as u32 & 0xFFFC),
            0x112 => self.cap[0].dad = (self.cap[0].dad & 0xFFFF) | (v as u32 & 0x07FF) << 16,
            0x114 => self.cap[0].len = v,
            0x118 => self.cap[1].dad = (self.cap[1].dad & 0xFFFF_0000) | (v as u32 & 0xFFFC),
            0x11A => self.cap[1].dad = (self.cap[1].dad & 0xFFFF) | (v as u32 & 0x07FF) << 16,
            0x11C => self.cap[1].len = v,
            _ => {}
        }
    }

    /// Produce one output sample.
    pub fn tick(&mut self, mem: &mut SpuMem<'_>) {
        for i in 0..16 {
            if self.ch[i].running() {
                self.ch[i].clock(i, mem);
            }
        }
        let (mut l, mut r) = (0i32, 0i32);
        let skip1 = self.soundcnt & 0x1000 != 0;
        let skip3 = self.soundcnt & 0x2000 != 0;
        for (i, c) in self.ch.iter().enumerate() {
            if (i == 1 && skip1) || (i == 3 && skip3) {
                continue;
            }
            let o = c.output();
            let pan = c.pan();
            l += (o * (128 - pan)) >> 7;
            r += (o * pan) >> 7;
        }
        let mixer = [l.clamp(-0x8000, 0x7FFF), r.clamp(-0x8000, 0x7FFF)];

        for i in 0..2 {
            if !self.cap[i].running() {
                continue;
            }
            let src = if self.cap[i].cnt & 2 != 0 { self.ch[2 * i].output() } else { mixer[i] };
            let tmr = self.ch[1 + 2 * i].tmr as u32;
            let cap = &mut self.cap[i];
            let mut c = cap.counter + TICKS_PER_SAMPLE;
            while c >= 0x1_0000 && cap.running() {
                c = c - 0x1_0000 + tmr;
                let pcm8 = cap.cnt & 8 != 0;
                let addr = cap.dad.wrapping_add(cap.pos);
                if pcm8 {
                    mem.write8(addr, (src >> 8) as u8);
                    cap.pos += 1;
                } else {
                    mem.write8(addr, src as u8);
                    mem.write8(addr + 1, (src >> 8) as u8);
                    cap.pos += 2;
                }
                if cap.pos >= (cap.len.max(1) as u32) * 4 {
                    cap.pos = 0;
                    if cap.cnt & 4 != 0 {
                        cap.cnt &= !0x80;
                    }
                }
            }
            cap.counter = c;
        }

        let frame = if self.soundcnt & 0x8000 == 0 {
            StereoFrame::SILENCE
        } else {
            let master = (self.soundcnt & 0x7F) as i32;
            let pick = |sel: u16, mix: i32| -> i32 {
                let o1 = self.ch[1].output();
                let o3 = self.ch[3].output();
                match sel & 3 {
                    0 => mix,
                    1 => o1,
                    2 => o3,
                    _ => o1 + o3,
                }
            };
            let lo = (pick(self.soundcnt >> 8, l) * master) >> 7;
            let ro = (pick(self.soundcnt >> 10, r) * master) >> 7;
            StereoFrame::new(lo.clamp(-0x8000, 0x7FFF) as i16, ro.clamp(-0x8000, 0x7FFF) as i16)
        };
        self.out.push(frame);
    }
}

impl SaveState for Spu {
    fn save_state(&self, w: &mut StateWriter) {
        for c in &self.ch {
            for v in [c.cnt, c.sad, c.len, c.counter, c.pos as u32] {
                w.u32(v);
            }
            for v in [c.tmr, c.pnt, c.sample as u16, c.adpcm.0 as u16, c.adpcm_loop.0 as u16, c.lfsr] {
                w.u16(v);
            }
            for v in [c.adpcm.1, c.adpcm_loop.1, c.psg_step] {
                w.u8(v);
            }
        }
        w.u16(self.soundcnt);
        w.u16(self.soundbias);
        for c in &self.cap {
            w.u8(c.cnt);
            w.u32(c.dad);
            w.u16(c.len);
            w.u32(c.counter);
            w.u32(c.pos);
        }
    }

    fn load_state(&mut self, r: &mut StateReader<'_>) -> StateResult<()> {
        for c in &mut self.ch {
            c.cnt = r.u32()?;
            c.sad = r.u32()?;
            c.len = r.u32()?;
            c.counter = r.u32()?;
            c.pos = r.u32()? as i32;
            c.tmr = r.u16()?;
            c.pnt = r.u16()?;
            c.sample = r.u16()? as i16;
            c.adpcm.0 = r.u16()? as i16;
            c.adpcm_loop.0 = r.u16()? as i16;
            c.lfsr = r.u16()?;
            c.adpcm.1 = r.u8()?;
            c.adpcm_loop.1 = r.u8()?;
            c.psg_step = r.u8()?;
        }
        self.soundcnt = r.u16()?;
        self.soundbias = r.u16()?;
        for c in &mut self.cap {
            c.cnt = r.u8()?;
            c.dad = r.u32()?;
            c.len = r.u16()?;
            c.counter = r.u32()?;
            c.pos = r.u32()?;
        }
        self.out.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Mem {
        main: Vec<u8>,
        wram: Vec<u8>,
        wram7: Vec<u8>,
    }

    impl Mem {
        fn new() -> Self {
            Self { main: vec![0; 0x40_0000], wram: vec![0; 0x8000], wram7: vec![0; 0x10000] }
        }
        fn view(&mut self) -> SpuMem<'_> {
            SpuMem { main_ram: &mut self.main, wram: &mut self.wram, wram7: &mut self.wram7, wramcnt: 3 }
        }
    }

    fn start(spu: &mut Spu, ch: usize, cnt: u32, sad: u32, tmr: u16, pnt: u16, len: u32) {
        let base = ch as u32 * 16;
        spu.write16(base + 4, sad as u16);
        spu.write16(base + 6, (sad >> 16) as u16);
        spu.write16(base + 8, tmr);
        spu.write16(base + 0xA, pnt);
        spu.write16(base + 0xC, len as u16);
        spu.write16(base + 0xE, (len >> 16) as u16);
        spu.write16(base, cnt as u16);
        spu.write16(base + 2, (cnt >> 16) as u16);
    }

    #[test]
    fn adpcm_matches_reference_steps() {
        let (mut v, mut i) = (0i16, 0u8);
        adpcm_step(&mut v, &mut i, 0x7); // step 7: 7/8 + 7/4 + 7/2 + 7
        assert_eq!((v, i), (1 + 3 + 7, 8)); // 7/8 = 0, 7/4 = 1, 7/2 = 3
        adpcm_step(&mut v, &mut i, 0x4);
        assert_eq!(i, 10);
        let (mut v, mut i) = (0x7FF0i16, 88u8);
        adpcm_step(&mut v, &mut i, 0x7);
        assert_eq!((v, i), (0x7FFF, 88), "clamped high");
        let (mut v, mut i) = (-0x7FF0i16, 88u8);
        adpcm_step(&mut v, &mut i, 0xF);
        assert_eq!(v, -0x7FFF, "clamped low");
    }

    #[test]
    fn pcm16_one_shot_plays_then_stops() {
        let mut mem = Mem::new();
        for i in 0..8u32 {
            let s = (1000 * (i as i32 + 1)) as i16;
            mem.main[0x1000 + i as usize * 2..][..2].copy_from_slice(&s.to_le_bytes());
        }
        let mut spu = Spu::new();
        spu.write16(0x100, 0x807F); // master on, volume 127
                                    // PCM16, one-shot, centre pan, full volume; one sample per 512 ticks.
        start(&mut spu, 0, 0x8000_0000 | 2 << 27 | 1 << 29 | 64 << 16 | 127, 0x0200_1000, 0xFE00, 0, 4);
        let mut m = mem.view();
        for _ in 0..12 {
            spu.tick(&mut m);
        }
        let left: Vec<i16> = spu.out.iter().map(|f| f.left).collect();
        assert!(left[0] > 0 && left[7] > left[0], "{left:?}");
        assert!(!spu.ch[0].running(), "one-shot channel stops after 8 samples");
        assert_eq!(left[10], 0);
        assert_eq!(spu.read16(2) & 0x8000, 0, "status bit cleared");
    }

    #[test]
    fn looping_pcm8_wraps_to_loop_start() {
        let mut mem = Mem::new();
        mem.main[0x2000..0x2008].copy_from_slice(&[10, 20, 30, 40, 50, 60, 70, 80]);
        let mut spu = Spu::new();
        spu.write16(0x100, 0x807F);
        // loop start = 1 word (sample 4), length 1 word → plays 0..8 then 4..8 forever.
        start(&mut spu, 2, 0x8000_0000 | 1 << 27 | 127 | 127 << 16, 0x0200_2000, 0xFE00, 1, 1);
        let mut m = mem.view();
        for _ in 0..16 {
            spu.tick(&mut m);
        }
        let s: Vec<i16> = spu.ch.iter().map(|c| c.sample).collect();
        assert!(spu.ch[2].running());
        assert!(s[2] >= 50 << 8, "position after wrap stays inside the loop: {}", s[2] >> 8);
    }

    #[test]
    fn psg_and_noise_toggle() {
        let mut mem = Mem::new();
        let mut spu = Spu::new();
        spu.write16(0x100, 0x807F);
        start(&mut spu, 8, 0x8000_0000 | 3 << 29 | 3 << 24 | 127 | 64 << 16, 0, 0xFE00, 0, 0);
        start(&mut spu, 14, 0x8000_0000 | 3 << 29 | 127 | 64 << 16, 0, 0xFE00, 0, 0);
        let mut m = mem.view();
        let mut highs = 0;
        let mut noise = std::collections::HashSet::new();
        for _ in 0..64 {
            spu.tick(&mut m);
            highs += (spu.ch[8].sample > 0) as u32;
            noise.insert(spu.ch[14].sample);
        }
        assert_eq!(highs, 64 * 4 / 8, "duty 3 = 50% high");
        assert_eq!(noise.len(), 2, "noise alternates between ±0x7FFF");
    }

    #[test]
    fn adpcm_channel_reads_header_and_decodes() {
        let mut mem = Mem::new();
        // header: initial value 100, index 10; then nibbles 0x7 (up), 0xF (down).
        mem.main[0x3000..0x3004].copy_from_slice(&(100u32 | 10 << 16).to_le_bytes());
        mem.main[0x3004] = 0xF7;
        let mut spu = Spu::new();
        spu.write16(0x100, 0x807F);
        start(&mut spu, 0, 0x8000_0000 | 2 << 27 | 2 << 29 | 127 | 64 << 16, 0x0200_3000, 0xFE00, 1, 1);
        let mut m = mem.view();
        spu.tick(&mut m);
        let (mut v, mut i) = (100i16, 10u8);
        adpcm_step(&mut v, &mut i, 7);
        assert_eq!(spu.ch[0].sample, v);
        spu.tick(&mut m);
        adpcm_step(&mut v, &mut i, 0xF);
        assert_eq!(spu.ch[0].sample, v);
    }

    #[test]
    fn capture_records_mixer_into_memory() {
        let mut mem = Mem::new();
        for i in 0..4 {
            mem.main[0x4000 + i * 2..][..2].copy_from_slice(&8000i16.to_le_bytes());
        }
        let mut spu = Spu::new();
        spu.write16(0x100, 0x807F);
        start(&mut spu, 0, 0x8000_0000 | 1 << 27 | 1 << 29 | 127, 0x0200_4000, 0xFE00, 0, 2);
        spu.write16(0x114, 2); // capture 0: 2 words
        spu.write16(0x110, 0x5000);
        spu.write16(0x112, 0x0200);
        spu.ch[1].tmr = 0xFE00;
        spu.write16(0x108, 0x80 | 4); // start, one-shot, PCM16, mixer source
        let mut m = mem.view();
        for _ in 0..8 {
            spu.tick(&mut m);
        }
        let captured = i16::from_le_bytes([mem.main[0x5002], mem.main[0x5003]]);
        assert!(captured > 7000, "left mixer (pan 0 = full left) captured: {captured}");
        assert_eq!(spu.cap[0].cnt & 0x80, 0, "one-shot capture finished");
    }

    #[test]
    fn master_disable_outputs_silence_and_state_round_trips() {
        let mut mem = Mem::new();
        let mut spu = Spu::new();
        start(&mut spu, 8, 0x8000_0000 | 3 << 29 | 127, 0, 0xFE00, 0, 0);
        let mut m = mem.view();
        spu.tick(&mut m);
        assert_eq!(spu.out[0], StereoFrame::SILENCE);
        let mut w = StateWriter::new();
        spu.save_state(&mut w);
        let blob = w.finish(2, *b"TEST00");
        let (_, mut r) = StateReader::open(&blob, 2).unwrap();
        let mut copy = Spu::new();
        copy.load_state(&mut r).unwrap();
        assert_eq!(copy.ch, spu.ch);
    }
}
