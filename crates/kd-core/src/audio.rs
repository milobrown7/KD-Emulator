//! Lock-free audio transport between the emulation thread and the AAudio
//! callback thread, plus a streaming resampler from a core's native rate
//! (GBA 32768 Hz, NDS 32728 Hz) to the device rate.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// One interleaved stereo sample.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StereoFrame {
    pub left: i16,
    pub right: i16,
}

impl StereoFrame {
    pub const SILENCE: Self = Self { left: 0, right: 0 };
    pub const fn new(left: i16, right: i16) -> Self {
        Self { left, right }
    }
}

struct Shared {
    buf: Box<[UnsafeCell<StereoFrame>]>,
    mask: usize,
    /// Total frames ever written (owned by the producer).
    head: AtomicUsize,
    /// Total frames ever read (owned by the consumer).
    tail: AtomicUsize,
}

// SAFETY: producer only writes slots in [tail+cap .. head) range it owns, the
// consumer only reads slots in [tail .. head). Indices are published with
// Release/Acquire so slot contents are visible before the index moves.
unsafe impl Sync for Shared {}
unsafe impl Send for Shared {}

/// Create a single-producer / single-consumer ring holding at least
/// `min_frames` frames (rounded up to a power of two).
pub fn audio_ring(min_frames: usize) -> (AudioProducer, AudioConsumer) {
    let cap = min_frames.max(2).next_power_of_two();
    let buf = (0..cap).map(|_| UnsafeCell::new(StereoFrame::SILENCE)).collect::<Vec<_>>().into_boxed_slice();
    let shared = Arc::new(Shared { buf, mask: cap - 1, head: AtomicUsize::new(0), tail: AtomicUsize::new(0) });
    (AudioProducer { shared: shared.clone(), dropped: 0 }, AudioConsumer { shared, underruns: 0 })
}

/// Emulation-thread side.
pub struct AudioProducer {
    shared: Arc<Shared>,
    dropped: u64,
}

impl AudioProducer {
    pub fn capacity(&self) -> usize {
        self.shared.mask + 1
    }

    /// Frames currently queued.
    pub fn len(&self) -> usize {
        let head = self.shared.head.load(Ordering::Relaxed);
        let tail = self.shared.tail.load(Ordering::Acquire);
        head.wrapping_sub(tail)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Push as many frames as fit; returns how many were accepted. Frames
    /// that do not fit are counted as dropped (the core is running fast).
    pub fn push(&mut self, frames: &[StereoFrame]) -> usize {
        let s = &*self.shared;
        let head = s.head.load(Ordering::Relaxed);
        let tail = s.tail.load(Ordering::Acquire);
        let free = self.capacity() - head.wrapping_sub(tail);
        let n = frames.len().min(free);
        for (i, f) in frames[..n].iter().enumerate() {
            // SAFETY: slot head+i is outside the consumer's readable window.
            unsafe { *s.buf[(head + i) & s.mask].get() = *f };
        }
        s.head.store(head.wrapping_add(n), Ordering::Release);
        self.dropped += (frames.len() - n) as u64;
        n
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }
}

/// Audio-callback side.
pub struct AudioConsumer {
    shared: Arc<Shared>,
    underruns: u64,
}

impl AudioConsumer {
    pub fn len(&self) -> usize {
        let head = self.shared.head.load(Ordering::Acquire);
        let tail = self.shared.tail.load(Ordering::Relaxed);
        head.wrapping_sub(tail)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Fill `out` completely. Missing frames repeat the last delivered frame
    /// (avoids clicks) and count as an underrun. Returns frames really read.
    pub fn pop_into(&mut self, out: &mut [StereoFrame]) -> usize {
        let s = &*self.shared;
        let tail = s.tail.load(Ordering::Relaxed);
        let head = s.head.load(Ordering::Acquire);
        let avail = head.wrapping_sub(tail);
        let n = out.len().min(avail);
        for (i, o) in out[..n].iter_mut().enumerate() {
            // SAFETY: slot tail+i was published by the producer.
            *o = unsafe { *s.buf[(tail + i) & s.mask].get() };
        }
        s.tail.store(tail.wrapping_add(n), Ordering::Release);
        if n < out.len() {
            self.underruns += 1;
            let fill = if n > 0 { out[n - 1] } else { StereoFrame::SILENCE };
            out[n..].fill(fill);
        }
        n
    }

    pub fn underruns(&self) -> u64 {
        self.underruns
    }
}

/// Streaming linear-interpolation resampler with a dynamic ratio so the
/// front-end can nudge the rate (±0.5 %) to keep the ring buffer half full
/// instead of dropping or duplicating whole buffers.
#[derive(Debug, Clone)]
pub struct Resampler {
    in_rate: f64,
    out_rate: f64,
    adjust: f64,
    phase: f64,
    prev: StereoFrame,
    have_prev: bool,
}

impl Resampler {
    pub fn new(in_rate: u32, out_rate: u32) -> Self {
        Self {
            in_rate: in_rate as f64,
            out_rate: out_rate as f64,
            adjust: 1.0,
            phase: 0.0,
            prev: StereoFrame::SILENCE,
            have_prev: false,
        }
    }

    pub fn set_input_rate(&mut self, in_rate: u32) {
        self.in_rate = in_rate as f64;
    }

    /// Fine rate correction, clamped to ±0.5 %.
    pub fn set_adjust(&mut self, factor: f64) {
        self.adjust = factor.clamp(0.995, 1.005);
    }

    /// Dynamic-rate control: steer toward a half-full ring.
    pub fn steer(&mut self, queued: usize, capacity: usize) {
        let fill = queued as f64 / capacity.max(1) as f64;
        self.set_adjust(1.0 + (0.5 - fill) * 0.01);
    }

    /// Resample `input`, appending to `out`.
    pub fn process(&mut self, input: &[StereoFrame], out: &mut Vec<StereoFrame>) {
        let step = self.in_rate / (self.out_rate * self.adjust);
        for &cur in input {
            if !self.have_prev {
                self.prev = cur;
                self.have_prev = true;
                continue;
            }
            while self.phase < 1.0 {
                let t = self.phase;
                let l = self.prev.left as f64 + (cur.left as f64 - self.prev.left as f64) * t;
                let r = self.prev.right as f64 + (cur.right as f64 - self.prev.right as f64) * t;
                out.push(StereoFrame::new(l.round() as i16, r.round() as i16));
                self.phase += step;
            }
            self.phase -= 1.0;
            self.prev = cur;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_pop_and_wraparound() {
        let (mut p, mut c) = audio_ring(5);
        assert_eq!(p.capacity(), 8);
        let frames: Vec<_> = (0..6).map(|i| StereoFrame::new(i, -i)).collect();
        assert_eq!(p.push(&frames), 6);
        let mut out = [StereoFrame::SILENCE; 4];
        assert_eq!(c.pop_into(&mut out), 4);
        assert_eq!(out[3], StereoFrame::new(3, -3));
        // wraps around the end of the buffer
        let more: Vec<_> = (6..12).map(|i| StereoFrame::new(i, -i)).collect();
        assert_eq!(p.push(&more), 6);
        assert_eq!(p.push(&more), 0, "ring is full");
        assert_eq!(p.dropped(), 6);
        let mut out = [StereoFrame::SILENCE; 8];
        assert_eq!(c.pop_into(&mut out), 8);
        let lefts: Vec<i16> = out.iter().map(|f| f.left).collect();
        assert_eq!(lefts, vec![4, 5, 6, 7, 8, 9, 10, 11]);
    }

    #[test]
    fn underrun_repeats_last_frame() {
        let (mut p, mut c) = audio_ring(8);
        p.push(&[StereoFrame::new(7, 9)]);
        let mut out = [StereoFrame::SILENCE; 3];
        assert_eq!(c.pop_into(&mut out), 1);
        assert_eq!(out, [StereoFrame::new(7, 9); 3]);
        assert_eq!(c.underruns(), 1);
    }

    #[test]
    fn concurrent_transfer_preserves_order() {
        let (mut p, mut c) = audio_ring(256);
        const N: i32 = 200_000;
        let producer = std::thread::spawn(move || {
            let mut i = 0i32;
            while i < N {
                let f = [StereoFrame::new(i as i16, (i >> 16) as i16)];
                if p.push(&f) == 1 {
                    i += 1;
                } else {
                    std::thread::yield_now();
                }
            }
        });
        let mut expected = 0i32;
        let mut buf = [StereoFrame::SILENCE; 1];
        while expected < N {
            if !c.is_empty() {
                c.pop_into(&mut buf);
                assert_eq!(buf[0], StereoFrame::new(expected as i16, (expected >> 16) as i16));
                expected += 1;
            } else {
                std::thread::yield_now();
            }
        }
        producer.join().unwrap();
    }

    #[test]
    fn resampler_ratio_is_correct() {
        let mut rs = Resampler::new(32768, 48000);
        let input: Vec<_> = (0..32768).map(|i| StereoFrame::new((i % 100) as i16, 0)).collect();
        let mut out = Vec::new();
        rs.process(&input, &mut out);
        let expected = 48000.0 * (32767.0 / 32768.0);
        assert!((out.len() as f64 - expected).abs() < 3.0, "got {}", out.len());
    }

    #[test]
    fn resampler_interpolates_linearly() {
        let mut rs = Resampler::new(1, 2);
        let mut out = Vec::new();
        rs.process(&[StereoFrame::new(0, 0), StereoFrame::new(100, -100), StereoFrame::new(200, -200)], &mut out);
        assert_eq!(out.iter().map(|f| f.left).collect::<Vec<_>>(), vec![0, 50, 100, 150]);
    }
}
