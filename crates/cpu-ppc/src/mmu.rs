//! Address translation: Block Address Translation (4 IBAT/DBAT pairs on
//! Gekko, 8 on Broadway when HID4[SBE] is set), hashed page tables via
//! SDR1 and the segment registers, and a two-level 4 KiB software TLB.
//!
//! The software TLB is what lets 4 KiB guest pages coexist with 16 KiB host
//! pages: guest protections and remaps are resolved here at 4 KiB
//! granularity, so the host never has to `mprotect` a sub-page range.

/// Two-level lookup table indexed by `ea >> 12`. The first level has 1024
/// entries (4 MiB each), allocated on demand; each second-level table maps
/// 1024 pages of 4 KiB.
#[derive(Clone, Default)]
pub struct SoftTlb {
    top: Vec<Option<Box<[u32; 1024]>>>,
    pub hits: u64,
    pub misses: u64,
}

const VALID: u32 = 1;

impl SoftTlb {
    pub fn new() -> Self {
        Self { top: (0..1024).map(|_| None).collect(), hits: 0, misses: 0 }
    }

    #[inline]
    pub fn lookup(&mut self, ea: u32) -> Option<u32> {
        let e = self.top[(ea >> 22) as usize].as_ref()?[((ea >> 12) & 0x3FF) as usize];
        if e & VALID != 0 {
            self.hits += 1;
            Some((e & !0xFFF) | (ea & 0xFFF))
        } else {
            None
        }
    }

    pub fn insert(&mut self, ea: u32, pa: u32) {
        self.misses += 1;
        let l2 = self.top[(ea >> 22) as usize].get_or_insert_with(|| Box::new([0u32; 1024]));
        l2[((ea >> 12) & 0x3FF) as usize] = (pa & !0xFFF) | VALID;
    }

    /// Drop one 4 KiB page (`tlbie`).
    pub fn invalidate_page(&mut self, ea: u32) {
        if let Some(l2) = self.top[(ea >> 22) as usize].as_mut() {
            l2[((ea >> 12) & 0x3FF) as usize] = 0;
        }
    }

    /// Drop everything (BAT, segment register, SDR1 or MSR changes).
    pub fn flush(&mut self) {
        for t in &mut self.top {
            *t = None;
        }
    }
}

impl std::fmt::Debug for SoftTlb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let pages = self.top.iter().flatten().count();
        write!(f, "SoftTlb {{ l2_tables: {pages}, hits: {}, misses: {} }}", self.hits, self.misses)
    }
}

/// One BAT pair (upper, lower).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Bat {
    pub upper: u32,
    pub lower: u32,
}

impl Bat {
    /// Translate `ea` through this BAT if it matches.
    #[inline]
    pub fn translate(&self, ea: u32, supervisor: bool) -> Option<u32> {
        let valid = if supervisor { self.upper & 2 != 0 } else { self.upper & 1 != 0 };
        if !valid {
            return None;
        }
        let bl = (self.upper >> 2) & 0x7FF;
        let mask = !(bl << 17) & 0xFFFE_0000;
        let bepi = self.upper & 0xFFFE_0000;
        if ea & mask != bepi & mask {
            return None;
        }
        let brpn = self.lower & 0xFFFE_0000;
        Some((brpn & mask) | (ea & !mask))
    }
}

/// A page table entry hit: physical address and where its R/C bits live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PteHit {
    pub pa: u32,
    pub pte_addr: u32,
    pub word1: u32,
}

/// Hashed page table search per the PowerPC 750 user manual. `read32`
/// reads physical memory.
pub fn page_table_walk(ea: u32, sr: u32, sdr1: u32, mut read32: impl FnMut(u32) -> u32) -> Option<PteHit> {
    if sr & 0x8000_0000 != 0 {
        return None; // direct-store segment
    }
    let vsid = sr & 0x00FF_FFFF;
    let page_index = (ea >> 12) & 0xFFFF;
    let api = page_index >> 10;
    let htabmask = sdr1 & 0x1FF;
    let hash1 = (vsid & 0x7FFFF) ^ page_index;
    for (secondary, hash) in [(false, hash1), (true, !hash1)] {
        // HTABORG[0:6] || (HTABORG[7:15] | (hash[0:8] & HTABMASK)) || hash[9:18] || 0b000000
        let mid = ((sdr1 >> 16) & 0x1FF) | ((hash >> 10) & htabmask);
        let pteg = (sdr1 & 0xFE00_0000) | (mid << 16) | ((hash & 0x3FF) << 6);
        for i in 0..8 {
            let addr = pteg + i * 8;
            let w0 = read32(addr);
            if w0 & 0x8000_0000 == 0 {
                continue;
            }
            if (w0 >> 7) & 0xFF_FFFF != vsid || ((w0 >> 6) & 1 != 0) != secondary || w0 & 0x3F != api {
                continue;
            }
            let w1 = read32(addr + 4);
            return Some(PteHit { pa: (w1 & 0xFFFF_F000) | (ea & 0xFFF), pte_addr: addr, word1: w1 });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soft_tlb_two_levels() {
        let mut t = SoftTlb::new();
        assert_eq!(t.lookup(0x8000_1234), None);
        t.insert(0x8000_1000, 0x0000_1000);
        assert_eq!(t.lookup(0x8000_1234), Some(0x0000_1234));
        assert_eq!(t.lookup(0x8000_2234), None, "4 KiB granularity");
        t.invalidate_page(0x8000_1FFF);
        assert_eq!(t.lookup(0x8000_1234), None);
        t.insert(0xC000_0000, 0);
        t.flush();
        assert_eq!(t.lookup(0xC000_0010), None);
    }

    #[test]
    fn gamecube_standard_bats() {
        // DBAT0: 0x80000000 → 0x00000000, 256 MiB, supervisor valid (cached)
        let cached = Bat { upper: 0x8000_1FFE, lower: 0x0000_0002 };
        // DBAT1: 0xC0000000 → 0x00000000, 256 MiB, cache-inhibited
        let uncached = Bat { upper: 0xC000_1FFE, lower: 0x0000_002A };
        assert_eq!(cached.translate(0x8000_3100, true), Some(0x0000_3100));
        assert_eq!(uncached.translate(0xCC00_6000, true), Some(0x0C00_6000));
        assert_eq!(cached.translate(0xC000_0000, true), None);
        assert_eq!(cached.translate(0x8000_0000, false), None, "user mode needs Vp");
        // 128 KiB locked-cache BAT at 0xE0000000
        let lc = Bat { upper: 0xE000_0002, lower: 0xE000_0002 };
        assert_eq!(lc.translate(0xE000_1234, true), Some(0xE000_1234));
        assert_eq!(lc.translate(0xE002_0000, true), None);
    }

    #[test]
    fn page_table_lookup() {
        let mut mem = vec![0u32; 0x20000 / 4];
        let sdr1 = 0x0001_0000; // HTABORG 0x10000, mask 0 (64 KiB table)
        let sr = 0x0000_0123; // VSID 0x123
        let ea = 0x1234_5678u32;
        let page_index = (ea >> 12) & 0xFFFF;
        let hash = (0x123 ^ page_index) & 0x7FFFF;
        let pteg = 0x10000 | ((hash & 0x3FF) << 6);
        let w0 = 0x8000_0000 | (0x123 << 7) | (page_index >> 10);
        let w1 = 0x00AB_C000 | 0x2;
        mem[(pteg / 4) as usize + 2] = w0;
        mem[(pteg / 4) as usize + 3] = w1;
        let hit = page_table_walk(ea, sr, sdr1, |a| mem[(a / 4) as usize]).unwrap();
        assert_eq!(hit.pa, 0x00AB_C678);
        assert_eq!(hit.pte_addr, pteg + 8);
        assert!(page_table_walk(0x1235_5678, sr, sdr1, |a| mem[(a / 4) as usize]).is_none());
    }
}
