//! The two 2D graphics engines (A = main, B = sub).
//!
//! Extends the GBA PPU model with the DS features: 32-bit DISPCNT with
//! character/screen base offsets, extended (16-bit map / bitmap / direct
//! colour) and large bitmap backgrounds, extended palettes, 1D tile
//! boundaries, bitmap sprites, the 3D layer on engine A's BG0, display
//! modes (off / normal / VRAM / main memory) and master brightness.

use crate::vram::{View, Vram};
use kd_core::{SaveState, StateReader, StateResult, StateWriter};

pub const WIDTH: usize = 256;
pub const HEIGHT: usize = 192;

const TRANSPARENT: u32 = 0x8000_0000;
const LAYER_OBJ: usize = 4;
const LAYER_BD: usize = 5;

#[derive(Debug, Clone, Copy)]
struct ObjPixel {
    color: u16,
    prio: u8,
    semi: bool,
    alpha: u8,
    opaque: bool,
}

const NO_OBJ: ObjPixel = ObjPixel { color: 0, prio: 4, semi: false, alpha: 16, opaque: false };

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineId {
    A,
    B,
}

pub struct Engine2d {
    pub id: EngineId,
    /// Registers 0x00..0x6F (halfword indexed) relative to the engine base.
    pub regs: [u16; 0x38],
    bg_ref: [[i32; 2]; 2],
    /// Per-BG line buffers: BGR555 with bit 31 = transparent, bits 16-20 =
    /// 3D alpha (BG0 3D layer only).
    bg_lines: [[u32; WIDTH]; 4],
    obj_line: [ObjPixel; WIDTH],
    obj_window: [bool; WIDTH],
}

#[inline]
fn rd16(m: &[u8], a: usize) -> u16 {
    u16::from_le_bytes([m[a], m[a + 1]])
}

impl Engine2d {
    pub fn new(id: EngineId) -> Self {
        let mut regs = [0u16; 0x38];
        for off in [0x20, 0x26, 0x30, 0x36] {
            regs[off / 2] = 0x100;
        }
        Self {
            id,
            regs,
            bg_ref: [[0; 2]; 2],
            bg_lines: [[TRANSPARENT; WIDTH]; 4],
            obj_line: [NO_OBJ; WIDTH],
            obj_window: [false; WIDTH],
        }
    }

    #[inline]
    fn reg(&self, off: usize) -> u16 {
        self.regs[off >> 1]
    }

    pub fn dispcnt(&self) -> u32 {
        self.regs[0] as u32 | (self.regs[1] as u32) << 16
    }

    pub fn read16(&self, off: u32) -> u16 {
        match off {
            0x00 | 0x02 | 0x08..=0x0E | 0x48 | 0x4A | 0x50 | 0x52 | 0x64 | 0x66 | 0x6C => self.reg(off as usize),
            _ => 0,
        }
    }

    pub fn write16(&mut self, off: u32, v: u16) {
        let o = off as usize;
        if o >= 0x70 {
            return;
        }
        self.regs[o >> 1] = v;
        if (0x28..0x30).contains(&off) {
            self.reload_ref(0);
        } else if (0x38..0x40).contains(&off) {
            self.reload_ref(1);
        }
    }

    fn reload_ref(&mut self, i: usize) {
        let base = 0x28 + i * 0x10;
        for k in 0..2 {
            let lo = self.reg(base + k * 4) as u32;
            let hi = self.reg(base + k * 4 + 2) as u32;
            self.bg_ref[i][k] = (((hi << 16 | lo) << 4) as i32) >> 4;
        }
    }

    pub fn on_vblank(&mut self) {
        self.reload_ref(0);
        self.reload_ref(1);
    }

    fn views(&self) -> (View, View, View, View) {
        match self.id {
            EngineId::A => (View::ABg, View::AObj, View::ABgExtPal, View::AObjExtPal),
            EngineId::B => (View::BBg, View::BObj, View::BBgExtPal, View::BObjExtPal),
        }
    }

    fn mosaic(&self) -> (usize, usize, usize, usize) {
        let m = self.reg(0x4C) as usize;
        ((m & 0xF) + 1, ((m >> 4) & 0xF) + 1, ((m >> 8) & 0xF) + 1, ((m >> 12) & 0xF) + 1)
    }

    /// Render line `y` into `out` (BGR555 per pixel, before master
    /// brightness). `layer3d` is engine A's 3D output for this line
    /// (RGB555 | alpha << 16, alpha 0 = transparent).
    pub fn render_line(&mut self, y: usize, vram: &Vram, palette: &[u8], oam: &[u8], layer3d: Option<&[u32]>, out: &mut [u16]) {
        let dispcnt = self.dispcnt();
        match (dispcnt >> 16) & 3 {
            0 => {
                out.fill(0x7FFF);
                return;
            }
            2 if self.id == EngineId::A => {
                let bank = ((dispcnt >> 18) & 3) * 0x20000;
                for (x, o) in out.iter_mut().enumerate().take(WIDTH) {
                    *o = vram.read16(View::Lcdc, bank + ((y * WIDTH + x) * 2) as u32) & 0x7FFF;
                }
                self.advance_refs();
                return;
            }
            3 => {
                // Main memory display FIFO is not emulated; show black.
                out.fill(0);
                self.advance_refs();
                return;
            }
            _ => {}
        }
        if dispcnt & 0x80 != 0 {
            out.fill(0x7FFF);
            self.advance_refs();
            return;
        }
        let mode = dispcnt & 7;
        let mut active = [false; 4];
        for (n, a) in active.iter_mut().enumerate() {
            *a = dispcnt & (0x100 << n) != 0;
        }
        for n in 0..4 {
            if !active[n] {
                continue;
            }
            let three_d = n == 0 && self.id == EngineId::A && dispcnt & 8 != 0;
            if three_d {
                self.render_3d_layer(layer3d);
                continue;
            }
            let kind = match (mode, n) {
                (0, _) | (1..=5, 0) | (1..=5, 1) | (1, 2) | (3, 2) => BgKind::Text,
                (1, 3) | (2, 2) | (2, 3) | (4, 2) => BgKind::Affine,
                (3, 3) | (4, 3) | (5, 2) | (5, 3) => BgKind::Extended,
                (6, 2) => BgKind::Large,
                _ => {
                    active[n] = false;
                    continue;
                }
            };
            match kind {
                BgKind::Text => self.render_text(n, y, vram, palette),
                BgKind::Affine => self.render_affine(n, vram, palette, false),
                BgKind::Extended => self.render_affine(n, vram, palette, true),
                BgKind::Large => self.render_large(vram, palette),
            }
        }
        self.obj_line.fill(NO_OBJ);
        self.obj_window.fill(false);
        if dispcnt & 0x1000 != 0 {
            self.render_objects(y, vram, palette, oam);
        }
        self.compose(y, &active, palette, out);
        self.advance_refs();
    }

    fn advance_refs(&mut self) {
        for i in 0..2 {
            let base = 0x20 + i * 0x10;
            self.bg_ref[i][0] += self.reg(base + 2) as i16 as i32;
            self.bg_ref[i][1] += self.reg(base + 6) as i16 as i32;
        }
    }

    /// Apply MASTER_BRIGHT and convert a finished line to RGBA8888.
    pub fn finish_line(&self, line: &[u16], out: &mut [u32]) {
        let mb = self.reg(0x6C);
        let factor = (mb & 0x1F).min(16) as u32;
        let mode = mb >> 14;
        for (o, &c) in out.iter_mut().zip(line.iter()) {
            let c = match mode {
                1 if factor > 0 => brighten(c, factor),
                2 if factor > 0 => darken(c, factor),
                _ => c,
            };
            *o = bgr555_to_rgba(c);
        }
    }

    fn render_3d_layer(&mut self, layer3d: Option<&[u32]>) {
        let hofs = (self.reg(0x10) as i32) << 23 >> 23; // 9-bit signed
        let mut line = [TRANSPARENT; WIDTH];
        if let Some(src) = layer3d {
            for (x, out) in line.iter_mut().enumerate() {
                let sx = x as i32 + hofs;
                if !(0..WIDTH as i32).contains(&sx) {
                    continue;
                }
                let p = src[sx as usize];
                if (p >> 16) & 0x1F != 0 {
                    *out = p;
                }
            }
        }
        self.bg_lines[0] = line;
    }

    fn char_screen_base(&self, cnt: usize) -> (u32, u32) {
        let dispcnt = self.dispcnt();
        let (cb, sb) = if self.id == EngineId::A {
            (((dispcnt >> 24) & 7) * 0x10000, ((dispcnt >> 27) & 7) * 0x10000)
        } else {
            (0, 0)
        };
        (cb + ((cnt as u32 >> 2) & 0xF) * 0x4000, sb + ((cnt as u32 >> 8) & 0x1F) * 0x800)
    }

    fn ext_color(&self, vram: &Vram, slot: usize, index: usize) -> u16 {
        let (_, _, bgx, _) = self.views();
        vram.read16(bgx, (slot * 0x2000 + index * 2) as u32) & 0x7FFF
    }

    fn render_text(&mut self, n: usize, y: usize, vram: &Vram, palette: &[u8]) {
        let (bg, _, _, _) = self.views();
        let cnt = self.reg(0x08 + n * 2) as usize;
        let hofs = self.reg(0x10 + n * 4) as usize & 0x1FF;
        let vofs = self.reg(0x12 + n * 4) as usize & 0x1FF;
        let (char_base, screen_base) = self.char_screen_base(cnt);
        let color256 = cnt & 0x80 != 0;
        let ext = color256 && self.dispcnt() & (1 << 30) != 0;
        let slot = if n < 2 && cnt & 0x2000 != 0 { n + 2 } else { n };
        let (w, h) = match cnt >> 14 {
            0 => (256, 256),
            1 => (512, 256),
            2 => (256, 512),
            _ => (512, 512),
        };
        let mosaic = cnt & 0x40 != 0;
        let (mh, mv, _, _) = self.mosaic();
        let y = if mosaic { y - y % mv } else { y };
        let py = (y + vofs) & (h - 1);
        let mut line = [TRANSPARENT; WIDTH];
        let mut cached = usize::MAX;
        let mut entry = 0u16;
        for (x, out) in line.iter_mut().enumerate() {
            let sx = if mosaic { x - x % mh } else { x };
            let px = (sx + hofs) & (w - 1);
            let key = (px >> 3) | (py >> 3) << 8;
            if key != cached {
                cached = key;
                let block = (px >> 8) + (py >> 8) * (w >> 8);
                let addr = screen_base as usize + block * 0x800 + (((py & 255) >> 3) * 32 + ((px & 255) >> 3)) * 2;
                entry = vram.read16(bg, addr as u32);
            }
            let tile = (entry & 0x3FF) as usize;
            let tx = if entry & 0x400 != 0 { 7 - (px & 7) } else { px & 7 };
            let ty = if entry & 0x800 != 0 { 7 - (py & 7) } else { py & 7 };
            let pal_num = (entry >> 12) as usize;
            if color256 {
                let idx = vram.read8(bg, char_base + (tile * 64 + ty * 8 + tx) as u32) as usize;
                if idx != 0 {
                    *out = if ext { self.ext_color(vram, slot, pal_num * 256 + idx) } else { rd16(palette, idx * 2) & 0x7FFF } as u32;
                }
            } else {
                let b = vram.read8(bg, char_base + (tile * 32 + ty * 4 + tx / 2) as u32);
                let idx = if tx & 1 != 0 { b >> 4 } else { b & 0xF } as usize;
                if idx != 0 {
                    *out = (rd16(palette, (pal_num * 16 + idx) * 2) & 0x7FFF) as u32;
                }
            }
        }
        self.bg_lines[n] = line;
    }

    /// Affine (8-bit map) or extended (16-bit map / bitmap) BG.
    fn render_affine(&mut self, n: usize, vram: &Vram, palette: &[u8], extended: bool) {
        let (bg, _, _, _) = self.views();
        let cnt = self.reg(0x08 + n * 2) as usize;
        let (char_base, screen_base) = self.char_screen_base(cnt);
        let wrap = cnt & 0x2000 != 0;
        let base = 0x20 + (n - 2) * 0x10;
        let pa = self.reg(base) as i16 as i32;
        let pc = self.reg(base + 4) as i16 as i32;
        let [mut fx, mut fy] = self.bg_ref[n - 2];
        let ext_pal = self.dispcnt() & (1 << 30) != 0;
        let bitmap = extended && cnt & 0x80 != 0;
        let direct = bitmap && cnt & 4 != 0;
        let (w, h) = if bitmap {
            match cnt >> 14 {
                0 => (128, 128),
                1 => (256, 256),
                2 => (512, 256),
                _ => (512, 512),
            }
        } else {
            let s = 128i32 << (cnt >> 14);
            (s, s)
        };
        let bitmap_base = ((cnt as u32 >> 8) & 0x1F) * 0x4000;
        let mut line = [TRANSPARENT; WIDTH];
        for out in line.iter_mut() {
            let (mut tx, mut ty) = (fx >> 8, fy >> 8);
            fx += pa;
            fy += pc;
            if wrap {
                tx = tx.rem_euclid(w);
                ty = ty.rem_euclid(h);
            } else if tx < 0 || ty < 0 || tx >= w || ty >= h {
                continue;
            }
            let (tx, ty) = (tx as u32, ty as u32);
            if bitmap {
                if direct {
                    let c = vram.read16(bg, bitmap_base + (ty * w as u32 + tx) * 2);
                    if c & 0x8000 != 0 {
                        *out = (c & 0x7FFF) as u32;
                    }
                } else {
                    let idx = vram.read8(bg, bitmap_base + ty * w as u32 + tx) as usize;
                    if idx != 0 {
                        *out = (rd16(palette, idx * 2) & 0x7FFF) as u32;
                    }
                }
            } else if extended {
                let map = screen_base + ((ty >> 3) * (w as u32 >> 3) + (tx >> 3)) * 2;
                let entry = vram.read16(bg, map);
                let tile = (entry & 0x3FF) as u32;
                let px = if entry & 0x400 != 0 { 7 - (tx & 7) } else { tx & 7 };
                let py = if entry & 0x800 != 0 { 7 - (ty & 7) } else { ty & 7 };
                let idx = vram.read8(bg, char_base + tile * 64 + py * 8 + px) as usize;
                if idx != 0 {
                    *out = if ext_pal {
                        self.ext_color(vram, n, (entry >> 12) as usize * 256 + idx)
                    } else {
                        rd16(palette, idx * 2) & 0x7FFF
                    } as u32;
                }
            } else {
                let map = screen_base + (ty >> 3) * (w as u32 >> 3) + (tx >> 3);
                let tile = vram.read8(bg, map) as u32;
                let idx = vram.read8(bg, char_base + tile * 64 + (ty & 7) * 8 + (tx & 7)) as usize;
                if idx != 0 {
                    *out = (rd16(palette, idx * 2) & 0x7FFF) as u32;
                }
            }
        }
        self.bg_lines[n] = line;
    }

    /// Mode 6 large 256-colour bitmap on BG2 (512x1024 or 1024x512).
    fn render_large(&mut self, vram: &Vram, palette: &[u8]) {
        let (bg, _, _, _) = self.views();
        let cnt = self.reg(0x0C) as usize;
        let (w, h) = if cnt >> 14 == 0 { (512, 1024) } else { (1024, 512) };
        let wrap = cnt & 0x2000 != 0;
        let pa = self.reg(0x20) as i16 as i32;
        let pc = self.reg(0x24) as i16 as i32;
        let [mut fx, mut fy] = self.bg_ref[0];
        let mut line = [TRANSPARENT; WIDTH];
        for out in line.iter_mut() {
            let (mut tx, mut ty) = (fx >> 8, fy >> 8);
            fx += pa;
            fy += pc;
            if wrap {
                tx = tx.rem_euclid(w);
                ty = ty.rem_euclid(h);
            } else if tx < 0 || ty < 0 || tx >= w || ty >= h {
                continue;
            }
            let idx = vram.read8(bg, (ty * w + tx) as u32) as usize;
            if idx != 0 {
                *out = (rd16(palette, idx * 2) & 0x7FFF) as u32;
            }
        }
        self.bg_lines[2] = line;
    }

    fn render_objects(&mut self, y: usize, vram: &Vram, palette: &[u8], oam: &[u8]) {
        const SIZES: [[(i32, i32); 4]; 3] = [
            [(8, 8), (16, 16), (32, 32), (64, 64)],
            [(16, 8), (32, 8), (32, 16), (64, 32)],
            [(8, 16), (8, 32), (16, 32), (32, 64)],
        ];
        let (_, obj, _, objx) = self.views();
        let dispcnt = self.dispcnt();
        let tile_1d = dispcnt & 0x10 != 0;
        let boundary_shift = 5 + ((dispcnt >> 20) & 3);
        let bmp_1d = dispcnt & 0x40 != 0;
        let bmp_wide = dispcnt & 0x20 != 0;
        let bmp_boundary = 7 + ((dispcnt >> 22) & 1);
        let ext_pal = dispcnt & (1 << 31) != 0;
        let (_, _, omh, omv) = self.mosaic();
        for i in 0..128 {
            let o = i * 8;
            let attr0 = rd16(oam, o);
            let attr1 = rd16(oam, o + 2);
            let attr2 = rd16(oam, o + 4);
            let affine = attr0 & 0x100 != 0;
            if !affine && attr0 & 0x200 != 0 {
                continue;
            }
            let mode = (attr0 >> 10) & 3;
            let shape = (attr0 >> 14) as usize;
            if shape == 3 {
                continue;
            }
            let (w, h) = SIZES[shape][(attr1 >> 14) as usize];
            let double = affine && attr0 & 0x200 != 0;
            let (bw, bh) = if double { (w * 2, h * 2) } else { (w, h) };
            let sy = (attr0 & 0xFF) as i32;
            let mut sx = (attr1 & 0x1FF) as i32;
            if sx >= 256 {
                sx -= 512;
            }
            let mosaic = attr0 & 0x1000 != 0;
            let line_y = if mosaic { y - y % omv } else { y } as i32;
            let dy = (line_y - sy) & 0xFF;
            if dy >= bh {
                continue;
            }
            let color256 = attr0 & 0x2000 != 0;
            let tile = (attr2 & 0x3FF) as u32;
            let prio = ((attr2 >> 10) & 3) as u8;
            let pal = (attr2 >> 12) as usize;
            let (pa, pb, pc, pd) = if affine {
                let g = ((attr1 >> 9) & 0x1F) as usize * 32;
                (
                    rd16(oam, g + 6) as i16 as i32,
                    rd16(oam, g + 14) as i16 as i32,
                    rd16(oam, g + 22) as i16 as i32,
                    rd16(oam, g + 30) as i16 as i32,
                )
            } else {
                (0, 0, 0, 0)
            };
            for bx in 0..bw {
                let x = sx + bx;
                if !(0..WIDTH as i32).contains(&x) {
                    continue;
                }
                let xb = if mosaic { bx - (x % omh as i32).min(bx) } else { bx };
                let (tx, ty) = if affine {
                    let ix = xb - bw / 2;
                    let iy = dy - bh / 2;
                    (((pa * ix + pb * iy) >> 8) + w / 2, ((pc * ix + pd * iy) >> 8) + h / 2)
                } else {
                    (
                        if attr1 & 0x1000 != 0 { w - 1 - xb } else { xb },
                        if attr1 & 0x2000 != 0 { h - 1 - dy } else { dy },
                    )
                };
                if tx < 0 || ty < 0 || tx >= w || ty >= h {
                    continue;
                }
                let (tx, ty) = (tx as u32, ty as u32);
                let px = x as usize;
                if mode == 3 {
                    // Bitmap sprite: 16-bit direct colour, bit 15 = opaque.
                    let addr = if bmp_1d {
                        (tile << bmp_boundary) + (ty * w as u32 + tx) * 2
                    } else {
                        let row_tiles = if bmp_wide { 0x20 } else { 0x10 };
                        let mask = row_tiles - 1;
                        ((tile & mask) * 0x10 + (tile & !mask) * 0x80) + (ty * row_tiles * 8 + tx) * 2
                    };
                    let c = vram.read16(obj, addr);
                    if c & 0x8000 == 0 {
                        continue;
                    }
                    let cur = self.obj_line[px];
                    if cur.opaque && cur.prio <= prio {
                        continue;
                    }
                    self.obj_line[px] =
                        ObjPixel { color: c & 0x7FFF, prio, semi: true, alpha: pal as u8 + 1, opaque: true };
                    continue;
                }
                let tile_addr = if tile_1d {
                    let per_row = (w as u32 / 8) * if color256 { 2 } else { 1 };
                    (tile << boundary_shift) + ((ty / 8) * per_row + (tx / 8) * if color256 { 2 } else { 1 }) * 32
                } else {
                    let t = if color256 { tile & !1 } else { tile };
                    (t + (ty / 8) * 32 + (tx / 8) * if color256 { 2 } else { 1 }) * 32
                };
                let (fx, fy) = (tx & 7, ty & 7);
                let idx = if color256 {
                    vram.read8(obj, tile_addr + fy * 8 + fx) as usize
                } else {
                    let b = vram.read8(obj, tile_addr + fy * 4 + fx / 2);
                    (if fx & 1 != 0 { b >> 4 } else { b & 0xF }) as usize
                };
                if idx == 0 {
                    continue;
                }
                if mode == 2 {
                    self.obj_window[px] = true;
                    continue;
                }
                let cur = self.obj_line[px];
                if cur.opaque && cur.prio <= prio {
                    continue;
                }
                let color = if color256 {
                    if ext_pal {
                        vram.read16(objx, ((pal * 256 + idx) * 2) as u32) & 0x7FFF
                    } else {
                        rd16(palette, 0x200 + idx * 2) & 0x7FFF
                    }
                } else {
                    rd16(palette, 0x200 + (pal * 16 + idx) * 2) & 0x7FFF
                };
                self.obj_line[px] = ObjPixel { color, prio, semi: mode == 1, alpha: 16, opaque: true };
            }
        }
    }

    fn compose(&mut self, y: usize, active: &[bool; 4], palette: &[u8], out: &mut [u16]) {
        let dispcnt = self.dispcnt();
        let any_window = dispcnt & 0xE000 != 0;
        let inside = |reg: u16, v: usize| {
            let a = (reg >> 8) as usize;
            let b = (reg & 0xFF) as usize;
            if a <= b {
                v >= a && v < b
            } else {
                v >= a || v < b
            }
        };
        let w0 = dispcnt & 0x2000 != 0 && inside(self.reg(0x44), y);
        let w1 = dispcnt & 0x4000 != 0 && inside(self.reg(0x46), y);
        let wobj = dispcnt & 0x8000 != 0;
        let (winin, winout, win0h, win1h) = (self.reg(0x48), self.reg(0x4A), self.reg(0x40), self.reg(0x42));
        let mut sorted: Vec<(usize, u16)> =
            (0..4).filter(|&n| active[n]).map(|n| (n, self.reg(0x08 + n * 2) & 3)).collect();
        sorted.sort_by_key(|&(n, p)| (p, n));
        let bldcnt = self.reg(0x50);
        let effect = (bldcnt >> 6) & 3;
        let alpha = self.reg(0x52);
        let eva = (alpha & 0x1F).min(16) as u32;
        let evb = ((alpha >> 8) & 0x1F).min(16) as u32;
        let evy = (self.reg(0x54) & 0x1F).min(16) as u32;
        let backdrop = rd16(palette, 0) & 0x7FFF;
        let obj_on = dispcnt & 0x1000 != 0;

        for (x, o) in out.iter_mut().enumerate().take(WIDTH) {
            let mask = if !any_window {
                0x3F
            } else if w0 && inside(win0h, x) {
                winin & 0x3F
            } else if w1 && inside(win1h, x) {
                (winin >> 8) & 0x3F
            } else if wobj && self.obj_window[x] {
                (winout >> 8) & 0x3F
            } else {
                winout & 0x3F
            };
            let mut layers = [(LAYER_BD, backdrop as u32); 2];
            let mut n = 0;
            let obj = self.obj_line[x];
            let obj_ok = obj_on && obj.opaque && mask & 0x10 != 0;
            let mut obj_placed = !obj_ok;
            for &(bg, prio) in &sorted {
                if n == 2 {
                    break;
                }
                if !obj_placed && obj.prio as u16 <= prio {
                    layers[n] = (LAYER_OBJ, obj.color as u32);
                    n += 1;
                    obj_placed = true;
                    if n == 2 {
                        break;
                    }
                }
                let c = self.bg_lines[bg][x];
                if mask & (1 << bg) != 0 && c & TRANSPARENT == 0 {
                    layers[n] = (bg, c);
                    n += 1;
                }
            }
            if n < 2 && !obj_placed {
                layers[n] = (LAYER_OBJ, obj.color as u32);
            }
            let (top_layer, top_raw) = layers[0];
            let (second_layer, second_raw) = layers[1];
            let top = (top_raw & 0x7FFF) as u16;
            let second = (second_raw & 0x7FFF) as u16;
            let second_target = (bldcnt >> 8) & (1 << second_layer) != 0;
            let first_target = bldcnt & (1 << top_layer) != 0;
            let is_3d = top_layer == 0 && self.id == EngineId::A && dispcnt & 8 != 0;
            let color = if mask & 0x20 == 0 {
                top
            } else if is_3d && second_target {
                // 3D pixels blend using their own alpha.
                let a = (top_raw >> 16) & 0x1F;
                if a >= 31 {
                    top
                } else {
                    blend32(top, second, a + 1, 31 - a)
                }
            } else if top_layer == LAYER_OBJ && obj.semi && second_target {
                if obj.alpha < 16 {
                    blend(top, second, obj.alpha as u32, 16 - obj.alpha as u32)
                } else {
                    blend(top, second, eva, evb)
                }
            } else {
                match effect {
                    1 if first_target && second_target => blend(top, second, eva, evb),
                    2 if first_target => brighten(top, evy),
                    3 if first_target => darken(top, evy),
                    _ => top,
                }
            };
            *o = color;
        }
    }
}

enum BgKind {
    Text,
    Affine,
    Extended,
    Large,
}

#[inline]
fn split(c: u16) -> (u32, u32, u32) {
    ((c & 0x1F) as u32, ((c >> 5) & 0x1F) as u32, ((c >> 10) & 0x1F) as u32)
}

#[inline]
fn join(r: u32, g: u32, b: u32) -> u16 {
    (r.min(31) | g.min(31) << 5 | b.min(31) << 10) as u16
}

/// Blend with weights out of 16.
fn blend(a: u16, b: u16, eva: u32, evb: u32) -> u16 {
    blend_shift(a, b, eva, evb, 4)
}

/// 3D layer blending uses the polygon's 5-bit alpha (weights out of 32).
fn blend32(a: u16, b: u16, wa: u32, wb: u32) -> u16 {
    blend_shift(a, b, wa, wb, 5)
}

fn blend_shift(a: u16, b: u16, wa: u32, wb: u32, shift: u32) -> u16 {
    let (ar, ag, ab) = split(a);
    let (br, bg, bb) = split(b);
    join((ar * wa + br * wb) >> shift, (ag * wa + bg * wb) >> shift, (ab * wa + bb * wb) >> shift)
}

fn brighten(c: u16, evy: u32) -> u16 {
    let (r, g, b) = split(c);
    join(r + (((31 - r) * evy) >> 4), g + (((31 - g) * evy) >> 4), b + (((31 - b) * evy) >> 4))
}

fn darken(c: u16, evy: u32) -> u16 {
    let (r, g, b) = split(c);
    join(r - ((r * evy) >> 4), g - ((g * evy) >> 4), b - ((b * evy) >> 4))
}

#[inline]
pub fn bgr555_to_rgba(c: u16) -> u32 {
    let (r, g, b) = split(c);
    let ex = |v: u32| (v << 3) | (v >> 2);
    ex(r) | ex(g) << 8 | ex(b) << 16 | 0xFF00_0000
}

impl SaveState for Engine2d {
    fn save_state(&self, w: &mut StateWriter) {
        w.u16s(&self.regs);
        for r in &self.bg_ref {
            w.i32(r[0]);
            w.i32(r[1]);
        }
    }
    fn load_state(&mut self, r: &mut StateReader<'_>) -> StateResult<()> {
        r.u16s_into(&mut self.regs)?;
        for i in 0..2 {
            self.bg_ref[i] = [r.i32()?, r.i32()?];
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_bg_through_vram_mapping() {
        let mut vram = Vram::new();
        vram.set_cnt(0, 0x81); // A → ABG 0
        let mut e = Engine2d::new(EngineId::A);
        e.write16(0x00, 0x0100); // mode 0, BG0
        e.write16(0x02, 0x0001); // display mode 1
        e.write16(0x08, 0x0100); // screen base 0x800
        let mut pal = vec![0u8; 0x400];
        pal[2..4].copy_from_slice(&0x001Fu16.to_le_bytes());
        for i in 32..64 {
            vram.write8(View::ABg, i, 0x11);
        }
        vram.write16(View::ABg, 0x800, 1);
        let oam = vec![0u8; 0x400];
        let mut line = [0u16; WIDTH];
        e.render_line(0, &vram, &pal, &oam, None, &mut line);
        assert_eq!(line[0], 0x001F);
        assert_eq!(line[8], 0);
    }

    #[test]
    fn three_d_layer_on_bg0() {
        let vram = Vram::new();
        let mut e = Engine2d::new(EngineId::A);
        e.write16(0x00, 0x0108); // BG0 on, BG0 = 3D
        e.write16(0x02, 0x0001);
        let pal = vec![0u8; 0x400];
        let oam = vec![0u8; 0x400];
        let mut src = [0u32; WIDTH];
        src[100] = 0x03E0 | 31 << 16;
        let mut line = [0u16; WIDTH];
        e.render_line(0, &vram, &pal, &oam, Some(&src), &mut line);
        assert_eq!(line[100], 0x03E0);
        assert_eq!(line[99], 0);
    }

    #[test]
    fn display_off_is_white_and_brightness() {
        let vram = Vram::new();
        let mut e = Engine2d::new(EngineId::B);
        let pal = vec![0u8; 0x400];
        let oam = vec![0u8; 0x400];
        let mut line = [0u16; WIDTH];
        e.render_line(0, &vram, &pal, &oam, None, &mut line);
        assert!(line.iter().all(|&c| c == 0x7FFF));
        e.write16(0x6C, 0x8000 | 16); // fully darken
        let mut out = [0u32; WIDTH];
        e.finish_line(&line, &mut out);
        assert!(out.iter().all(|&p| p == 0xFF00_0000));
    }
}
