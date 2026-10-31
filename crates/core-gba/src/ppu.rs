//! Scanline PPU: text and affine backgrounds, bitmap modes 3–5, regular
//! and affine sprites, windows (WIN0/WIN1/OBJ window), mosaic and colour
//! special effects (alpha blending, brighten, darken).

use kd_core::{SaveState, StateReader, StateResult, StateWriter};

pub const WIDTH: usize = 240;
pub const HEIGHT: usize = 160;

/// Marks a transparent pixel in 15-bit BGR line buffers.
const TRANSPARENT: u16 = 0x8000;

const LAYER_OBJ: usize = 4;
const LAYER_BD: usize = 5;

#[derive(Debug, Clone, Copy, Default)]
struct ObjPixel {
    color: u16,
    prio: u8,
    semi: bool,
}

pub struct Ppu {
    pub vram: Box<[u8]>,
    pub palette: Box<[u8]>,
    pub oam: Box<[u8]>,
    /// Raw register file for 0x04000000..0x04000060 (halfword indexed).
    pub regs: [u16; 0x30],
    pub vcount: u16,
    /// Writable DISPSTAT bits (IRQ enables and VCount setting).
    pub dispstat: u16,
    pub in_hblank: bool,
    pub in_vblank: bool,
    /// Internal BG2/BG3 affine reference points (20.8 fixed point).
    bg_ref: [[i32; 2]; 2],
    /// Finished frame, RGBA8888.
    pub framebuffer: Vec<u32>,
    bg_lines: [[u16; WIDTH]; 4],
    obj_line: [ObjPixel; WIDTH],
    obj_window: [bool; WIDTH],
    color_line: [u16; WIDTH],
}

impl Default for Ppu {
    fn default() -> Self {
        Self::new()
    }
}

#[inline]
fn rd16(mem: &[u8], addr: usize) -> u16 {
    u16::from_le_bytes([mem[addr], mem[addr + 1]])
}

impl Ppu {
    pub fn new() -> Self {
        let mut regs = [0u16; 0x30];
        // Identity affine matrices for BG2/BG3 after reset.
        for off in [0x20, 0x26, 0x30, 0x36] {
            regs[off / 2] = 0x100;
        }
        Self {
            vram: vec![0; 0x18000].into_boxed_slice(),
            palette: vec![0; 0x400].into_boxed_slice(),
            oam: vec![0; 0x400].into_boxed_slice(),
            regs,
            vcount: 0,
            dispstat: 0,
            in_hblank: false,
            in_vblank: false,
            bg_ref: [[0; 2]; 2],
            framebuffer: vec![0xFF00_0000; WIDTH * HEIGHT],
            bg_lines: [[TRANSPARENT; WIDTH]; 4],
            obj_line: [ObjPixel::default(); WIDTH],
            obj_window: [false; WIDTH],
            color_line: [0; WIDTH],
        }
    }

    #[inline]
    fn reg(&self, offset: usize) -> u16 {
        self.regs[offset >> 1]
    }

    pub fn dispcnt(&self) -> u16 {
        self.regs[0]
    }

    /// DISPSTAT as read by the CPU.
    pub fn read_dispstat(&self) -> u16 {
        let vc_setting = self.dispstat >> 8;
        self.dispstat & 0xFF38
            | self.in_vblank as u16
            | (self.in_hblank as u16) << 1
            | ((self.vcount == vc_setting) as u16) << 2
    }

    pub fn read16(&self, offset: u32) -> u16 {
        match offset {
            0x00 | 0x02 => self.reg(offset as usize),
            0x04 => self.read_dispstat(),
            0x06 => self.vcount,
            0x08..=0x0E => self.reg(offset as usize),
            0x48 | 0x4A | 0x50 | 0x52 => self.reg(offset as usize),
            _ => 0, // write-only registers
        }
    }

    pub fn write16(&mut self, offset: u32, value: u16) {
        let o = offset as usize;
        match offset {
            0x04 => self.dispstat = value & 0xFF38,
            0x06 => {}
            _ if o < 0x60 => {
                self.regs[o >> 1] = value;
                if (0x28..0x30).contains(&offset) {
                    self.reload_ref(0);
                } else if (0x38..0x40).contains(&offset) {
                    self.reload_ref(1);
                }
            }
            _ => {}
        }
    }

    fn reload_ref(&mut self, i: usize) {
        let base = 0x28 + i * 0x10;
        for k in 0..2 {
            let lo = self.reg(base + k * 4) as u32;
            let hi = self.reg(base + k * 4 + 2) as u32;
            // 28-bit signed
            self.bg_ref[i][k] = (((hi << 16 | lo) << 4) as i32) >> 4;
        }
    }

    /// Called at the start of VBlank: affine references reload.
    pub fn on_vblank(&mut self) {
        self.reload_ref(0);
        self.reload_ref(1);
    }

    #[inline]
    fn bg_color(&self, idx: usize) -> u16 {
        rd16(&self.palette, idx * 2) & 0x7FFF
    }

    #[inline]
    fn obj_color(&self, idx: usize) -> u16 {
        rd16(&self.palette, 0x200 + idx * 2) & 0x7FFF
    }

    fn mosaic(&self) -> (usize, usize, usize, usize) {
        let m = self.reg(0x4C) as usize;
        ((m & 0xF) + 1, ((m >> 4) & 0xF) + 1, ((m >> 8) & 0xF) + 1, ((m >> 12) & 0xF) + 1)
    }

    /// Render scanline `y` into the framebuffer.
    pub fn render_line(&mut self, y: usize) {
        if y >= HEIGHT {
            return;
        }
        let dispcnt = self.dispcnt();
        let row = &mut self.framebuffer[y * WIDTH..(y + 1) * WIDTH];
        if dispcnt & 0x80 != 0 {
            row.fill(0xFFFF_FFFF);
            self.advance_refs();
            return;
        }
        let mode = dispcnt & 7;
        let mut active: [bool; 4] = [false; 4];
        for (n, a) in active.iter_mut().enumerate() {
            *a = dispcnt & (0x100 << n) != 0
                && match mode {
                    0 => true,
                    1 => n <= 2,
                    2 => n >= 2,
                    3..=5 => n == 2,
                    _ => false,
                };
        }
        for (n, &on) in active.iter().enumerate() {
            if !on {
                continue;
            }
            match (mode, n) {
                (0, _) | (1, 0) | (1, 1) => self.render_text_bg(n, y),
                (1, 2) | (2, _) => self.render_affine_bg(n),
                (3, _) => self.render_bitmap(3),
                (4, _) => self.render_bitmap(4),
                (5, _) => self.render_bitmap(5),
                _ => {}
            }
        }
        self.obj_line.fill(ObjPixel { color: TRANSPARENT, prio: 4, semi: false });
        self.obj_window.fill(false);
        if dispcnt & 0x1000 != 0 {
            self.render_objects(y, mode >= 3, dispcnt & 0x40 != 0);
        }
        self.compose(y, &active);
        self.advance_refs();
        let row = &mut self.framebuffer[y * WIDTH..(y + 1) * WIDTH];
        convert_line(&self.color_line, row);
    }

    /// Affine reference points step by (PB, PD) after every line.
    fn advance_refs(&mut self) {
        for i in 0..2 {
            let base = 0x20 + i * 0x10;
            self.bg_ref[i][0] += self.reg(base + 2) as i16 as i32;
            self.bg_ref[i][1] += self.reg(base + 6) as i16 as i32;
        }
    }

    fn render_text_bg(&mut self, n: usize, y: usize) {
        let cnt = self.reg(0x08 + n * 2) as usize;
        let hofs = self.reg(0x10 + n * 4) as usize & 0x1FF;
        let vofs = self.reg(0x12 + n * 4) as usize & 0x1FF;
        let char_base = ((cnt >> 2) & 3) * 0x4000;
        let screen_base = ((cnt >> 8) & 0x1F) * 0x800;
        let color256 = cnt & 0x80 != 0;
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
        let blocks_per_row = w >> 8;
        let mut line = [TRANSPARENT; WIDTH];
        let mut cached_tile = usize::MAX;
        let mut entry = 0u16;
        for (x, out) in line.iter_mut().enumerate() {
            let sx = if mosaic { x - x % mh } else { x };
            let px = (sx + hofs) & (w - 1);
            let tile_key = (px >> 3) | (py >> 3) << 8;
            if tile_key != cached_tile {
                cached_tile = tile_key;
                let block = (px >> 8) + (py >> 8) * blocks_per_row;
                let addr = screen_base + block * 0x800 + (((py & 255) >> 3) * 32 + ((px & 255) >> 3)) * 2;
                entry = if addr + 1 < 0x10000 { rd16(&self.vram, addr) } else { 0 };
            }
            let tile = (entry & 0x3FF) as usize;
            let tx = if entry & 0x400 != 0 { 7 - (px & 7) } else { px & 7 };
            let ty = if entry & 0x800 != 0 { 7 - (py & 7) } else { py & 7 };
            let (idx, pal_idx) = if color256 {
                let a = char_base + tile * 64 + ty * 8 + tx;
                let i = if a < 0x10000 { self.vram[a] as usize } else { 0 };
                (i, i)
            } else {
                let a = char_base + tile * 32 + ty * 4 + tx / 2;
                let b = if a < 0x10000 { self.vram[a] } else { 0 };
                let i = if tx & 1 != 0 { b >> 4 } else { b & 0xF } as usize;
                (i, ((entry >> 12) as usize) * 16 + i)
            };
            if idx != 0 {
                *out = self.bg_color(pal_idx);
            }
        }
        self.bg_lines[n] = line;
    }

    fn render_affine_bg(&mut self, n: usize) {
        let cnt = self.reg(0x08 + n * 2) as usize;
        let char_base = ((cnt >> 2) & 3) * 0x4000;
        let screen_base = ((cnt >> 8) & 0x1F) * 0x800;
        let size = 128usize << (cnt >> 14);
        let wrap = cnt & 0x2000 != 0;
        let base = 0x20 + (n - 2) * 0x10;
        let pa = self.reg(base) as i16 as i32;
        let pc = self.reg(base + 4) as i16 as i32;
        let [mut fx, mut fy] = self.bg_ref[n - 2];
        let mut line = [TRANSPARENT; WIDTH];
        for out in line.iter_mut() {
            let (mut tx, mut ty) = (fx >> 8, fy >> 8);
            fx += pa;
            fy += pc;
            if wrap {
                tx &= size as i32 - 1;
                ty &= size as i32 - 1;
            } else if tx < 0 || ty < 0 || tx >= size as i32 || ty >= size as i32 {
                continue;
            }
            let (tx, ty) = (tx as usize, ty as usize);
            let map = screen_base + (ty >> 3) * (size >> 3) + (tx >> 3);
            let tile = if map < 0x10000 { self.vram[map] as usize } else { 0 };
            let a = char_base + tile * 64 + (ty & 7) * 8 + (tx & 7);
            let idx = if a < 0x10000 { self.vram[a] as usize } else { 0 };
            if idx != 0 {
                *out = self.bg_color(idx);
            }
        }
        self.bg_lines[n] = line;
    }

    fn render_bitmap(&mut self, mode: u16) {
        let page = if self.dispcnt() & 0x10 != 0 { 0xA000 } else { 0 };
        let pa = self.reg(0x20) as i16 as i32;
        let pc = self.reg(0x24) as i16 as i32;
        let [mut fx, mut fy] = self.bg_ref[0];
        let (w, h) = if mode == 5 { (160, 128) } else { (240, 160) };
        let mut line = [TRANSPARENT; WIDTH];
        for out in line.iter_mut() {
            let (tx, ty) = (fx >> 8, fy >> 8);
            fx += pa;
            fy += pc;
            if tx < 0 || ty < 0 || tx >= w || ty >= h {
                continue;
            }
            let i = (ty * w + tx) as usize;
            *out = match mode {
                3 => rd16(&self.vram, i * 2) & 0x7FFF,
                4 => {
                    let idx = self.vram[page + i] as usize;
                    if idx == 0 {
                        continue;
                    }
                    self.bg_color(idx)
                }
                _ => rd16(&self.vram, page + i * 2) & 0x7FFF,
            };
        }
        self.bg_lines[2] = line;
    }

    fn render_objects(&mut self, y: usize, bitmap_mode: bool, one_dim: bool) {
        const SIZES: [[(i32, i32); 4]; 3] = [
            [(8, 8), (16, 16), (32, 32), (64, 64)],
            [(16, 8), (32, 8), (32, 16), (64, 32)],
            [(8, 16), (8, 32), (16, 32), (32, 64)],
        ];
        let (_, _, omh, omv) = self.mosaic();
        for i in 0..128 {
            let o = i * 8;
            let attr0 = rd16(&self.oam, o);
            let attr1 = rd16(&self.oam, o + 2);
            let attr2 = rd16(&self.oam, o + 4);
            let affine = attr0 & 0x100 != 0;
            if !affine && attr0 & 0x200 != 0 {
                continue;
            }
            let mode = (attr0 >> 10) & 3;
            let shape = (attr0 >> 14) as usize;
            if mode == 3 || shape == 3 {
                continue;
            }
            let (w, h) = SIZES[shape][(attr1 >> 14) as usize];
            let double = affine && attr0 & 0x200 != 0;
            let (bw, bh) = if double { (w * 2, h * 2) } else { (w, h) };
            let sy = (attr0 & 0xFF) as i32;
            let mut sx = (attr1 & 0x1FF) as i32;
            if sx >= 240 {
                sx -= 512;
            }
            let mosaic = attr0 & 0x1000 != 0;
            let line_y = if mosaic { y - y % omv } else { y } as i32;
            let dy = (line_y - sy) & 0xFF;
            if dy >= bh {
                continue;
            }
            let color256 = attr0 & 0x2000 != 0;
            let tile_base = (attr2 & 0x3FF) as i32;
            let prio = ((attr2 >> 10) & 3) as u8;
            let pal = (attr2 >> 12) as usize;
            let (pa, pb, pc, pd) = if affine {
                let g = ((attr1 >> 9) & 0x1F) as usize * 32;
                (
                    rd16(&self.oam, g + 6) as i16 as i32,
                    rd16(&self.oam, g + 14) as i16 as i32,
                    rd16(&self.oam, g + 22) as i16 as i32,
                    rd16(&self.oam, g + 30) as i16 as i32,
                )
            } else {
                (0, 0, 0, 0)
            };
            let tile_step = if color256 { 2 } else { 1 };
            let row_tiles = if one_dim { (w / 8) * tile_step } else { 32 };
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
                    let tx = if attr1 & 0x1000 != 0 { w - 1 - xb } else { xb };
                    let ty = if attr1 & 0x2000 != 0 { h - 1 - dy } else { dy };
                    (tx, ty)
                };
                if tx < 0 || ty < 0 || tx >= w || ty >= h {
                    continue;
                }
                let base_tile = if color256 && !one_dim { tile_base & !1 } else { tile_base };
                let tile = (base_tile + (ty / 8) * row_tiles + (tx / 8) * tile_step) & 0x3FF;
                if bitmap_mode && tile < 512 {
                    continue;
                }
                let (tx, ty) = (tx as usize & 7, ty as usize & 7);
                let a = 0x10000 + tile as usize * 32;
                let idx = if color256 {
                    self.vram[(a + ty * 8 + tx) % 0x18000] as usize
                } else {
                    let b = self.vram[(a + ty * 4 + tx / 2) % 0x18000];
                    (if tx & 1 != 0 { b >> 4 } else { b & 0xF }) as usize
                };
                if idx == 0 {
                    continue;
                }
                let x = x as usize;
                if mode == 2 {
                    self.obj_window[x] = true;
                    continue;
                }
                let cur = self.obj_line[x];
                if cur.color != TRANSPARENT && cur.prio <= prio {
                    continue;
                }
                let color = if color256 { self.obj_color(idx) } else { self.obj_color(pal * 16 + idx) };
                self.obj_line[x] = ObjPixel { color, prio, semi: mode == 1 };
            }
        }
    }

    fn window_masks(&self, y: usize) -> (bool, bool, bool) {
        let dispcnt = self.dispcnt();
        let inside_v = |reg: u16| {
            let top = (reg >> 8) as usize;
            let bottom = (reg & 0xFF) as usize;
            if top <= bottom {
                y >= top && y < bottom
            } else {
                y >= top || y < bottom
            }
        };
        (
            dispcnt & 0x2000 != 0 && inside_v(self.reg(0x44)),
            dispcnt & 0x4000 != 0 && inside_v(self.reg(0x46)),
            dispcnt & 0x8000 != 0,
        )
    }

    fn compose(&mut self, y: usize, active: &[bool; 4]) {
        let dispcnt = self.dispcnt();
        let any_window = dispcnt & 0xE000 != 0;
        let (w0, w1, wobj) = self.window_masks(y);
        let winin = self.reg(0x48);
        let winout = self.reg(0x4A);
        let inside_h = |reg: u16, x: usize| {
            let left = (reg >> 8) as usize;
            let right = (reg & 0xFF) as usize;
            if left <= right {
                x >= left && x < right
            } else {
                x >= left || x < right
            }
        };
        let win0h = self.reg(0x40);
        let win1h = self.reg(0x42);

        let mut sorted: Vec<(usize, u16)> =
            (0..4).filter(|&n| active[n]).map(|n| (n, self.reg(0x08 + n * 2) & 3)).collect();
        sorted.sort_by_key(|&(n, p)| (p, n));

        let bldcnt = self.reg(0x50);
        let effect = (bldcnt >> 6) & 3;
        let alpha = self.reg(0x52);
        let eva = (alpha & 0x1F).min(16) as u32;
        let evb = ((alpha >> 8) & 0x1F).min(16) as u32;
        let evy = (self.reg(0x54) & 0x1F).min(16) as u32;
        let backdrop = self.bg_color(0);
        let obj_on = dispcnt & 0x1000 != 0;

        for x in 0..WIDTH {
            let mask = if !any_window {
                0x3F
            } else if w0 && inside_h(win0h, x) {
                winin & 0x3F
            } else if w1 && inside_h(win1h, x) {
                (winin >> 8) & 0x3F
            } else if wobj && self.obj_window[x] {
                (winout >> 8) & 0x3F
            } else {
                winout & 0x3F
            };

            let mut layers = [(LAYER_BD, backdrop); 2];
            let mut n = 0;
            let obj = self.obj_line[x];
            let obj_ok = obj_on && obj.color != TRANSPARENT && mask & 0x10 != 0;
            let mut obj_placed = !obj_ok;
            for &(bg, prio) in &sorted {
                if n == 2 {
                    break;
                }
                if !obj_placed && obj.prio as u16 <= prio {
                    layers[n] = (LAYER_OBJ, obj.color);
                    n += 1;
                    obj_placed = true;
                    if n == 2 {
                        break;
                    }
                }
                let c = self.bg_lines[bg][x];
                if mask & (1 << bg) != 0 && c != TRANSPARENT {
                    layers[n] = (bg, c);
                    n += 1;
                }
            }
            if n < 2 && !obj_placed {
                layers[n] = (LAYER_OBJ, obj.color);
            }

            let (top_layer, top) = layers[0];
            let (second_layer, second) = layers[1];
            let first_target = bldcnt & (1 << top_layer) != 0;
            let second_target = (bldcnt >> 8) & (1 << second_layer) != 0;
            let semi = top_layer == LAYER_OBJ && obj.semi;
            let color = if mask & 0x20 == 0 {
                top
            } else if semi && second_target {
                blend(top, second, eva, evb)
            } else {
                match effect {
                    1 if first_target && second_target => blend(top, second, eva, evb),
                    2 if first_target => brighten(top, evy),
                    3 if first_target => darken(top, evy),
                    _ => top,
                }
            };
            self.color_line[x] = color;
        }
    }
}

#[inline]
fn split(c: u16) -> (u32, u32, u32) {
    ((c & 0x1F) as u32, ((c >> 5) & 0x1F) as u32, ((c >> 10) & 0x1F) as u32)
}

#[inline]
fn join(r: u32, g: u32, b: u32) -> u16 {
    (r.min(31) | g.min(31) << 5 | b.min(31) << 10) as u16
}

fn blend(a: u16, b: u16, eva: u32, evb: u32) -> u16 {
    let (ar, ag, ab) = split(a);
    let (br, bg, bb) = split(b);
    join((ar * eva + br * evb) >> 4, (ag * eva + bg * evb) >> 4, (ab * eva + bb * evb) >> 4)
}

fn brighten(c: u16, evy: u32) -> u16 {
    let (r, g, b) = split(c);
    join(r + (((31 - r) * evy) >> 4), g + (((31 - g) * evy) >> 4), b + (((31 - b) * evy) >> 4))
}

fn darken(c: u16, evy: u32) -> u16 {
    let (r, g, b) = split(c);
    join(r - ((r * evy) >> 4), g - ((g * evy) >> 4), b - ((b * evy) >> 4))
}

/// BGR555 → RGBA8888 for one scanline (NEON on AArch64).
pub fn convert_line(src: &[u16; WIDTH], dst: &mut [u32]) {
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: NEON is mandatory on AArch64; 240 is a multiple of 8 and
        // both slices hold 240 elements.
        unsafe { convert_line_neon(src, dst) }
    }
    #[cfg(not(target_arch = "aarch64"))]
    convert_line_scalar(src, dst);
}

#[inline]
pub fn bgr555_to_rgba(c: u16) -> u32 {
    let (r, g, b) = split(c);
    let ex = |v: u32| (v << 3) | (v >> 2);
    ex(r) | ex(g) << 8 | ex(b) << 16 | 0xFF00_0000
}

#[allow(dead_code)]
fn convert_line_scalar(src: &[u16; WIDTH], dst: &mut [u32]) {
    for (d, &s) in dst.iter_mut().zip(src.iter()) {
        *d = bgr555_to_rgba(s);
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn convert_line_neon(src: &[u16; WIDTH], dst: &mut [u32]) {
    use core::arch::aarch64::*;
    assert!(dst.len() >= WIDTH);
    let mask = vdupq_n_u16(0x1F);
    let alpha = vdup_n_u8(0xFF);
    let out = dst.as_mut_ptr() as *mut u8;
    for i in (0..WIDTH).step_by(8) {
        let v = vld1q_u16(src.as_ptr().add(i));
        let r = vandq_u16(v, mask);
        let g = vandq_u16(vshrq_n_u16::<5>(v), mask);
        let b = vandq_u16(vshrq_n_u16::<10>(v), mask);
        let r8 = vmovn_u16(vorrq_u16(vshlq_n_u16::<3>(r), vshrq_n_u16::<2>(r)));
        let g8 = vmovn_u16(vorrq_u16(vshlq_n_u16::<3>(g), vshrq_n_u16::<2>(g)));
        let b8 = vmovn_u16(vorrq_u16(vshlq_n_u16::<3>(b), vshrq_n_u16::<2>(b)));
        vst4_u8(out.add(i * 4), uint8x8x4_t(r8, g8, b8, alpha));
    }
}

impl SaveState for Ppu {
    fn save_state(&self, w: &mut StateWriter) {
        w.bytes(&self.vram);
        w.bytes(&self.palette);
        w.bytes(&self.oam);
        w.u16s(&self.regs);
        w.u16(self.vcount);
        w.u16(self.dispstat);
        w.bool(self.in_hblank);
        w.bool(self.in_vblank);
        for r in &self.bg_ref {
            w.i32(r[0]);
            w.i32(r[1]);
        }
    }

    fn load_state(&mut self, r: &mut StateReader<'_>) -> StateResult<()> {
        r.bytes_into(&mut self.vram)?;
        r.bytes_into(&mut self.palette)?;
        r.bytes_into(&mut self.oam)?;
        r.u16s_into(&mut self.regs)?;
        self.vcount = r.u16()?;
        self.dispstat = r.u16()?;
        self.in_hblank = r.bool()?;
        self.in_vblank = r.bool()?;
        for i in 0..2 {
            self.bg_ref[i] = [r.i32()?, r.i32()?];
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_pal(p: &mut Ppu, idx: usize, c: u16) {
        p.palette[idx * 2..idx * 2 + 2].copy_from_slice(&c.to_le_bytes());
    }

    #[test]
    fn color_conversion() {
        assert_eq!(bgr555_to_rgba(0x7FFF), 0xFFFF_FFFF);
        assert_eq!(bgr555_to_rgba(0x001F), 0xFF00_00FF);
        assert_eq!(bgr555_to_rgba(0x7C00), 0xFFFF_0000);
        let src = [0x03E0u16; WIDTH];
        let mut dst = [0u32; WIDTH];
        convert_line(&src, &mut dst);
        assert!(dst.iter().all(|&p| p == 0xFF00_FF00));
    }

    #[test]
    fn mode3_bitmap() {
        let mut p = Ppu::new();
        p.write16(0x00, 0x0403); // mode 3, BG2 on
        p.write16(0x20, 0x100); // PA = 1.0
        p.write16(0x26, 0x100); // PD = 1.0
        p.on_vblank();
        p.vram[(5 * 240 + 7) * 2..(5 * 240 + 7) * 2 + 2].copy_from_slice(&0x001Fu16.to_le_bytes());
        for y in 0..HEIGHT {
            p.render_line(y);
        }
        assert_eq!(p.framebuffer[5 * WIDTH + 7], 0xFF00_00FF);
        assert_eq!(p.framebuffer[5 * WIDTH + 8], 0xFF00_0000);
    }

    #[test]
    fn text_bg_with_scroll_and_backdrop() {
        let mut p = Ppu::new();
        p.write16(0x00, 0x0100); // mode 0, BG0
        p.write16(0x08, 0x0100); // char base 0, screen base 0x800
        set_pal(&mut p, 0, 0x7C00); // backdrop blue
        set_pal(&mut p, 1, 0x001F); // red
                                    // tile 1: all pixels colour index 1
        for b in &mut p.vram[32..64] {
            *b = 0x11;
        }
        // map entry (1,0) → tile 1
        p.vram[0x800 + 2..0x800 + 4].copy_from_slice(&1u16.to_le_bytes());
        p.render_line(0);
        assert_eq!(p.framebuffer[0], bgr555_to_rgba(0x7C00));
        assert_eq!(p.framebuffer[8], bgr555_to_rgba(0x001F));
        assert_eq!(p.framebuffer[16], bgr555_to_rgba(0x7C00));
        p.write16(0x10, 4); // hofs = 4
        p.render_line(0);
        assert_eq!(p.framebuffer[4], bgr555_to_rgba(0x001F));
        assert_eq!(p.framebuffer[12], bgr555_to_rgba(0x7C00));
    }

    #[test]
    fn sprite_priority_and_alpha_blend() {
        let mut p = Ppu::new();
        p.write16(0x00, 0x1040); // mode 0, OBJ on, 1D mapping
        set_pal(&mut p, 0, 0x0000);
        p.palette[0x200 + 2..0x200 + 4].copy_from_slice(&0x001Fu16.to_le_bytes());
        for b in &mut p.vram[0x10000..0x10020] {
            *b = 0x11;
        }
        // sprite 0 at (10, 0), 8x8, semi-transparent
        let attr0: u16 = 1 << 10;
        p.oam[0..2].copy_from_slice(&attr0.to_le_bytes());
        p.oam[2..4].copy_from_slice(&10u16.to_le_bytes());
        // hide all other sprites
        for i in 1..128 {
            p.oam[i * 8..i * 8 + 2].copy_from_slice(&0x0200u16.to_le_bytes());
        }
        p.write16(0x50, 0x2000); // 2nd target = backdrop
        p.write16(0x52, 0x0008); // EVA = 8/16, EVB = 0
        p.render_line(0);
        // red 31 * 8/16 = 15
        assert_eq!(p.framebuffer[10], bgr555_to_rgba(15));
        assert_eq!(p.framebuffer[9], bgr555_to_rgba(0));
    }

    #[test]
    fn window_masks_layers() {
        let mut p = Ppu::new();
        p.write16(0x00, 0x2403); // mode 3, BG2, WIN0
        p.write16(0x20, 0x100);
        p.write16(0x26, 0x100);
        p.on_vblank();
        for i in 0..WIDTH {
            p.vram[i * 2..i * 2 + 2].copy_from_slice(&0x03E0u16.to_le_bytes());
        }
        p.write16(0x40, (10 << 8) | 20); // x in [10, 20)
        p.write16(0x44, 160); // y in [0, 160)
        p.write16(0x48, 0x0004); // BG2 inside
        p.write16(0x4A, 0x0000); // nothing outside
        p.render_line(0);
        assert_eq!(p.framebuffer[15], bgr555_to_rgba(0x03E0));
        assert_eq!(p.framebuffer[25], bgr555_to_rgba(0));
    }

    #[test]
    fn brighten_and_darken() {
        assert_eq!(brighten(0, 16), 0x7FFF);
        assert_eq!(darken(0x7FFF, 16), 0);
        assert_eq!(blend(0x001F, 0x001F, 16, 16), 0x001F);
    }
}
