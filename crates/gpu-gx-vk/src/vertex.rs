//! Command Processor vertex state (VCD / VAT / arrays) and vertex decoding.

use kd_core::{SaveState, StateReader, StateResult, StateWriter};

/// How an attribute is supplied (VCD).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttrType {
    None,
    Direct,
    Index8,
    Index16,
}

impl AttrType {
    fn from_bits(v: u32) -> Self {
        match v & 3 {
            0 => AttrType::None,
            1 => AttrType::Direct,
            2 => AttrType::Index8,
            _ => AttrType::Index16,
        }
    }
}

/// CP register file (vertex descriptor, attribute tables, arrays).
#[derive(Debug, Clone, Default)]
pub struct CpState {
    pub matidx_a: u32,
    pub matidx_b: u32,
    pub vcd_lo: u32,
    pub vcd_hi: u32,
    pub vat_a: [u32; 8],
    pub vat_b: [u32; 8],
    pub vat_c: [u32; 8],
    pub array_base: [u32; 16],
    pub array_stride: [u32; 16],
}

pub const ARRAY_POS: usize = 0;
pub const ARRAY_NRM: usize = 1;
pub const ARRAY_CLR0: usize = 2;
pub const ARRAY_TEX0: usize = 4;

/// Per-attribute layout derived from VCD + VAT for one VAT slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttrFormat {
    pub ty: AttrType,
    /// Component count.
    pub count: u32,
    /// 0 u8, 1 s8, 2 u16, 3 s16, 4 f32 (colors: 0..=5 colour formats).
    pub format: u32,
    pub frac: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VertexLayout {
    pub pnmtx: bool,
    pub texmtx: [bool; 8],
    pub pos: AttrFormat,
    pub nrm: AttrFormat,
    /// Normal-binormal-tangent with three separate indices.
    pub nrm_index3: bool,
    pub color: [AttrFormat; 2],
    pub tex: [AttrFormat; 8],
}

fn comp_size(format: u32) -> u32 {
    match format {
        0 | 1 => 1,
        2 | 3 => 2,
        _ => 4,
    }
}

fn color_size(format: u32) -> u32 {
    match format {
        0 | 3 => 2,
        1 | 4 => 3,
        _ => 4,
    }
}

impl CpState {
    pub fn write(&mut self, reg: u8, v: u32) {
        let r = reg as usize;
        match reg {
            0x30 => self.matidx_a = v,
            0x40 => self.matidx_b = v,
            0x50 => self.vcd_lo = v,
            0x60 => self.vcd_hi = v,
            0x70..=0x77 => self.vat_a[r & 7] = v,
            0x80..=0x87 => self.vat_b[r & 7] = v,
            0x90..=0x97 => self.vat_c[r & 7] = v,
            0xA0..=0xAF => self.array_base[r & 15] = v & 0x03FF_FFFF,
            0xB0..=0xBF => self.array_stride[r & 15] = v & 0xFF,
            _ => tracing::warn!("unknown CP register {reg:#04x} = {v:#x}"),
        }
    }

    pub fn layout(&self, vat: usize) -> VertexLayout {
        let (a, b, c) = (self.vat_a[vat], self.vat_b[vat], self.vat_c[vat]);
        let lo = self.vcd_lo;
        let tex_fmt = |i: usize| -> (u32, u32, u32) {
            match i {
                0 => ((a >> 21) & 1, (a >> 22) & 7, (a >> 25) & 31),
                1 => (b & 1, (b >> 1) & 7, (b >> 4) & 31),
                2 => ((b >> 9) & 1, (b >> 10) & 7, (b >> 13) & 31),
                3 => ((b >> 18) & 1, (b >> 19) & 7, (b >> 22) & 31),
                4 => ((b >> 27) & 1, (b >> 28) & 7, c & 31),
                5 => ((c >> 5) & 1, (c >> 6) & 7, (c >> 9) & 31),
                6 => ((c >> 14) & 1, (c >> 15) & 7, (c >> 18) & 31),
                _ => ((c >> 23) & 1, (c >> 24) & 7, (c >> 27) & 31),
            }
        };
        let tex = std::array::from_fn(|i| {
            let (cnt, fmt, frac) = tex_fmt(i);
            AttrFormat { ty: AttrType::from_bits(self.vcd_hi >> (i * 2)), count: cnt + 1, format: fmt, frac }
        });
        VertexLayout {
            pnmtx: lo & 1 != 0,
            texmtx: std::array::from_fn(|i| lo & (2 << i) != 0),
            pos: AttrFormat { ty: AttrType::from_bits(lo >> 9), count: 2 + (a & 1), format: (a >> 1) & 7, frac: (a >> 4) & 31 },
            nrm: AttrFormat {
                ty: AttrType::from_bits(lo >> 11),
                count: if (a >> 9) & 1 != 0 { 9 } else { 3 },
                format: (a >> 10) & 7,
                frac: if (a >> 10) & 7 == 1 { 6 } else { 14 },
            },
            nrm_index3: a & (1 << 31) != 0,
            color: [
                AttrFormat { ty: AttrType::from_bits(lo >> 13), count: 4, format: (a >> 14) & 7, frac: 0 },
                AttrFormat { ty: AttrType::from_bits(lo >> 15), count: 4, format: (a >> 18) & 7, frac: 0 },
            ],
            tex,
        }
    }

    /// Bytes per vertex in the FIFO for VAT slot `vat`.
    pub fn vertex_size(&self, vat: usize) -> usize {
        let l = self.layout(vat);
        let mut n = l.pnmtx as u32 + l.texmtx.iter().filter(|&&t| t).count() as u32;
        let attr = |f: &AttrFormat, direct: u32| match f.ty {
            AttrType::None => 0,
            AttrType::Direct => direct,
            AttrType::Index8 => 1,
            AttrType::Index16 => 2,
        };
        n += attr(&l.pos, l.pos.count * comp_size(l.pos.format));
        let nrm_idx_mul = if l.nrm.count == 9 && l.nrm_index3 { 3 } else { 1 };
        n += match l.nrm.ty {
            AttrType::Index8 | AttrType::Index16 => attr(&l.nrm, 0) * nrm_idx_mul,
            _ => attr(&l.nrm, l.nrm.count * comp_size(l.nrm.format)),
        };
        for c in &l.color {
            n += attr(c, color_size(c.format));
        }
        for t in &l.tex {
            n += attr(t, t.count * comp_size(t.format));
        }
        n as usize
    }
}

/// A decoded vertex in model space.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vertex {
    pub pnmtx: u8,
    pub texmtx: [u8; 8],
    pub pos: [f32; 3],
    pub nrm: [f32; 3],
    /// RGBA8 (R in the high byte, as GX stores it).
    pub color: [u32; 2],
    pub tex: [[f32; 2]; 8],
    pub has_color: [bool; 2],
}

fn read_comp(data: &[u8], format: u32, frac: u32) -> f32 {
    let scale = 1.0 / (1u32 << frac.min(31)) as f32;
    match format {
        0 => data[0] as f32 * scale,
        1 => data[0] as i8 as f32 * scale,
        2 => u16::from_be_bytes([data[0], data[1]]) as f32 * scale,
        3 => i16::from_be_bytes([data[0], data[1]]) as f32 * scale,
        _ => f32::from_be_bytes([data[0], data[1], data[2], data[3]]),
    }
}

fn read_color(d: &[u8], format: u32) -> u32 {
    let ex = |v: u32, bits: u32| (v << (8 - bits)) | (v >> (2 * bits).saturating_sub(8));
    match format {
        0 => {
            let v = u16::from_be_bytes([d[0], d[1]]) as u32;
            ex(v >> 11, 5) << 24 | ex((v >> 5) & 63, 6) << 16 | ex(v & 31, 5) << 8 | 0xFF
        }
        1 | 2 => (d[0] as u32) << 24 | (d[1] as u32) << 16 | (d[2] as u32) << 8 | 0xFF,
        3 => {
            let v = u16::from_be_bytes([d[0], d[1]]) as u32;
            ((v >> 12) * 0x11) << 24 | (((v >> 8) & 15) * 0x11) << 16 | (((v >> 4) & 15) * 0x11) << 8 | ((v & 15) * 0x11)
        }
        4 => {
            let v = (d[0] as u32) << 16 | (d[1] as u32) << 8 | d[2] as u32;
            ex(v >> 18, 6) << 24 | ex((v >> 12) & 63, 6) << 16 | ex((v >> 6) & 63, 6) << 8 | ex(v & 63, 6)
        }
        _ => u32::from_be_bytes([d[0], d[1], d[2], d[3]]),
    }
}

/// Take an attribute's bytes: inline data, or the indexed array element
/// (array base + index * stride) read through `mem`.
fn fetch_attr(
    d: &mut &[u8],
    cp: &CpState,
    mem: &dyn Fn(u32, usize) -> Vec<u8>,
    f: &AttrFormat,
    array: usize,
    direct_len: usize,
) -> Option<Vec<u8>> {
    match f.ty {
        AttrType::None => None,
        AttrType::Direct => {
            let (h, t) = d.split_at(direct_len);
            *d = t;
            Some(h.to_vec())
        }
        AttrType::Index8 | AttrType::Index16 => {
            let idx = if f.ty == AttrType::Index8 {
                let i = d[0] as u32;
                *d = &d[1..];
                i
            } else {
                let i = u16::from_be_bytes([d[0], d[1]]) as u32;
                *d = &d[2..];
                i
            };
            let addr = cp.array_base[array] + idx * cp.array_stride[array];
            Some(mem(addr, direct_len))
        }
    }
}

/// Decode `count` vertices. `mem` reads guest memory for indexed
/// attributes (array base + index * stride).
pub fn decode_vertices(cp: &CpState, vat: usize, data: &[u8], count: usize, mem: &dyn Fn(u32, usize) -> Vec<u8>) -> Vec<Vertex> {
    let l = cp.layout(vat);
    let size = cp.vertex_size(vat);
    let default_pnmtx = (cp.matidx_a & 0x3F) as u8;
    let default_tex: [u8; 8] = std::array::from_fn(|i| {
        if i < 4 {
            ((cp.matidx_a >> (6 + i * 6)) & 0x3F) as u8
        } else {
            ((cp.matidx_b >> ((i - 4) * 6)) & 0x3F) as u8
        }
    });
    let mut out = Vec::with_capacity(count);
    for v in 0..count {
        let Some(mut d) = data.get(v * size..(v + 1) * size) else { break };
        let mut vx = Vertex { pnmtx: default_pnmtx, texmtx: default_tex, ..Default::default() };
        if l.pnmtx {
            vx.pnmtx = d[0];
            d = &d[1..];
        }
        for i in 0..8 {
            if l.texmtx[i] {
                vx.texmtx[i] = d[0];
                d = &d[1..];
            }
        }
        if let Some(p) = fetch_attr(&mut d, cp, mem, &l.pos, ARRAY_POS, (l.pos.count * comp_size(l.pos.format)) as usize) {
            let cs = comp_size(l.pos.format) as usize;
            for c in 0..l.pos.count as usize {
                vx.pos[c] = read_comp(&p[c * cs..], l.pos.format, l.pos.frac);
            }
        }
        let nrm_len = (l.nrm.count * comp_size(l.nrm.format)) as usize;
        if let Some(n) = fetch_attr(&mut d, cp, mem, &l.nrm, ARRAY_NRM, nrm_len) {
            let cs = comp_size(l.nrm.format) as usize;
            for c in 0..3 {
                vx.nrm[c] = read_comp(&n[c * cs..], l.nrm.format, l.nrm.frac);
            }
            if l.nrm.count == 9 && l.nrm_index3 && l.nrm.ty != AttrType::Direct {
                // binormal/tangent indices follow; skip them
                let skip = if l.nrm.ty == AttrType::Index8 { 2 } else { 4 };
                d = &d[skip..];
            }
        }
        for i in 0..2 {
            let fmt = l.color[i].format;
            if let Some(c) = fetch_attr(&mut d, cp, mem, &l.color[i], ARRAY_CLR0 + i, color_size(fmt) as usize) {
                vx.color[i] = read_color(&c, fmt);
                vx.has_color[i] = true;
            }
        }
        for i in 0..8 {
            let t = l.tex[i];
            let cs = comp_size(t.format) as usize;
            if let Some(tc) = fetch_attr(&mut d, cp, mem, &t, ARRAY_TEX0 + i, t.count as usize * cs) {
                for c in 0..t.count as usize {
                    vx.tex[i][c] = read_comp(&tc[c * cs..], t.format, t.frac);
                }
            }
        }
        out.push(vx);
    }
    out
}

impl SaveState for CpState {
    fn save_state(&self, w: &mut StateWriter) {
        w.u32s(&[self.matidx_a, self.matidx_b, self.vcd_lo, self.vcd_hi]);
        w.u32s(&self.vat_a);
        w.u32s(&self.vat_b);
        w.u32s(&self.vat_c);
        w.u32s(&self.array_base);
        w.u32s(&self.array_stride);
    }
    fn load_state(&mut self, r: &mut StateReader<'_>) -> StateResult<()> {
        let mut h = [0u32; 4];
        r.u32s_into(&mut h)?;
        [self.matidx_a, self.matidx_b, self.vcd_lo, self.vcd_hi] = h;
        r.u32s_into(&mut self.vat_a)?;
        r.u32s_into(&mut self.vat_b)?;
        r.u32s_into(&mut self.vat_c)?;
        r.u32s_into(&mut self.array_base)?;
        r.u32s_into(&mut self.array_stride)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_mem(_: u32, n: usize) -> Vec<u8> {
        vec![0; n]
    }

    #[test]
    fn direct_float_position_and_rgba_color() {
        let mut cp = CpState::default();
        cp.write(0x50, 1 << 9 | 1 << 13); // POS direct, COL0 direct
        cp.write(0x70, 1 | 4 << 1 | 5 << 14); // XYZ f32, RGBA8888
        assert_eq!(cp.vertex_size(0), 16);
        let mut data = vec![];
        for f in [1.0f32, -2.0, 0.5] {
            data.extend(f.to_be_bytes());
        }
        data.extend([0xFF, 0x80, 0x00, 0x40]);
        let v = decode_vertices(&cp, 0, &data, 1, &no_mem);
        assert_eq!(v[0].pos, [1.0, -2.0, 0.5]);
        assert_eq!(v[0].color[0], 0xFF80_0040);
    }

    #[test]
    fn indexed_s16_position_with_fraction_and_texcoords() {
        let mut cp = CpState::default();
        cp.write(0x50, 2 << 9); // POS index8
        cp.write(0x60, 1); // TEX0 direct
        cp.write(0x70, 1 | 3 << 1 | 8 << 4 | 1 << 21 | 3 << 22 | 8 << 25); // XYZ s16 frac 8, TEX0 ST s16 frac 8
        cp.write(0xA0, 0x1000);
        cp.write(0xB0, 6);
        assert_eq!(cp.vertex_size(0), 1 + 4);
        let mem = |addr: u32, n: usize| -> Vec<u8> {
            assert_eq!(addr, 0x1000 + 2 * 6);
            let mut v = vec![];
            for x in [0x0180i16, -0x0100, 0x0040] {
                v.extend(x.to_be_bytes());
            }
            v.truncate(n);
            v
        };
        let data = [2u8, 0x01, 0x00, 0x00, 0x80];
        let v = decode_vertices(&cp, 0, &data, 1, &mem);
        assert_eq!(v[0].pos, [1.5, -1.0, 0.25]);
        assert_eq!(v[0].tex[0], [1.0, 0.5]);
    }

    #[test]
    fn color_formats() {
        assert_eq!(read_color(&[0xF8, 0x00], 0), 0xFF00_00FF); // RGB565 red
        assert_eq!(read_color(&[0xF0, 0x0F], 3), 0xFF00_00FF); // RGBA4444
        assert_eq!(read_color(&[0x12, 0x34, 0x56], 1), 0x1234_56FF);
    }

    #[test]
    fn matrix_index_bytes() {
        let mut cp = CpState::default();
        cp.write(0x50, 1 | 2 | 1 << 9); // PNMTXIDX + TEX0MTXIDX + POS direct
        cp.write(0x70, 0); // XY u8
        assert_eq!(cp.vertex_size(0), 4);
        let v = decode_vertices(&cp, 0, &[6, 30, 10, 20], 1, &no_mem);
        assert_eq!(v[0].pnmtx, 6);
        assert_eq!(v[0].texmtx[0], 30);
        assert_eq!(v[0].pos, [10.0, 20.0, 0.0]);
    }
}
