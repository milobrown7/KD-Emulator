//! Nintendo DSP-ADPCM (4-bit, 8-byte frames of 14 samples) decoder used by
//! AX voices and the DSP accelerator.

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AdpcmState {
    /// Eight predictor coefficient pairs (signed 5.11 fixed point).
    pub coefs: [[i16; 2]; 8],
    pub pred_scale: u16,
    pub yn1: i16,
    pub yn2: i16,
}

impl AdpcmState {
    /// Decode one sample from a 4-bit nibble with the current header.
    #[inline]
    pub fn decode_nibble(&mut self, nibble: u8) -> i16 {
        let pred = ((self.pred_scale >> 4) & 7) as usize;
        let scale = 1i32 << (self.pred_scale & 0xF);
        let n = ((nibble as i32) << 28) >> 28; // sign-extend 4 bits
        let [c1, c2] = self.coefs[pred];
        let s = (n * scale) << 11;
        let s = (s + c1 as i32 * self.yn1 as i32 + c2 as i32 * self.yn2 as i32 + 1024) >> 11;
        let s = s.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        self.yn2 = self.yn1;
        self.yn1 = s;
        s
    }

    /// Decode whole 8-byte frames into PCM.
    pub fn decode_frames(&mut self, data: &[u8], out: &mut Vec<i16>) {
        for frame in data.chunks_exact(8) {
            self.pred_scale = frame[0] as u16;
            for &b in &frame[1..] {
                out.push(self.decode_nibble(b >> 4));
                out.push(self.decode_nibble(b & 0xF));
            }
        }
    }
}

/// ADPCM sample addressing: addresses count nibbles; every 16th nibble
/// position is a frame header.
#[inline]
pub fn is_header_nibble(addr: u32) -> bool {
    addr & 0xF == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_predictor_scales_nibbles() {
        let mut s = AdpcmState::default();
        let mut out = vec![];
        // header: predictor 0, scale 2^3; nibbles 1, -1, 7, -8 ...
        s.decode_frames(&[0x03, 0x1F, 0x78, 0, 0, 0, 0, 0], &mut out);
        assert_eq!(out.len(), 14);
        assert_eq!(&out[..4], &[8, -8, 56, -64]);
    }

    #[test]
    fn predictor_uses_history() {
        let mut s = AdpcmState { coefs: [[2048, 0]; 8], ..Default::default() }; // y = x + 1.0*yn1
        let mut out = vec![];
        s.decode_frames(&[0x00, 0x11, 0x11, 0, 0, 0, 0, 0], &mut out);
        assert_eq!(&out[..4], &[1, 2, 3, 4], "accumulates");
        assert_eq!(out[13], 4);
    }

    #[test]
    fn header_positions() {
        assert!(is_header_nibble(0x20));
        assert!(!is_header_nibble(0x22));
    }
}
