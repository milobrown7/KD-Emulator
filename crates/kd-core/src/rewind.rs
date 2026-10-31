//! Rewind history.
//!
//! The newest snapshot is kept whole. Every older snapshot is stored as a
//! *reverse* delta: the XOR against the snapshot that followed it, with runs
//! of zero bytes collapsed. Consecutive emulator states differ in a few
//! kilobytes, so a 4 MiB DS state usually costs tens of kilobytes.
//! Because each delta depends only on its newer neighbour, the oldest entry
//! can be dropped at any time to stay inside the byte budget.

use std::collections::VecDeque;

pub struct RewindBuffer {
    latest: Option<Vec<u8>>,
    older: VecDeque<Vec<u8>>,
    bytes: usize,
    budget: usize,
}

const DELTA: u8 = 0;
const FULL: u8 = 1;

fn put_varint(out: &mut Vec<u8>, mut v: usize) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn get_varint(data: &[u8], at: &mut usize) -> Option<usize> {
    let mut v = 0usize;
    let mut shift = 0;
    loop {
        let b = *data.get(*at)?;
        *at += 1;
        v |= ((b & 0x7F) as usize) << shift;
        if b & 0x80 == 0 {
            return Some(v);
        }
        shift += 7;
        if shift > 56 {
            return None;
        }
    }
}

/// Encode `old` relative to `new`.
pub fn encode(old: &[u8], new: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    if old.len() != new.len() {
        out.push(FULL);
        out.extend_from_slice(old);
        return out;
    }
    out.push(DELTA);
    let mut i = 0;
    let n = old.len();
    while i < n {
        let zs = i;
        while i < n && old[i] == new[i] {
            i += 1;
        }
        let zeros = i - zs;
        let ls = i;
        // A literal run ends at the next stretch of 8+ equal bytes.
        while i < n {
            if old[i] == new[i] {
                let mut j = i;
                while j < n && j - i < 8 && old[j] == new[j] {
                    j += 1;
                }
                if j - i >= 8 || j == n {
                    break;
                }
                i = j;
            } else {
                i += 1;
            }
        }
        put_varint(&mut out, zeros);
        put_varint(&mut out, i - ls);
        out.extend(old[ls..i].iter().zip(&new[ls..i]).map(|(a, b)| a ^ b));
    }
    out
}

/// Rebuild the older snapshot from its delta and the newer snapshot.
pub fn decode(delta: &[u8], new: &[u8]) -> Option<Vec<u8>> {
    match *delta.first()? {
        FULL => Some(delta[1..].to_vec()),
        DELTA => {
            let mut out = new.to_vec();
            let mut at = 1;
            let mut pos = 0usize;
            while at < delta.len() {
                pos += get_varint(delta, &mut at)?;
                let lit = get_varint(delta, &mut at)?;
                let bytes = delta.get(at..at + lit)?;
                for (k, x) in bytes.iter().enumerate() {
                    *out.get_mut(pos + k)? ^= x;
                }
                at += lit;
                pos += lit;
            }
            Some(out)
        }
        _ => None,
    }
}

impl RewindBuffer {
    /// `budget` caps the memory used by older snapshots, in bytes.
    pub fn new(budget: usize) -> Self {
        Self { latest: None, older: VecDeque::new(), bytes: 0, budget }
    }

    /// Record a new snapshot.
    pub fn push(&mut self, state: Vec<u8>) {
        if let Some(prev) = self.latest.take() {
            let d = encode(&prev, &state);
            self.bytes += d.len();
            self.older.push_back(d);
        }
        self.latest = Some(state);
        while self.bytes > self.budget {
            match self.older.pop_front() {
                Some(d) => self.bytes -= d.len(),
                None => break,
            }
        }
    }

    /// Take the newest snapshot, making the one before it the newest.
    pub fn pop(&mut self) -> Option<Vec<u8>> {
        let cur = self.latest.take()?;
        if let Some(d) = self.older.pop_back() {
            self.bytes -= d.len();
            self.latest = decode(&d, &cur);
            if self.latest.is_none() {
                self.clear();
            }
        }
        Some(cur)
    }

    /// Number of snapshots held.
    pub fn len(&self) -> usize {
        self.older.len() + self.latest.is_some() as usize
    }

    pub fn is_empty(&self) -> bool {
        self.latest.is_none()
    }

    /// Bytes used by the encoded history (excluding the newest snapshot).
    pub fn history_bytes(&self) -> usize {
        self.bytes
    }

    pub fn clear(&mut self) {
        self.latest = None;
        self.older.clear();
        self.bytes = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(seed: u32, len: usize) -> Vec<u8> {
        let mut v = vec![0u8; len];
        // Mostly stable memory with a few bytes changing per frame.
        for (i, b) in v.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        for k in 0..16 {
            let at = (seed as usize * 977 + k * 4099) % len;
            v[at] = (seed + k as u32) as u8;
        }
        v
    }

    #[test]
    fn encode_decode_round_trip() {
        let a = state(1, 100_000);
        let b = state(2, 100_000);
        let d = encode(&a, &b);
        assert!(d.len() < 400, "delta {} bytes", d.len());
        assert_eq!(decode(&d, &b).unwrap(), a);
        let short = vec![7u8; 10];
        assert_eq!(decode(&encode(&short, &b), &b).unwrap(), short, "size change stores the full state");
        assert_eq!(decode(&encode(&b, &b), &b).unwrap(), b);
    }

    #[test]
    fn pops_in_reverse_order() {
        let mut r = RewindBuffer::new(usize::MAX);
        for s in 0..10 {
            r.push(state(s, 50_000));
        }
        assert_eq!(r.len(), 10);
        for s in (0..10).rev() {
            assert_eq!(r.pop().unwrap(), state(s, 50_000), "snapshot {s}");
        }
        assert!(r.pop().is_none());
    }

    #[test]
    fn budget_drops_oldest_first() {
        let mut r = RewindBuffer::new(1500);
        for s in 0..50 {
            r.push(state(s, 50_000));
        }
        assert!(r.history_bytes() <= 1500);
        assert!(r.len() < 50 && r.len() > 1);
        let kept = r.len() as u32;
        let mut last = 0;
        while let Some(s) = r.pop() {
            last += 1;
            assert_eq!(s, state(50 - last, 50_000));
        }
        assert_eq!(last, kept);
    }

    #[test]
    fn corrupt_delta_is_rejected() {
        assert!(decode(&[DELTA, 0xFF], &[0; 4]).is_none());
        assert!(decode(&[9], &[0; 4]).is_none());
        assert!(decode(&[], &[0; 4]).is_none());
    }
}
