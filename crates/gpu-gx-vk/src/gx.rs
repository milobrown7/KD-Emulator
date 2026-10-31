//! The GX state machine: consumes FIFO commands, keeps CP/XF/BP state,
//! decodes geometry and textures, renders through the software reference
//! rasteriser and performs EFB copies (to XFB and to textures).

use crate::fifo::{triangulate, FifoParser, GxCommand, Primitive};
use crate::soft::{RasterState, SoftRenderer, SoftTexture, WinVertex, EFB_HEIGHT, EFB_WIDTH};
use crate::tev::TevConfig;
use crate::texture::{decode, TexFormat, TlutFormat};
use crate::vertex::{decode_vertices, CpState};
use crate::xf::XfState;
use std::collections::HashMap;

/// Guest physical memory as seen by the graphics hardware.
pub trait GxMemory {
    fn read(&self, addr: u32, len: usize) -> Vec<u8>;
    fn write(&mut self, addr: u32, data: &[u8]);
}

/// Events the system (PE / VI) must react to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GxEvent {
    /// PE_DONE (draw done / `GXSetDrawDone`).
    Finish,
    Token(u16),
    TokenInterrupt(u16),
    /// EFB → XFB copy finished (end of frame).
    XfbCopy { addr: u32, width: u16, height: u16 },
}

pub const TMEM_SIZE: usize = 0x10_0000;

pub struct Gx {
    pub cp: CpState,
    pub xf: XfState,
    pub bp: [u32; 256],
    bp_mask: u32,
    pub parser: FifoParser,
    pub soft: SoftRenderer,
    /// Texture memory (TLUTs are loaded here).
    pub tmem: Vec<u8>,
    texture_cache: HashMap<(u32, u32, u32, u32, u32), SoftTexture>,
    pub events: Vec<GxEvent>,
    pub draws: u64,
}

impl Default for Gx {
    fn default() -> Self {
        Self::new()
    }
}

impl Gx {
    pub fn new() -> Self {
        let mut bp = [0u32; 256];
        bp[0x41] = 0x18; // colour + alpha update
        bp[0x21] = (342 + EFB_WIDTH as u32 - 1) << 12 | (342 + EFB_HEIGHT as u32 - 1);
        bp[0x20] = 342 << 12 | 342;
        bp[0xF3] = 7 << 16 | 7 << 19;
        Self {
            cp: CpState::default(),
            xf: XfState::default(),
            bp,
            bp_mask: 0xFF_FFFF,
            parser: FifoParser::new(),
            soft: SoftRenderer::new(),
            tmem: vec![0; TMEM_SIZE],
            texture_cache: HashMap::new(),
            events: Vec::new(),
            draws: 0,
        }
    }

    /// Feed bytes written to the GX FIFO and execute complete commands.
    pub fn write_fifo(&mut self, bytes: &[u8], mem: &mut dyn GxMemory) {
        self.parser.push(bytes);
        while let Some(cmd) = self.parser.next(&self.cp) {
            self.execute(cmd, mem);
        }
    }

    pub fn execute(&mut self, cmd: GxCommand, mem: &mut dyn GxMemory) {
        match cmd {
            GxCommand::Nop | GxCommand::InvalidateVertexCache => {}
            GxCommand::LoadCp { reg, value } => self.cp.write(reg, value),
            GxCommand::LoadXf { addr, values } => self.xf.write(addr, &values),
            GxCommand::LoadIndexed { array, index, len, addr } => {
                let a = 12 + array as usize;
                let src = self.cp.array_base[a] + index as u32 * self.cp.array_stride[a];
                let data = mem.read(src, len as usize * 4);
                let words: Vec<u32> = data.chunks(4).map(|c| u32::from_be_bytes([c[0], c[1], c[2], c[3]])).collect();
                self.xf.write(addr, &words);
            }
            GxCommand::CallDisplayList { addr, size } => {
                let data = mem.read(addr & 0x03FF_FFE0, size as usize & !31);
                let mut p = FifoParser::new();
                p.push(&data);
                while let Some(c) = p.next(&self.cp) {
                    if !matches!(c, GxCommand::CallDisplayList { .. }) {
                        self.execute(c, mem);
                    }
                }
            }
            GxCommand::LoadBp { reg, value } => self.write_bp(reg, value, mem),
            GxCommand::Draw { primitive, vat, count, data } => self.draw(primitive, vat as usize, count as usize, &data, mem),
            GxCommand::Unknown(op) => tracing::warn!("unknown GX opcode {op:#04x}"),
        }
    }

    fn write_bp(&mut self, reg: u8, value: u32, mem: &mut dyn GxMemory) {
        if reg == 0xFE {
            self.bp_mask = value & 0xFF_FFFF;
            return;
        }
        let r = reg as usize;
        self.bp[r] = (self.bp[r] & !self.bp_mask) | (value & self.bp_mask);
        self.bp_mask = 0xFF_FFFF;
        match reg {
            0x45 => self.events.push(GxEvent::Finish),
            0x47 => self.events.push(GxEvent::Token(value as u16)),
            0x48 => self.events.push(GxEvent::TokenInterrupt(value as u16)),
            0x52 => self.efb_copy(value, mem),
            0x65 => {
                // LOADTLUT1: copy from main memory (LOADTLUT0 << 5) into TMEM.
                let src = (self.bp[0x64] & 0xFF_FFFF) << 5;
                let tmem_addr = ((value & 0x3FF) << 9) as usize;
                let count = ((value >> 10) & 0x7FF) as usize * 32;
                let data = mem.read(src, count);
                let end = (tmem_addr + data.len()).min(TMEM_SIZE);
                self.tmem[tmem_addr..end].copy_from_slice(&data[..end - tmem_addr]);
                self.texture_cache.clear();
            }
            _ => {}
        }
    }

    fn texture(&mut self, map: usize, mem: &dyn GxMemory) -> Option<SoftTexture> {
        let (mode, image0, image3, tlut) = if map < 4 {
            (self.bp[0x80 + map], self.bp[0x88 + map], self.bp[0x94 + map], self.bp[0x98 + map])
        } else {
            (self.bp[0xA0 + map - 4], self.bp[0xA8 + map - 4], self.bp[0xB4 + map - 4], self.bp[0xB8 + map - 4])
        };
        let w = (image0 & 0x3FF) + 1;
        let h = ((image0 >> 10) & 0x3FF) + 1;
        let fmt_bits = (image0 >> 20) & 0xF;
        let addr = (image3 & 0xFF_FFFF) << 5;
        let key = (addr, w, h, fmt_bits, tlut);
        if let Some(t) = self.texture_cache.get(&key) {
            return Some(t.clone());
        }
        let fmt = TexFormat::from_bits(fmt_bits)?;
        let data = mem.read(addr, fmt.size(w as usize, h as usize));
        let tlut_addr = ((tlut & 0x3FF) << 9) as usize;
        let tlut_fmt = match (tlut >> 10) & 3 {
            0 => TlutFormat::Ia8,
            1 => TlutFormat::Rgb565,
            _ => TlutFormat::Rgb5a3,
        };
        let pixels = decode(fmt, &data, w as usize, h as usize, &self.tmem[tlut_addr.min(TMEM_SIZE)..], tlut_fmt);
        let wrap = |v: u32| (v & 3).min(2) as u8;
        let t = SoftTexture { width: w as usize, height: h as usize, pixels, wrap: [wrap(mode), wrap(mode >> 2)] };
        self.texture_cache.insert(key, t.clone());
        Some(t)
    }

    fn draw(&mut self, primitive: Primitive, vat: usize, count: usize, data: &[u8], mem: &mut dyn GxMemory) {
        self.draws += 1;
        let verts = {
            let m: &dyn GxMemory = mem;
            decode_vertices(&self.cp, vat, data, count, &|addr, n| m.read(addr, n))
        };
        let win: Vec<WinVertex> = verts
            .iter()
            .map(|v| {
                let c = self.xf.transform(v);
                WinVertex::new(self.xf.to_window(&c), &c)
            })
            .collect();
        let tev = TevConfig::from_bp(&self.bp);
        let mut textures: [Option<SoftTexture>; 8] = Default::default();
        for st in &tev.stages {
            if st.tex_enabled && textures[st.texmap as usize].is_none() {
                textures[st.texmap as usize] = self.texture(st.texmap as usize, mem);
            }
        }
        // TEV colour registers: 0xE0.. (RA, BG pairs, 11-bit signed).
        let reg_val = |v: u32| ((((v & 0x7FF) << 21) as i32) >> 21) as f32 / 255.0;
        let mut regs = [[0.0f32; 4]; 4];
        let mut konst = [[0.0f32; 4]; 4];
        for i in 0..4 {
            let ra = self.bp[0xE0 + i * 2];
            let bg = self.bp[0xE1 + i * 2];
            let target = if ra & (1 << 23) != 0 { &mut konst[i] } else { &mut regs[i] };
            *target = [reg_val(ra), reg_val(bg >> 12), reg_val(bg), reg_val(ra >> 12)];
        }
        let rs = RasterState::from_bp(&self.bp);
        for tri in triangulate(primitive, win.len()) {
            self.soft.draw_triangle([win[tri[0]], win[tri[1]], win[tri[2]]], &tev, &regs, &konst, &textures, &rs);
        }
    }

    /// EFB copy (BP 0x52). Copies to the XFB in YUYV, or to a texture in
    /// RGBA8 / RGB565 / RGB5A3 / I8 tiled layout.
    fn efb_copy(&mut self, ctrl: u32, mem: &mut dyn GxMemory) {
        let src = self.bp[0x49];
        let size = self.bp[0x4A];
        let (sx, sy) = ((src & 0x3FF) as usize, ((src >> 10) & 0x3FF) as usize);
        let (w, h) = (((size & 0x3FF) + 1) as usize, (((size >> 10) & 0x3FF) + 1) as usize);
        let dest = (self.bp[0x4B] & 0xFF_FFFF) << 5;
        let to_xfb = ctrl & (1 << 14) != 0;
        let px = |x: usize, y: usize| -> u32 {
            let (x, y) = ((sx + x).min(EFB_WIDTH - 1), (sy + y).min(EFB_HEIGHT - 1));
            self.soft.color[y * EFB_WIDTH + x]
        };
        if to_xfb {
            let stride = ((self.bp[0x4D] & 0x3FF) << 5) as usize;
            let stride = if stride == 0 { w * 2 } else { stride };
            for y in 0..h {
                let mut row = Vec::with_capacity(w * 2);
                for x in (0..w).step_by(2) {
                    let yuv = |p: u32| {
                        let (r, g, b) = ((p & 0xFF) as f32, ((p >> 8) & 0xFF) as f32, ((p >> 16) & 0xFF) as f32);
                        (
                            (16.0 + 0.257 * r + 0.504 * g + 0.098 * b).round(),
                            (128.0 - 0.148 * r - 0.291 * g + 0.439 * b).round(),
                            (128.0 + 0.439 * r - 0.368 * g - 0.071 * b).round(),
                        )
                    };
                    let (y0, u0, v0) = yuv(px(x, y));
                    let (y1, u1, v1) = yuv(px(x + 1, y));
                    row.extend([y0 as u8, ((u0 + u1) / 2.0) as u8, y1 as u8, ((v0 + v1) / 2.0) as u8]);
                }
                mem.write(dest + (y * stride) as u32, &row);
            }
            self.events.push(GxEvent::XfbCopy { addr: dest, width: w as u16, height: h as u16 });
            self.texture_cache.clear();
        } else {
            let fmt = ((ctrl >> 4) & 7) | ((ctrl >> 3) & 1) << 3;
            let (tex_fmt, block_bytes) = match fmt {
                6 => (TexFormat::Rgba8, 64),
                4 => (TexFormat::Rgb565, 32),
                5 => (TexFormat::Rgb5a3, 32),
                _ => (TexFormat::I8, 32),
            };
            let (bw, bh) = tex_fmt.block();
            let blocks_x = w.div_ceil(bw);
            let mut out = vec![0u8; blocks_x * h.div_ceil(bh) * block_bytes];
            for y in 0..h {
                for x in 0..w {
                    let p = px(x, y);
                    let (r, g, b, a) = (p & 0xFF, (p >> 8) & 0xFF, (p >> 16) & 0xFF, p >> 24);
                    let blk = ((y / bh) * blocks_x + x / bw) * block_bytes;
                    let i = (y % bh) * bw + x % bw;
                    match tex_fmt {
                        TexFormat::Rgba8 => {
                            out[blk + i * 2] = a as u8;
                            out[blk + i * 2 + 1] = r as u8;
                            out[blk + 32 + i * 2] = g as u8;
                            out[blk + 32 + i * 2 + 1] = b as u8;
                        }
                        TexFormat::Rgb565 => {
                            let v = ((r >> 3) << 11 | (g >> 2) << 5 | (b >> 3)) as u16;
                            out[blk + i * 2..blk + i * 2 + 2].copy_from_slice(&v.to_be_bytes());
                        }
                        TexFormat::Rgb5a3 => {
                            let v = (0x8000 | (r >> 3) << 10 | (g >> 3) << 5 | (b >> 3)) as u16;
                            out[blk + i * 2..blk + i * 2 + 2].copy_from_slice(&v.to_be_bytes());
                        }
                        _ => out[blk + i] = ((r * 77 + g * 150 + b * 29) >> 8) as u8,
                    }
                }
            }
            mem.write(dest, &out);
            self.texture_cache.clear();
        }
        if ctrl & (1 << 11) != 0 {
            let ar = self.bp[0x4F];
            let gb = self.bp[0x50];
            let rgba = ((ar >> 8) & 0xFF) | (gb & 0xFF00) | (gb & 0xFF) << 16 | (ar & 0xFF) << 24;
            let z = (self.bp[0x51] & 0xFF_FFFF) as f32 / 16_777_215.0;
            self.soft.clear(rgba, z);
        }
    }

    /// Convert an RGBA8 EFB pixel to the packed format the front-end uses.
    pub fn efb_pixel(&self, x: usize, y: usize) -> u32 {
        self.soft.color[y * EFB_WIDTH + x]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xf::standard_2d;

    struct Ram(Vec<u8>);
    impl GxMemory for Ram {
        fn read(&self, addr: u32, len: usize) -> Vec<u8> {
            let a = addr as usize;
            self.0.get(a..a + len).map(|s| s.to_vec()).unwrap_or_else(|| vec![0; len])
        }
        fn write(&mut self, addr: u32, data: &[u8]) {
            let a = addr as usize;
            self.0[a..a + data.len()].copy_from_slice(data);
        }
    }

    fn bp(reg: u8, v: u32) -> Vec<u8> {
        let w = (reg as u32) << 24 | (v & 0xFF_FFFF);
        let mut b = vec![0x61];
        b.extend(w.to_be_bytes());
        b
    }

    fn cp(reg: u8, v: u32) -> Vec<u8> {
        let mut b = vec![0x08, reg];
        b.extend(v.to_be_bytes());
        b
    }

    #[test]
    fn fifo_triangle_then_xfb_copy() {
        let mut gx = Gx::new();
        standard_2d(&mut gx.xf);
        let mut ram = Ram(vec![0; 4 << 20]);
        let mut s = vec![];
        s.extend(cp(0x50, 1 << 9 | 1 << 13)); // POS direct, COL0 direct
        s.extend(cp(0x70, 1 | 4 << 1 | 5 << 14)); // XYZ f32, RGBA8
        s.extend(bp(0x00, 0)); // 1 stage, no cull
        s.extend(bp(0x28, 0)); // stage 0: colour channel 0, no texture
        s.extend(bp(0xC0, 0xFFFA)); // d = RASC
        s.extend(bp(0xC1, 0x7 << 13 | 7 << 10 | 7 << 7 | 5 << 4)); // d = RASA
        s.push(0x90); // triangles, VAT 0
        s.extend(3u16.to_be_bytes());
        for (x, y) in [(-0.5f32, 0.5f32), (0.5, 0.5), (0.0, -0.5)] {
            for f in [x, y, 0.0] {
                s.extend(f.to_be_bytes());
            }
            s.extend([0x00, 0xFF, 0x00, 0xFF]); // green
        }
        // copy 640x480 to XFB at 0x100000
        s.extend(bp(0x49, 0));
        s.extend(bp(0x4A, 639 | 479 << 10));
        s.extend(bp(0x4B, 0x10_0000 >> 5));
        s.extend(bp(0x4D, 1280 >> 5));
        s.extend(bp(0x52, 1 << 14));
        gx.write_fifo(&s, &mut ram);
        assert_eq!(gx.draws, 1);
        assert_eq!(gx.efb_pixel(320, 240), 0xFF00_FF00, "green centre");
        assert_eq!(gx.efb_pixel(10, 10), 0xFF00_0000);
        assert_eq!(gx.events, vec![GxEvent::XfbCopy { addr: 0x10_0000, width: 640, height: 480 }]);
        // YUYV for pure green: Y ≈ 145, U ≈ 54, V ≈ 34
        let row = 240 * 1280 + 320 * 2;
        let yuyv = &ram.0[0x10_0000 + row..0x10_0000 + row + 4];
        assert!((yuyv[0] as i32 - 145).abs() <= 1 && (yuyv[1] as i32 - 54).abs() <= 1, "{yuyv:?}");
    }

    #[test]
    fn bp_mask_applies_once() {
        let mut gx = Gx::new();
        let mut ram = Ram(vec![0; 1024]);
        let mut s = bp(0x40, 0x17);
        s.extend(bp(0xFE, 0x01));
        s.extend(bp(0x40, 0x00));
        s.extend(bp(0x41, 0x00));
        gx.write_fifo(&s, &mut ram);
        assert_eq!(gx.bp[0x40], 0x16, "only bit 0 was writable");
        assert_eq!(gx.bp[0x41], 0, "mask resets after one write");
    }
}
