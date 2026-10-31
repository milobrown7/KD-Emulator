//! The nine configurable VRAM banks (A–I) and their mapping into the
//! engine A/B background and sprite windows, LCDC, ARM7 WRAM, 3D texture
//! and texture-palette slots, and the extended palette slots.
//!
//! Each view is a table of pages; a page lists every bank slice mapped
//! there (overlapping banks OR together on read, all receive writes).

use kd_core::{SaveState, StateReader, StateResult, StateWriter};

pub const BANK_SIZES: [usize; 9] = [0x20000, 0x20000, 0x20000, 0x20000, 0x10000, 0x4000, 0x4000, 0x8000, 0x4000];
pub const BANK_OFFSETS: [usize; 9] = [0x00000, 0x20000, 0x40000, 0x60000, 0x80000, 0x90000, 0x94000, 0x98000, 0xA0000];
pub const VRAM_TOTAL: usize = 0xA4000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    ABg,
    BBg,
    AObj,
    BObj,
    Lcdc,
    Arm7,
    Texture,
    TexPalette,
    ABgExtPal,
    BBgExtPal,
    AObjExtPal,
    BObjExtPal,
}

const VIEWS: usize = 12;

#[derive(Debug, Clone)]
struct PageTable {
    shift: u32,
    pages: Vec<Vec<u32>>,
}

impl PageTable {
    fn new(size: usize, page: usize) -> Self {
        Self { shift: page.trailing_zeros(), pages: vec![Vec::new(); size / page] }
    }
    fn clear(&mut self) {
        for p in &mut self.pages {
            p.clear();
        }
    }
    fn map(&mut self, at: usize, bank_offset: usize, len: usize) {
        let page = 1usize << self.shift;
        let n = self.pages.len();
        for k in 0..len / page {
            let idx = (at / page + k) % n;
            self.pages[idx].push((bank_offset + k * page) as u32);
        }
    }
}

pub struct Vram {
    pub data: Box<[u8]>,
    pub cnt: [u8; 9],
    tables: Vec<PageTable>,
}

impl Default for Vram {
    fn default() -> Self {
        Self::new()
    }
}

impl Vram {
    pub fn new() -> Self {
        let k16 = 0x4000;
        let k8 = 0x2000;
        let tables = vec![
            PageTable::new(0x80000, k16), // ABg
            PageTable::new(0x20000, k16), // BBg
            PageTable::new(0x40000, k16), // AObj
            PageTable::new(0x20000, k16), // BObj
            PageTable::new(0xA4000, k16), // Lcdc
            PageTable::new(0x40000, k16), // Arm7
            PageTable::new(0x80000, k16), // Texture
            PageTable::new(0x18000, k16), // TexPalette
            PageTable::new(0x8000, k8),   // ABgExtPal
            PageTable::new(0x8000, k8),   // BBgExtPal
            PageTable::new(0x2000, k8),   // AObjExtPal
            PageTable::new(0x2000, k8),   // BObjExtPal
        ];
        debug_assert_eq!(tables.len(), VIEWS);
        Self { data: vec![0; VRAM_TOTAL].into_boxed_slice(), cnt: [0; 9], tables }
    }

    fn table(&self, v: View) -> &PageTable {
        &self.tables[v as usize]
    }

    /// Write VRAMCNT for bank `bank` (0 = A … 8 = I) and rebuild the maps.
    pub fn set_cnt(&mut self, bank: usize, value: u8) {
        self.cnt[bank] = value;
        self.remap();
    }

    fn remap(&mut self) {
        for t in &mut self.tables {
            t.clear();
        }
        for bank in 0..9 {
            let c = self.cnt[bank];
            if c & 0x80 == 0 {
                continue;
            }
            let mst = (c & 7) as usize;
            let ofs = ((c >> 3) & 3) as usize;
            let base = BANK_OFFSETS[bank];
            let size = BANK_SIZES[bank];
            let mut map = |v: View, at: usize, len: usize| self.tables[v as usize].map(at, base, len);
            match (bank, mst) {
                (_, 0) => map(View::Lcdc, base, size),
                (0..=1, 1) | (2..=3, 1) => map(View::ABg, ofs * 0x20000, size),
                (0..=1, 2) => map(View::AObj, (ofs & 1) * 0x20000, size),
                (0..=3, 3) => map(View::Texture, ofs * 0x20000, size),
                (2..=3, 2) => map(View::Arm7, (ofs & 1) * 0x20000, size),
                (2, 4) => map(View::BBg, 0, size),
                (3, 4) => map(View::BObj, 0, size),
                (4, 1) => map(View::ABg, 0, size),
                (4, 2) => map(View::AObj, 0, size),
                (4, 3) => map(View::TexPalette, 0, size),
                (4, 4) => map(View::ABgExtPal, 0, 0x8000),
                (5..=6, 1) => map(View::ABg, (ofs & 1) * 0x4000 + (ofs >> 1) * 0x10000, size),
                (5..=6, 2) => map(View::AObj, (ofs & 1) * 0x4000 + (ofs >> 1) * 0x10000, size),
                (5..=6, 3) => map(View::TexPalette, ((ofs & 1) + (ofs >> 1) * 4) * 0x4000, size),
                (5..=6, 4) => map(View::ABgExtPal, (ofs & 1) * 0x4000, size),
                (5..=6, 5) => map(View::AObjExtPal, 0, 0x2000),
                (7, 1) => map(View::BBg, 0, size),
                (7, 2) => map(View::BBgExtPal, 0, size),
                (8, 1) => map(View::BBg, 0x8000, size),
                (8, 2) => map(View::BObj, 0, size),
                (8, 3) => map(View::BObjExtPal, 0, 0x2000),
                _ => tracing::warn!("invalid VRAM bank {} MST {mst}", (b'A' + bank as u8) as char),
            }
        }
    }

    /// Is anything mapped at `offset` of `view`?
    pub fn mapped(&self, v: View, offset: u32) -> bool {
        let t = self.table(v);
        !t.pages[(offset >> t.shift) as usize % t.pages.len()].is_empty()
    }

    #[inline]
    pub fn read8(&self, v: View, offset: u32) -> u8 {
        let t = self.table(v);
        let page = &t.pages[(offset >> t.shift) as usize % t.pages.len()];
        let within = offset as usize & ((1 << t.shift) - 1);
        match page.len() {
            0 => 0,
            1 => self.data[page[0] as usize + within],
            _ => page.iter().fold(0, |acc, &b| acc | self.data[b as usize + within]),
        }
    }

    #[inline]
    pub fn read16(&self, v: View, offset: u32) -> u16 {
        let t = self.table(v);
        let page = &t.pages[(offset >> t.shift) as usize % t.pages.len()];
        let within = offset as usize & ((1 << t.shift) - 2);
        let rd = |b: u32| u16::from_le_bytes([self.data[b as usize + within], self.data[b as usize + within + 1]]);
        match page.len() {
            0 => 0,
            1 => rd(page[0]),
            _ => page.iter().fold(0, |acc, &b| acc | rd(b)),
        }
    }

    pub fn read32(&self, v: View, offset: u32) -> u32 {
        self.read16(v, offset) as u32 | (self.read16(v, offset + 2) as u32) << 16
    }

    pub fn write8(&mut self, v: View, offset: u32, value: u8) {
        let t = &self.tables[v as usize];
        let idx = (offset >> t.shift) as usize % t.pages.len();
        let within = offset as usize & ((1 << t.shift) - 1);
        for &b in &t.pages[idx] {
            self.data[b as usize + within] = value;
        }
    }

    pub fn write16(&mut self, v: View, offset: u32, value: u16) {
        let t = &self.tables[v as usize];
        let idx = (offset >> t.shift) as usize % t.pages.len();
        let within = offset as usize & ((1 << t.shift) - 2);
        for &b in &t.pages[idx] {
            self.data[b as usize + within..b as usize + within + 2].copy_from_slice(&value.to_le_bytes());
        }
    }

    pub fn write32(&mut self, v: View, offset: u32, value: u32) {
        self.write16(v, offset, value as u16);
        self.write16(v, offset + 2, (value >> 16) as u16);
    }

    /// Decode a CPU address in 0x06000000..0x06FFFFFF (ARM9 view).
    pub fn arm9_view(addr: u32) -> (View, u32) {
        let off = addr & 0x00FF_FFFF;
        match off >> 21 {
            0 => (View::ABg, off & 0x7FFFF),
            1 => (View::BBg, off & 0x1FFFF),
            2 => (View::AObj, off & 0x3FFFF),
            3 => (View::BObj, off & 0x1FFFF),
            _ => (View::Lcdc, off % 0xA4000),
        }
    }

    /// VRAMSTAT as seen by the ARM7: bit 0 = C, bit 1 = D mapped to ARM7.
    pub fn arm7_stat(&self) -> u8 {
        let c = |bank: usize| self.cnt[bank] & 0x87 == 0x82;
        c(2) as u8 | (c(3) as u8) << 1
    }
}

impl SaveState for Vram {
    fn save_state(&self, w: &mut StateWriter) {
        w.bytes(&self.data);
        w.bytes(&self.cnt);
    }
    fn load_state(&mut self, r: &mut StateReader<'_>) -> StateResult<()> {
        r.bytes_into(&mut self.data)?;
        r.bytes_into(&mut self.cnt)?;
        self.remap();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lcdc_and_bg_mapping() {
        let mut v = Vram::new();
        v.set_cnt(0, 0x80); // A → LCDC
        v.write16(View::Lcdc, 0x10, 0xBEEF);
        assert_eq!(v.data[0x10], 0xEF);
        v.set_cnt(0, 0x81 | 1 << 3); // A → ABG offset 1 (0x20000)
        assert!(!v.mapped(View::Lcdc, 0));
        assert_eq!(v.read16(View::ABg, 0x20010), 0xBEEF);
        assert_eq!(v.read16(View::ABg, 0x10), 0);
        // mirrored across the 512 KiB window
        let (view, off) = Vram::arm9_view(0x0608_0000 + 0x20010);
        assert_eq!(view, View::ABg);
        assert_eq!(v.read16(view, off), 0xBEEF);
    }

    #[test]
    fn small_banks_and_overlap() {
        let mut v = Vram::new();
        v.set_cnt(5, 0x81 | 3 << 3); // F → ABG at 0x14000
        v.set_cnt(6, 0x81 | 3 << 3); // G → same place: overlap
        v.data[BANK_OFFSETS[5] + 4] = 0x0F;
        v.data[BANK_OFFSETS[6] + 4] = 0xF0;
        assert_eq!(v.read8(View::ABg, 0x14004), 0xFF);
        v.write8(View::ABg, 0x14008, 0x5A);
        assert_eq!(v.data[BANK_OFFSETS[5] + 8], 0x5A);
        assert_eq!(v.data[BANK_OFFSETS[6] + 8], 0x5A);
    }

    #[test]
    fn arm7_and_texture_slots() {
        let mut v = Vram::new();
        v.set_cnt(2, 0x82); // C → ARM7 slot 0
        v.set_cnt(3, 0x82 | 1 << 3); // D → ARM7 slot 1
        assert_eq!(v.arm7_stat(), 3);
        v.set_cnt(0, 0x83 | 2 << 3); // A → texture slot 2
        v.data[0x100] = 7;
        assert_eq!(v.read8(View::Texture, 0x40100), 7);
        v.set_cnt(4, 0x84); // E → ABG ext palette
        v.data[BANK_OFFSETS[4] + 0x2000] = 9;
        assert_eq!(v.read8(View::ABgExtPal, 0x2000), 9);
    }
}
