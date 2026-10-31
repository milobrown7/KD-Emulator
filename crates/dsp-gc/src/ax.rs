//! High-level emulation of the AX audio microcode: walks the parameter
//! block (PB) list in main memory, decodes each running voice (PCM8,
//! PCM16 or DSP-ADPCM from ARAM), resamples it and mixes it into the
//! main stereo bus, then writes the voice state back.
//!
//! PB layout (big-endian 16-bit words), following the AX `AXPB` field
//! order:
//!
//! | word | field |
//! |------|-------|
//! | 0x00-01 | next PB address |
//! | 0x02-03 | this PB address |
//! | 0x07 | running |
//! | 0x09 / 0x0B | left / right volume (1.15) |
//! | 0x36 | envelope volume (1.15) |
//! | 0x3B | looping |
//! | 0x3C | sample format (0 ADPCM, 0x0A PCM16, 0x19 PCM8) |
//! | 0x3D-3E / 0x3F-40 / 0x41-42 | loop / end / current address |
//! | 0x43-52 | ADPCM coefficients |
//! | 0x54 / 0x55 / 0x56 | pred_scale / yn1 / yn2 |
//! | 0x57-58 | resampling ratio (16.16) |
//! | 0x59 | current address fraction |

use crate::adpcm::{is_header_nibble, AdpcmState};

pub const SAMPLES_PER_FRAME: usize = 160; // 5 ms at 32 kHz
pub const FORMAT_ADPCM: u16 = 0x00;
pub const FORMAT_PCM16: u16 = 0x0A;
pub const FORMAT_PCM8: u16 = 0x19;

pub const PB_WORDS: usize = 0x60;

/// Memory the AX mixer reads from.
pub trait AxMemory {
    fn read_main16(&self, addr: u32) -> u16;
    fn write_main16(&mut self, addr: u32, v: u16);
    /// ARAM byte access (sample data).
    fn read_aram8(&self, addr: u32) -> u8;
}

#[derive(Debug, Clone, PartialEq)]
pub struct Voice {
    pub pb: [u16; PB_WORDS],
}

impl Default for Voice {
    fn default() -> Self {
        Self { pb: [0; PB_WORDS] }
    }
}

impl Voice {
    fn word(&self, i: usize) -> u16 {
        self.pb[i]
    }
    fn dword(&self, i: usize) -> u32 {
        (self.pb[i] as u32) << 16 | self.pb[i + 1] as u32
    }
    fn set_dword(&mut self, i: usize, v: u32) {
        self.pb[i] = (v >> 16) as u16;
        self.pb[i + 1] = v as u16;
    }

    pub fn load(mem: &dyn AxMemory, addr: u32) -> Self {
        let mut pb = [0u16; PB_WORDS];
        for (i, w) in pb.iter_mut().enumerate() {
            *w = mem.read_main16(addr + i as u32 * 2);
        }
        Self { pb }
    }

    pub fn store(&self, mem: &mut dyn AxMemory, addr: u32) {
        for (i, &w) in self.pb.iter().enumerate() {
            mem.write_main16(addr + i as u32 * 2, w);
        }
    }

    pub fn running(&self) -> bool {
        self.word(0x07) != 0
    }

    fn adpcm_state(&self) -> AdpcmState {
        let mut coefs = [[0i16; 2]; 8];
        for (i, c) in coefs.iter_mut().enumerate() {
            *c = [self.pb[0x43 + i * 2] as i16, self.pb[0x44 + i * 2] as i16];
        }
        AdpcmState { coefs, pred_scale: self.pb[0x54], yn1: self.pb[0x55] as i16, yn2: self.pb[0x56] as i16 }
    }

    /// Render `n` output samples, advancing the voice. Returns mono PCM.
    pub fn render(&mut self, mem: &dyn AxMemory, n: usize) -> Vec<i16> {
        let mut out = Vec::with_capacity(n);
        if !self.running() {
            out.resize(n, 0);
            return out;
        }
        let format = self.word(0x3C);
        let looping = self.word(0x3B) != 0;
        let loop_addr = self.dword(0x3D);
        let end_addr = self.dword(0x3F);
        let mut cur = self.dword(0x41);
        let ratio = self.dword(0x57).max(1);
        let mut frac = self.word(0x59) as u32;
        let mut adpcm = self.adpcm_state();
        let mut sample = self.pb[0x5A] as i16; // last decoded sample
                                               // Fetch one source sample at `cur`, advancing it.
        let fetch = |cur: &mut u32, adpcm: &mut AdpcmState| -> i16 {
            match format {
                FORMAT_PCM16 => {
                    let a = *cur * 2;
                    let v = i16::from_be_bytes([mem.read_aram8(a), mem.read_aram8(a + 1)]);
                    *cur += 1;
                    v
                }
                FORMAT_PCM8 => {
                    let v = (mem.read_aram8(*cur) as i8 as i16) << 8;
                    *cur += 1;
                    v
                }
                _ => {
                    if is_header_nibble(*cur) {
                        adpcm.pred_scale = mem.read_aram8(*cur / 2) as u16;
                        *cur += 2;
                    }
                    let byte = mem.read_aram8(*cur / 2);
                    let nib = if *cur & 1 == 0 { byte >> 4 } else { byte & 0xF };
                    *cur += 1;
                    adpcm.decode_nibble(nib)
                }
            }
        };
        let mut running = true;
        for _ in 0..n {
            frac += ratio;
            while frac >= 0x1_0000 && running {
                frac -= 0x1_0000;
                sample = fetch(&mut cur, &mut adpcm);
                if cur > end_addr {
                    if looping {
                        cur = loop_addr;
                    } else {
                        running = false;
                    }
                }
            }
            out.push(if running || frac < 0x1_0000 { sample } else { 0 });
        }
        if !running {
            self.pb[0x07] = 0;
        }
        self.set_dword(0x41, cur);
        self.pb[0x59] = frac as u16;
        self.pb[0x54] = adpcm.pred_scale;
        self.pb[0x55] = adpcm.yn1 as u16;
        self.pb[0x56] = adpcm.yn2 as u16;
        self.pb[0x5A] = sample as u16;
        out
    }
}

/// Mix every voice in the PB list starting at `first_pb` into one 5 ms
/// stereo frame (interleaved L/R).
pub fn mix_frame(mem: &mut dyn AxMemory, first_pb: u32) -> Vec<[i16; 2]> {
    let mut acc = vec![[0i32; 2]; SAMPLES_PER_FRAME];
    let mut addr = first_pb;
    let mut guard = 0;
    while addr != 0 && guard < 128 {
        guard += 1;
        let mut v = Voice::load(mem, addr);
        let next = v.dword(0x00);
        if v.running() {
            let samples = v.render(mem, SAMPLES_PER_FRAME);
            let env = v.word(0x36) as i32;
            let (vl, vr) = (v.word(0x09) as i32, v.word(0x0B) as i32);
            for (a, &s) in acc.iter_mut().zip(&samples) {
                let s = (s as i32 * env) >> 15;
                a[0] += (s * vl) >> 15;
                a[1] += (s * vr) >> 15;
            }
            v.store(mem, addr);
        }
        addr = next;
    }
    acc.iter()
        .map(|a| {
            [a[0].clamp(i16::MIN as i32, i16::MAX as i32) as i16, a[1].clamp(i16::MIN as i32, i16::MAX as i32) as i16]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Mem {
        main: Vec<u8>,
        aram: Vec<u8>,
    }

    impl AxMemory for Mem {
        fn read_main16(&self, a: u32) -> u16 {
            u16::from_be_bytes([self.main[a as usize], self.main[a as usize + 1]])
        }
        fn write_main16(&mut self, a: u32, v: u16) {
            self.main[a as usize..a as usize + 2].copy_from_slice(&v.to_be_bytes());
        }
        fn read_aram8(&self, a: u32) -> u8 {
            self.aram[a as usize]
        }
    }

    fn pb(mem: &mut Mem, addr: u32, next: u32, fmt: u16, start: u32, end: u32, looping: bool) {
        let mut v = Voice::default();
        v.set_dword(0x00, next);
        v.set_dword(0x02, addr);
        v.pb[0x07] = 1;
        v.pb[0x09] = 0x8000; // 1.0
        v.pb[0x0B] = 0x4000; // 0.5
        v.pb[0x36] = 0x8000;
        v.pb[0x3B] = looping as u16;
        v.pb[0x3C] = fmt;
        v.set_dword(0x3D, start);
        v.set_dword(0x3F, end);
        v.set_dword(0x41, start);
        v.set_dword(0x57, 0x1_0000);
        v.store(mem, addr);
    }

    #[test]
    fn pcm16_voice_mixes_with_pan() {
        let mut mem = Mem { main: vec![0; 0x10000], aram: vec![0; 0x10000] };
        for i in 0..400u32 {
            let s = ((i as i16) * 50).to_be_bytes();
            mem.aram[(0x100 + i * 2) as usize..(0x102 + i * 2) as usize].copy_from_slice(&s);
        }
        pb(&mut mem, 0x1000, 0, FORMAT_PCM16, 0x80, 0x80 + 399, false);
        let frame = mix_frame(&mut mem, 0x1000);
        assert_eq!(frame.len(), SAMPLES_PER_FRAME);
        assert_eq!(frame[2], [100, 50], "full volume left, half volume right");
        let v = Voice::load(&mem, 0x1000);
        assert_eq!(v.dword(0x41), 0x80 + 160, "current address advanced by one frame");
    }

    #[test]
    fn one_shot_voice_stops_at_end_and_looping_voice_wraps() {
        let mut mem = Mem { main: vec![0; 0x10000], aram: vec![0x40; 0x10000] };
        pb(&mut mem, 0x1000, 0x2000, FORMAT_PCM8, 0x0, 99, false);
        pb(&mut mem, 0x2000, 0, FORMAT_PCM8, 0x0, 99, true);
        mix_frame(&mut mem, 0x1000);
        assert!(!Voice::load(&mem, 0x1000).running(), "one-shot ended");
        let looped = Voice::load(&mem, 0x2000);
        assert!(looped.running());
        assert_eq!(looped.dword(0x41), 160 - 100, "wrapped to the loop start");
    }

    #[test]
    fn adpcm_voice_decodes_frames() {
        let mut mem = Mem { main: vec![0; 0x10000], aram: vec![0; 0x1000] };
        // Frames with predictor 0 / scale 2^4 and nibbles of 1 → 16 each.
        for f in 0..40 {
            let base = 0x200 + f * 8;
            mem.aram[base] = 0x04;
            mem.aram[base + 1..base + 8].fill(0x11);
        }
        let start = 0x200 * 2 + 2; // nibble address after the first header
        pb(&mut mem, 0x1000, 0, FORMAT_ADPCM, start, 0x200 * 2 + 40 * 16 - 1, false);
        mem.write_main16(0x1000 + 0x54 * 2, 0x04); // header of the first frame
        let frame = mix_frame(&mut mem, 0x1000);
        assert!(frame[1..].iter().all(|s| s[0] == 16), "{:?}", &frame[..8]);
    }
}
