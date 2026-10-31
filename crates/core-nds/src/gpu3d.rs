//! DS 3D engine.
//!
//! **Geometry**: packed GXFIFO and direct command ports, the four matrix
//! modes with their stacks (4x4 20.12 fixed point), vertex formats,
//! hardware lighting (4 lights, shininess table), texture coordinate
//! transforms, polygon assembly (triangles, quads, strips) and homogeneous
//! clipping against the view volume.
//!
//! **Rendering**: scanline rasteriser with Z or W buffering, perspective-
//! correct colour/texture interpolation, all seven texture formats
//! (including 4x4 compressed), modulate/decal/toon/highlight modes, alpha
//! test, translucent polygon sorting and blending, and fog. Output is a
//! 256x192 RGB555 + 5-bit alpha layer composited by engine A as BG0.

use crate::vram::{View, Vram};
use kd_core::{SaveState, StateReader, StateResult, StateWriter};
use std::collections::VecDeque;

pub const WIDTH: usize = 256;
pub const HEIGHT: usize = 192;
const MAX_POLYGONS: usize = 2048;
const MAX_VERTICES: usize = 6144;

type Mat = [i32; 16];
const IDENTITY: Mat = [4096, 0, 0, 0, 0, 4096, 0, 0, 0, 0, 4096, 0, 0, 0, 0, 4096];

/// `a × b` in 20.12 fixed point (row-vector convention: v' = v × M).
pub fn mat_mul(a: &Mat, b: &Mat) -> Mat {
    let mut r = [0i32; 16];
    for i in 0..4 {
        for j in 0..4 {
            let mut acc = 0i64;
            for k in 0..4 {
                acc += a[i * 4 + k] as i64 * b[k * 4 + j] as i64;
            }
            r[i * 4 + j] = (acc >> 12) as i32;
        }
    }
    r
}

fn vec_mul(v: [i32; 4], m: &Mat) -> [i32; 4] {
    let mut r = [0i32; 4];
    for (j, out) in r.iter_mut().enumerate() {
        let mut acc = 0i64;
        for k in 0..4 {
            acc += v[k] as i64 * m[k * 4 + j] as i64;
        }
        *out = (acc >> 12) as i32;
    }
    r
}

/// Number of parameter words for each geometry command.
pub fn param_count(cmd: u8) -> usize {
    match cmd {
        0x10 | 0x12 | 0x13 | 0x14 => 1,
        0x11 | 0x15 | 0x41 => 0,
        0x16 | 0x18 => 16,
        0x17 | 0x19 => 12,
        0x1A => 9,
        0x1B | 0x1C => 3,
        0x20..=0x22 | 0x24..=0x2B | 0x30..=0x33 => 1,
        0x23 => 2,
        0x34 => 32,
        0x40 | 0x50 | 0x60 => 1,
        0x70 => 3,
        0x71 => 2,
        0x72 => 1,
        _ => 0,
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct ClipVertex {
    pos: [i32; 4],
    color: [i32; 3],
    tex: [i32; 2],
}

#[derive(Debug, Clone, Copy, Default)]
struct ScreenVertex {
    x: f32,
    y: f32,
    depth: f32,
    inv_w: f32,
    color: [f32; 3],
    tex: [f32; 2],
}

#[derive(Debug, Clone)]
struct Polygon {
    verts: Vec<ScreenVertex>,
    attr: u32,
    tex_param: u32,
    pal_base: u32,
    translucent: bool,
    min_y: f32,
    max_y: f32,
}

#[derive(Debug, Clone, Copy, Default)]
struct Light {
    dir: [i32; 3],
    half: [i32; 3],
    color: [i32; 3],
}

pub struct Gpu3d {
    pub disp3dcnt: u16,
    mtx_mode: u8,
    proj: Mat,
    proj_stack: Mat,
    proj_sp: u32,
    pos: Mat,
    vec: Mat,
    pos_stack: [Mat; 32],
    vec_stack: [Mat; 32],
    pos_sp: u32,
    tex: Mat,
    tex_stack: Mat,
    tex_sp: u32,
    clip: Mat,
    clip_dirty: bool,
    stack_error: bool,

    color: [i32; 3],
    texcoord: [i32; 2],
    raw_texcoord: [i32; 2],
    last_vtx: [i32; 3],
    pending_attr: u32,
    attr: u32,
    tex_param: u32,
    pal_base: u32,
    diffuse: [i32; 3],
    ambient: [i32; 3],
    specular: [i32; 3],
    emission: [i32; 3],
    shininess_on: bool,
    shininess: [u8; 128],
    lights: [Light; 4],
    prim: u32,
    strip: Vec<ClipVertex>,
    strip_count: usize,
    viewport: [i32; 4],

    polygons: Vec<Polygon>,
    vertex_count: usize,
    swap_pending: bool,
    swap_flags: u32,

    queue: VecDeque<u8>,
    params: Vec<u32>,
    port_latch: u16,

    pub clear_color: u32,
    pub clear_depth: u16,
    pub alpha_ref: u8,
    pub fog_color: u32,
    pub fog_offset: u16,
    pub fog_table: [u8; 32],
    pub toon_table: [u16; 32],
    pub gxstat_irq: u8,
    pos_result: [i32; 4],
    vec_result: [i16; 3],
    box_result: bool,

    /// RGB555 | alpha (5 bits) << 16; read by engine A.
    pub framebuffer: Vec<u32>,
    depth: Vec<u32>,
    fog_flags: Vec<bool>,
    /// Polygons rendered in the last frame (debugger).
    pub rendered_polygons: usize,
}

impl Default for Gpu3d {
    fn default() -> Self {
        Self::new()
    }
}

#[inline]
fn rgb555(c: u32) -> [i32; 3] {
    [(c & 0x1F) as i32, ((c >> 5) & 0x1F) as i32, ((c >> 10) & 0x1F) as i32]
}

#[inline]
fn sext(v: u32, bits: u32) -> i32 {
    ((v << (32 - bits)) as i32) >> (32 - bits)
}

impl Gpu3d {
    pub fn new() -> Self {
        Self {
            disp3dcnt: 0,
            mtx_mode: 0,
            proj: IDENTITY,
            proj_stack: IDENTITY,
            proj_sp: 0,
            pos: IDENTITY,
            vec: IDENTITY,
            pos_stack: [IDENTITY; 32],
            vec_stack: [IDENTITY; 32],
            pos_sp: 0,
            tex: IDENTITY,
            tex_stack: IDENTITY,
            tex_sp: 0,
            clip: IDENTITY,
            clip_dirty: false,
            stack_error: false,
            color: [31; 3],
            texcoord: [0; 2],
            raw_texcoord: [0; 2],
            last_vtx: [0; 3],
            pending_attr: 0,
            attr: 0,
            tex_param: 0,
            pal_base: 0,
            diffuse: [0; 3],
            ambient: [0; 3],
            specular: [0; 3],
            emission: [0; 3],
            shininess_on: false,
            shininess: [0; 128],
            lights: [Light::default(); 4],
            prim: 0,
            strip: Vec::with_capacity(4),
            strip_count: 0,
            viewport: [0, 0, 255, 191],
            polygons: Vec::with_capacity(MAX_POLYGONS),
            vertex_count: 0,
            swap_pending: false,
            swap_flags: 0,
            queue: VecDeque::new(),
            params: Vec::with_capacity(32),
            port_latch: 0,
            clear_color: 0,
            clear_depth: 0x7FFF,
            alpha_ref: 0,
            fog_color: 0,
            fog_offset: 0,
            fog_table: [0; 32],
            toon_table: [0; 32],
            gxstat_irq: 0,
            pos_result: [0; 4],
            vec_result: [0; 3],
            box_result: true,
            framebuffer: vec![0; WIDTH * HEIGHT],
            depth: vec![0xFF_FFFF; WIDTH * HEIGHT],
            fog_flags: vec![false; WIDTH * HEIGHT],
            rendered_polygons: 0,
        }
    }

    pub fn swap_pending(&self) -> bool {
        self.swap_pending
    }

    // ------------------------------------------------------------ registers

    pub fn gxstat(&self) -> u32 {
        (self.box_result as u32) << 1
            | (self.pos_sp & 31) << 8
            | (self.proj_sp & 1) << 13
            | (self.stack_error as u32) << 15
            | 1 << 25
            | 1 << 26
            | (self.gxstat_irq as u32) << 30
    }

    /// Does GXSTAT request the "FIFO less than half full / empty" IRQ?
    /// (Commands execute instantly, so the FIFO is always empty.)
    pub fn fifo_irq(&self) -> bool {
        self.gxstat_irq == 1 || self.gxstat_irq == 2
    }

    pub fn read32(&self, off: u32) -> u32 {
        match off {
            0x060 => self.disp3dcnt as u32,
            0x320 => 46,
            0x600 => self.gxstat(),
            0x604 => self.polygons.len() as u32 | (self.vertex_count as u32) << 16,
            0x620..=0x62F => self.pos_result[((off - 0x620) / 4) as usize] as u32,
            0x630..=0x635 => {
                let i = ((off - 0x630) / 2) as usize;
                self.vec_result[i] as u16 as u32
                    | (self.vec_result.get(i + 1).copied().unwrap_or(0) as u16 as u32) << 16
            }
            0x640..=0x67F => {
                let clip = if self.clip_dirty { mat_mul(&self.pos, &self.proj) } else { self.clip };
                clip[((off - 0x640) / 4) as usize] as u32
            }
            0x680..=0x6A3 => {
                let i = ((off - 0x680) / 4) as usize;
                self.vec[(i / 3) * 4 + i % 3] as u32
            }
            _ => 0,
        }
    }

    pub fn read16(&self, off: u32) -> u16 {
        let v = self.read32(off & !3);
        (v >> ((off & 2) * 8)) as u16
    }

    pub fn write16(&mut self, off: u32, v: u16) {
        match off {
            0x060 => self.disp3dcnt = v & 0x7FFF,
            0x340 => self.alpha_ref = v as u8 & 0x1F,
            0x350 => self.clear_color = (self.clear_color & 0xFFFF_0000) | v as u32,
            0x352 => self.clear_color = (self.clear_color & 0xFFFF) | (v as u32) << 16,
            0x354 => self.clear_depth = v & 0x7FFF,
            0x358 => self.fog_color = (self.fog_color & 0xFFFF_0000) | v as u32,
            0x35A => self.fog_color = (self.fog_color & 0xFFFF) | (v as u32) << 16,
            0x35C => self.fog_offset = v & 0x7FFF,
            0x360..=0x37F => {
                let i = (off - 0x360) as usize;
                self.fog_table[i] = v as u8 & 0x7F;
                self.fog_table[i + 1] = (v >> 8) as u8 & 0x7F;
            }
            0x380..=0x3BF => self.toon_table[((off - 0x380) / 2) as usize] = v & 0x7FFF,
            0x400..=0x5FF => {
                if off & 2 == 0 {
                    self.port_latch = v;
                } else {
                    let full = self.port_latch as u32 | (v as u32) << 16;
                    self.write32(off & !3, full);
                }
            }
            0x600 => {
                if v & 0x8000 != 0 {
                    self.stack_error = false;
                }
            }
            0x602 => self.gxstat_irq = (v >> 14) as u8,
            _ => {}
        }
    }

    pub fn write32(&mut self, off: u32, v: u32) {
        match off {
            0x400..=0x43F => self.write_fifo(v),
            0x440..=0x5FF => {
                let cmd = ((off - 0x440) / 4 + 0x10) as u8;
                self.write_port(cmd, v);
            }
            0x600 => {
                self.write16(0x600, v as u16);
                self.write16(0x602, (v >> 16) as u16);
            }
            _ => {
                self.write16(off, v as u16);
                self.write16(off + 2, (v >> 16) as u16);
            }
        }
    }

    // ---------------------------------------------------- command decoding

    /// Packed command FIFO (0x04000400).
    pub fn write_fifo(&mut self, v: u32) {
        if self.queue.is_empty() {
            for i in 0..4 {
                let cmd = (v >> (i * 8)) as u8;
                if cmd != 0 {
                    self.queue.push_back(cmd);
                }
            }
            self.params.clear();
            self.run_parameterless();
            return;
        }
        self.params.push(v);
        let cmd = self.queue[0];
        if self.params.len() >= param_count(cmd) {
            let params = std::mem::take(&mut self.params);
            self.queue.pop_front();
            self.execute(cmd, &params);
            self.params = params;
            self.params.clear();
            self.run_parameterless();
        }
    }

    fn run_parameterless(&mut self) {
        while let Some(&cmd) = self.queue.front() {
            if param_count(cmd) != 0 {
                break;
            }
            self.queue.pop_front();
            self.execute(cmd, &[]);
        }
    }

    /// Direct command port write (0x04000440 + 4 * (cmd - 0x10)).
    pub fn write_port(&mut self, cmd: u8, v: u32) {
        let n = param_count(cmd);
        if n == 0 {
            self.execute(cmd, &[]);
            return;
        }
        self.params.push(v);
        if self.params.len() >= n {
            let params = std::mem::take(&mut self.params);
            self.execute(cmd, &params);
            self.params = params;
            self.params.clear();
        }
    }

    fn cur_matrix_apply(&mut self, f: impl Fn(&Mat) -> Mat, scale: bool) {
        match self.mtx_mode {
            0 => self.proj = f(&self.proj),
            1 => self.pos = f(&self.pos),
            2 => {
                self.pos = f(&self.pos);
                if !scale {
                    self.vec = f(&self.vec);
                }
            }
            _ => self.tex = f(&self.tex),
        }
        self.clip_dirty = true;
    }

    fn execute(&mut self, cmd: u8, p: &[u32]) {
        match cmd {
            0x10 => self.mtx_mode = (p[0] & 3) as u8,
            0x11 => match self.mtx_mode {
                0 => {
                    if self.proj_sp >= 1 {
                        self.stack_error = true;
                    }
                    self.proj_stack = self.proj;
                    self.proj_sp = 1;
                }
                3 => {
                    self.tex_stack = self.tex;
                    self.tex_sp = 1;
                }
                _ => {
                    if self.pos_sp >= 31 {
                        self.stack_error = true;
                    }
                    let i = (self.pos_sp & 31) as usize;
                    self.pos_stack[i] = self.pos;
                    self.vec_stack[i] = self.vec;
                    self.pos_sp = (self.pos_sp + 1).min(63);
                }
            },
            0x12 => match self.mtx_mode {
                0 => {
                    self.proj_sp = 0;
                    self.proj = self.proj_stack;
                    self.clip_dirty = true;
                }
                3 => {
                    self.tex_sp = 0;
                    self.tex = self.tex_stack;
                }
                _ => {
                    let n = sext(p[0] & 0x3F, 6);
                    let sp = self.pos_sp as i32 - n;
                    if !(0..=31).contains(&sp) {
                        self.stack_error = true;
                    }
                    self.pos_sp = sp.clamp(0, 63) as u32;
                    let i = (self.pos_sp & 31) as usize;
                    self.pos = self.pos_stack[i];
                    self.vec = self.vec_stack[i];
                    self.clip_dirty = true;
                }
            },
            0x13 => match self.mtx_mode {
                0 => self.proj_stack = self.proj,
                3 => self.tex_stack = self.tex,
                _ => {
                    let i = (p[0] & 31) as usize;
                    self.pos_stack[i] = self.pos;
                    self.vec_stack[i] = self.vec;
                }
            },
            0x14 => {
                match self.mtx_mode {
                    0 => self.proj = self.proj_stack,
                    3 => self.tex = self.tex_stack,
                    _ => {
                        let i = (p[0] & 31) as usize;
                        self.pos = self.pos_stack[i];
                        self.vec = self.vec_stack[i];
                    }
                }
                self.clip_dirty = true;
            }
            0x15 => self.cur_matrix_apply(|_| IDENTITY, false),
            0x16 => {
                let m: Mat = std::array::from_fn(|i| p[i] as i32);
                self.cur_matrix_apply(|_| m, false);
            }
            0x17 => {
                let m = mat4x3(p);
                self.cur_matrix_apply(|_| m, false);
            }
            0x18 => {
                let m: Mat = std::array::from_fn(|i| p[i] as i32);
                self.cur_matrix_apply(|c| mat_mul(&m, c), false);
            }
            0x19 => {
                let m = mat4x3(p);
                self.cur_matrix_apply(|c| mat_mul(&m, c), false);
            }
            0x1A => {
                let mut m = IDENTITY;
                for r in 0..3 {
                    for c in 0..3 {
                        m[r * 4 + c] = p[r * 3 + c] as i32;
                    }
                }
                self.cur_matrix_apply(|c| mat_mul(&m, c), false);
            }
            0x1B => {
                let mut m = IDENTITY;
                m[0] = p[0] as i32;
                m[5] = p[1] as i32;
                m[10] = p[2] as i32;
                self.cur_matrix_apply(|c| mat_mul(&m, c), true);
            }
            0x1C => {
                let mut m = IDENTITY;
                m[12] = p[0] as i32;
                m[13] = p[1] as i32;
                m[14] = p[2] as i32;
                self.cur_matrix_apply(|c| mat_mul(&m, c), false);
            }
            0x20 => self.color = rgb555(p[0]),
            0x21 => self.normal(p[0]),
            0x22 => {
                self.raw_texcoord = [sext(p[0] & 0xFFFF, 16), sext(p[0] >> 16, 16)];
                self.texcoord = self.raw_texcoord;
                if (self.tex_param >> 30) == 1 {
                    let v = [self.raw_texcoord[0] << 8, self.raw_texcoord[1] << 8, 1 << 8, 1 << 8];
                    let r = vec_mul(v, &self.tex);
                    self.texcoord = [r[0] >> 8, r[1] >> 8];
                }
            }
            0x23 => self.vertex([sext(p[0] & 0xFFFF, 16), sext(p[0] >> 16, 16), sext(p[1] & 0xFFFF, 16)]),
            0x24 => self.vertex([
                sext(p[0] & 0x3FF, 10) << 6,
                sext((p[0] >> 10) & 0x3FF, 10) << 6,
                sext((p[0] >> 20) & 0x3FF, 10) << 6,
            ]),
            0x25 => self.vertex([sext(p[0] & 0xFFFF, 16), sext(p[0] >> 16, 16), self.last_vtx[2]]),
            0x26 => self.vertex([sext(p[0] & 0xFFFF, 16), self.last_vtx[1], sext(p[0] >> 16, 16)]),
            0x27 => self.vertex([self.last_vtx[0], sext(p[0] & 0xFFFF, 16), sext(p[0] >> 16, 16)]),
            0x28 => {
                let d = [
                    sext(p[0] & 0x3FF, 10) << 3,
                    sext((p[0] >> 10) & 0x3FF, 10) << 3,
                    sext((p[0] >> 20) & 0x3FF, 10) << 3,
                ];
                let v = [self.last_vtx[0] + d[0], self.last_vtx[1] + d[1], self.last_vtx[2] + d[2]];
                self.vertex(v);
            }
            0x29 => self.pending_attr = p[0],
            0x2A => self.tex_param = p[0],
            0x2B => self.pal_base = p[0] & 0x1FFF,
            0x30 => {
                self.diffuse = rgb555(p[0]);
                self.ambient = rgb555(p[0] >> 16);
                if p[0] & 0x8000 != 0 {
                    self.color = self.diffuse;
                }
            }
            0x31 => {
                self.specular = rgb555(p[0]);
                self.emission = rgb555(p[0] >> 16);
                self.shininess_on = p[0] & 0x8000 != 0;
            }
            0x32 => {
                let n = (p[0] >> 30) as usize;
                let d = [
                    sext(p[0] & 0x3FF, 10) << 3,
                    sext((p[0] >> 10) & 0x3FF, 10) << 3,
                    sext((p[0] >> 20) & 0x3FF, 10) << 3,
                ];
                let t = vec_mul([d[0], d[1], d[2], 0], &self.vec);
                self.lights[n].dir = [t[0], t[1], t[2]];
                // half vector between the light and the line of sight (0,0,-1)
                self.lights[n].half = [t[0] / 2, t[1] / 2, (t[2] - 4096) / 2];
            }
            0x33 => self.lights[(p[0] >> 30) as usize].color = rgb555(p[0]),
            0x34 => {
                for (i, w) in p.iter().enumerate() {
                    self.shininess[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
                }
            }
            0x40 => {
                self.prim = p[0] & 3;
                self.attr = self.pending_attr;
                self.strip.clear();
                self.strip_count = 0;
            }
            0x41 => {}
            0x50 => {
                self.swap_pending = true;
                self.swap_flags = p[0] & 3;
            }
            0x60 => {
                self.viewport = [
                    (p[0] & 0xFF) as i32,
                    ((p[0] >> 8) & 0xFF) as i32,
                    ((p[0] >> 16) & 0xFF) as i32,
                    (p[0] >> 24) as i32,
                ];
            }
            0x70 => self.box_result = true,
            0x71 => {
                let v = [sext(p[0] & 0xFFFF, 16), sext(p[0] >> 16, 16), sext(p[1] & 0xFFFF, 16), 4096];
                self.update_clip();
                self.pos_result = vec_mul(v, &self.clip);
            }
            0x72 => {
                let d = [
                    sext(p[0] & 0x3FF, 10) << 3,
                    sext((p[0] >> 10) & 0x3FF, 10) << 3,
                    sext((p[0] >> 20) & 0x3FF, 10) << 3,
                ];
                let t = vec_mul([d[0], d[1], d[2], 0], &self.vec);
                self.vec_result = [t[0] as i16, t[1] as i16, t[2] as i16];
            }
            0x00 => {}
            _ => tracing::warn!("unknown 3D command {cmd:#04x}"),
        }
    }

    fn update_clip(&mut self) {
        if self.clip_dirty {
            self.clip = mat_mul(&self.pos, &self.proj);
            self.clip_dirty = false;
        }
    }

    fn normal(&mut self, p: u32) {
        let n = [sext(p & 0x3FF, 10) << 3, sext((p >> 10) & 0x3FF, 10) << 3, sext((p >> 20) & 0x3FF, 10) << 3];
        if (self.tex_param >> 30) == 2 {
            let v = [n[0], n[1], n[2], 1 << 12];
            let r = vec_mul(v, &self.tex);
            self.texcoord = [self.raw_texcoord[0] + (r[0] >> 12), self.raw_texcoord[1] + (r[1] >> 12)];
        }
        let t = vec_mul([n[0], n[1], n[2], 0], &self.vec);
        let n = [t[0] as i64, t[1] as i64, t[2] as i64];
        let mut c = [self.emission[0] << 5, self.emission[1] << 5, self.emission[2] << 5];
        for (i, light) in self.lights.iter().enumerate() {
            if self.pending_attr & (1 << i) == 0 && self.attr & (1 << i) == 0 {
                continue;
            }
            let d = &light.dir;
            let diffuse = (-(d[0] as i64 * n[0] + d[1] as i64 * n[1] + d[2] as i64 * n[2]) >> 12).clamp(0, 4096) as i32;
            let h = &light.half;
            let mut shine = (-(h[0] as i64 * n[0] + h[1] as i64 * n[1] + h[2] as i64 * n[2]) >> 12).clamp(0, 4096);
            shine = (shine * shine) >> 12;
            let mut shine = shine as i32;
            if self.shininess_on {
                shine = (self.shininess[(shine >> 5).min(127) as usize] as i32) << 4;
            }
            for ch in 0..3 {
                let lc = light.color[ch];
                c[ch] += (self.specular[ch] * lc * shine) >> 12;
                c[ch] += (self.diffuse[ch] * lc * diffuse) >> 12;
                c[ch] += self.ambient[ch] * lc;
            }
        }
        self.color = [(c[0] >> 5).clamp(0, 31), (c[1] >> 5).clamp(0, 31), (c[2] >> 5).clamp(0, 31)];
    }

    fn vertex(&mut self, v: [i32; 3]) {
        self.last_vtx = v;
        self.update_clip();
        let pos = vec_mul([v[0], v[1], v[2], 4096], &self.clip);
        let mut tex = self.texcoord;
        if (self.tex_param >> 30) == 3 {
            let r = vec_mul([v[0], v[1], v[2], 1 << 12], &self.tex);
            tex = [self.raw_texcoord[0] + (r[0] >> 12), self.raw_texcoord[1] + (r[1] >> 12)];
        }
        if self.vertex_count >= MAX_VERTICES {
            return;
        }
        self.strip.push(ClipVertex { pos, color: self.color, tex });
        self.strip_count += 1;
        match self.prim {
            0 if self.strip.len() == 3 => {
                let s = std::mem::take(&mut self.strip);
                self.emit(&s);
            }
            1 if self.strip.len() == 4 => {
                let s = std::mem::take(&mut self.strip);
                self.emit(&s);
            }
            2 if self.strip.len() == 3 => {
                // Odd triangles in a strip have reversed winding.
                let tri = if self.strip_count % 2 == 0 {
                    [self.strip[1], self.strip[0], self.strip[2]]
                } else {
                    [self.strip[0], self.strip[1], self.strip[2]]
                };
                self.emit(&tri);
                self.strip.remove(0);
            }
            3 if self.strip.len() == 4 => {
                let q = [self.strip[0], self.strip[1], self.strip[3], self.strip[2]];
                self.emit(&q);
                self.strip.drain(0..2);
            }
            _ => {}
        }
    }

    fn emit(&mut self, verts: &[ClipVertex]) {
        if self.polygons.len() >= MAX_POLYGONS {
            return;
        }
        let clipped = clip_polygon(verts);
        if clipped.len() < 3 {
            return;
        }
        let [x1, y1, x2, y2] = self.viewport;
        let vw = (x2 - x1 + 1) as f32;
        let vh = (y2 - y1 + 1) as f32;
        let w_buffer = self.swap_flags & 2 != 0;
        let mut sv = Vec::with_capacity(clipped.len());
        for c in &clipped {
            let w = c.pos[3] as f32;
            if w <= 0.0 {
                return;
            }
            let x = (c.pos[0] as f32 + w) * vw / (2.0 * w) + x1 as f32;
            let y_up = (c.pos[1] as f32 + w) * vh / (2.0 * w) + y1 as f32;
            let y = 191.0 - y_up + 1.0;
            let z = c.pos[2] as f32 / w;
            let depth = if w_buffer { w } else { ((z * 0.5 + 0.5) * 16_777_215.0).clamp(0.0, 16_777_215.0) };
            let iw = 1.0 / w;
            sv.push(ScreenVertex {
                x,
                y,
                depth,
                inv_w: iw,
                color: [c.color[0] as f32 * iw, c.color[1] as f32 * iw, c.color[2] as f32 * iw],
                tex: [c.tex[0] as f32 * iw, c.tex[1] as f32 * iw],
            });
        }
        // Culling: signed area in screen space (y grows downwards).
        let mut area = 0.0;
        for i in 0..sv.len() {
            let a = &sv[i];
            let b = &sv[(i + 1) % sv.len()];
            area += a.x * b.y - b.x * a.y;
        }
        let front = area < 0.0;
        let attr = self.attr;
        if (front && attr & 0x80 == 0) || (!front && attr & 0x40 == 0) {
            return;
        }
        let alpha = (attr >> 16) & 0x1F;
        let tex_format = (self.tex_param >> 26) & 7;
        let translucent = (alpha != 0 && alpha != 31) || matches!(tex_format, 1 | 6);
        let min_y = sv.iter().map(|v| v.y).fold(f32::MAX, f32::min);
        let max_y = sv.iter().map(|v| v.y).fold(f32::MIN, f32::max);
        self.vertex_count += sv.len();
        self.polygons.push(Polygon {
            verts: sv,
            attr,
            tex_param: self.tex_param,
            pal_base: self.pal_base,
            translucent,
            min_y,
            max_y,
        });
    }

    // ----------------------------------------------------------- rendering

    /// Called at VBlank: if SWAP_BUFFERS was issued, render the finished
    /// polygon list and start a new one. Returns whether a frame was drawn.
    pub fn on_vblank(&mut self, vram: &Vram) -> bool {
        if !self.swap_pending {
            return false;
        }
        self.swap_pending = false;
        let mut polys = std::mem::take(&mut self.polygons);
        self.vertex_count = 0;
        let manual_sort = self.swap_flags & 1 != 0;
        // Opaque first (in order), then translucent sorted by Y unless
        // manual sorting was requested.
        let (mut opaque, mut trans): (Vec<Polygon>, Vec<Polygon>) = polys.drain(..).partition(|p| !p.translucent);
        if !manual_sort {
            trans.sort_by(|a, b| a.max_y.partial_cmp(&b.max_y).unwrap().then(a.min_y.partial_cmp(&b.min_y).unwrap()));
        }
        opaque.append(&mut trans);
        self.render(vram, &opaque);
        self.rendered_polygons = opaque.len();
        polys = opaque;
        polys.clear();
        self.polygons = polys;
        true
    }

    fn render(&mut self, vram: &Vram, polys: &[Polygon]) {
        let cc = self.clear_color;
        let clear = (cc & 0x7FFF) | ((cc >> 16) & 0x1F) << 16;
        let clear_depth = (self.clear_depth as u32) * 0x200 + 0x1FF;
        self.framebuffer.fill(clear);
        self.depth.fill(clear_depth);
        self.fog_flags.fill(cc & 0x8000 != 0);
        for poly in polys {
            self.raster(vram, poly);
        }
        if self.disp3dcnt & 0x80 != 0 {
            self.apply_fog();
        }
    }

    fn raster(&mut self, vram: &Vram, poly: &Polygon) {
        let v = &poly.verts;
        let n = v.len();
        let y_start = poly.min_y.max(0.0).ceil() as i32;
        let y_end = poly.max_y.min(HEIGHT as f32).ceil() as i32;
        let alpha = ((poly.attr >> 16) & 0x1F) as i32;
        let wireframe = alpha == 0;
        let poly_alpha = if wireframe { 31 } else { alpha };
        let mode = (poly.attr >> 4) & 3;
        if mode == 3 {
            return; // shadow volumes are not emulated
        }
        let tex_enabled = self.disp3dcnt & 1 != 0 && (poly.tex_param >> 26) & 7 != 0;
        let depth_equal = poly.attr & (1 << 14) != 0;
        let update_depth_trans = poly.attr & (1 << 11) != 0;
        let fog = poly.attr & (1 << 15) != 0;
        let blending = self.disp3dcnt & 8 != 0;
        let alpha_test = self.disp3dcnt & 4 != 0;
        for y in y_start..y_end {
            let fy = y as f32 + 0.5;
            // Find the span by intersecting every edge with this scanline.
            let mut left: Option<(f32, ScreenVertex)> = None;
            let mut right: Option<(f32, ScreenVertex)> = None;
            for i in 0..n {
                let a = &v[i];
                let b = &v[(i + 1) % n];
                if (a.y <= fy && b.y > fy) || (b.y <= fy && a.y > fy) {
                    let t = (fy - a.y) / (b.y - a.y);
                    let p = lerp_vertex(a, b, t);
                    if left.as_ref().is_none_or(|(x, _)| p.x < *x) {
                        left = Some((p.x, p));
                    }
                    if right.as_ref().is_none_or(|(x, _)| p.x > *x) {
                        right = Some((p.x, p));
                    }
                }
            }
            let (Some((xl, l)), Some((xr, r))) = (left, right) else { continue };
            let x0 = xl.max(0.0).ceil() as i32;
            let x1 = xr.min(WIDTH as f32).ceil() as i32;
            let span = (xr - xl).max(1e-6);
            for x in x0..x1 {
                let t = ((x as f32 + 0.5) - xl) / span;
                if wireframe && x != x0 && x != x1 - 1 && y != y_start && y != y_end - 1 {
                    continue;
                }
                let p = lerp_vertex(&l, &r, t.clamp(0.0, 1.0));
                let idx = y as usize * WIDTH + x as usize;
                let depth = p.depth.clamp(0.0, 16_777_215.0) as u32;
                let stored = self.depth[idx];
                let pass = if depth_equal { depth.abs_diff(stored) <= 0x200 } else { depth < stored };
                if !pass {
                    continue;
                }
                let w = 1.0 / p.inv_w;
                let vc = [
                    (p.color[0] * w).clamp(0.0, 31.0) as i32,
                    (p.color[1] * w).clamp(0.0, 31.0) as i32,
                    (p.color[2] * w).clamp(0.0, 31.0) as i32,
                ];
                let (mut rgb, mut a) = (vc, poly_alpha);
                if mode == 2 {
                    // Toon (or highlight with DISP3DCNT bit 1): shade from the toon table.
                    let toon = rgb555(self.toon_table[(vc[0] as usize).min(31)] as u32);
                    rgb = if self.disp3dcnt & 2 != 0 {
                        [(vc[0] + toon[0]).min(31), (vc[1] + toon[0]).min(31), (vc[2] + toon[0]).min(31)]
                    } else {
                        toon
                    };
                }
                if tex_enabled {
                    let s = (p.tex[0] * w) as i32;
                    let tt = (p.tex[1] * w) as i32;
                    let Some((tc, ta)) = sample_texture(vram, poly.tex_param, poly.pal_base, s, tt) else { continue };
                    match mode {
                        1 => {
                            // decal
                            for ch in 0..3 {
                                rgb[ch] = (tc[ch] * ta + rgb[ch] * (31 - ta)) / 31;
                            }
                        }
                        _ => {
                            for ch in 0..3 {
                                rgb[ch] = (((tc[ch] * 2 + 1) * (rgb[ch] * 2 + 1) - 1) / 64).min(31);
                            }
                            a = ((ta + 1) * (poly_alpha + 1) - 1) / 32;
                        }
                    }
                }
                if a == 0 || (alpha_test && a <= self.alpha_ref as i32) {
                    continue;
                }
                let translucent = a < 31;
                let dst = self.framebuffer[idx];
                let out = if translucent && blending && (dst >> 16) & 0x1F != 0 {
                    let d = rgb555(dst);
                    let da = ((dst >> 16) & 0x1F) as i32;
                    let mix = |s: i32, dd: i32| (s * (a + 1) + dd * (31 - a)) >> 5;
                    let c = [mix(rgb[0], d[0]), mix(rgb[1], d[1]), mix(rgb[2], d[2])];
                    (c[0] | c[1] << 5 | c[2] << 10) as u32 | (a.max(da) as u32) << 16
                } else {
                    (rgb[0] | rgb[1] << 5 | rgb[2] << 10) as u32 | (a as u32) << 16
                };
                self.framebuffer[idx] = out;
                if !translucent || update_depth_trans {
                    self.depth[idx] = depth;
                }
                self.fog_flags[idx] = fog;
            }
        }
    }

    fn apply_fog(&mut self) {
        let shift = ((self.disp3dcnt >> 8) & 0xF) as u32;
        let step = (0x400u32 >> shift).max(1);
        let fc = rgb555(self.fog_color);
        let fa = ((self.fog_color >> 16) & 0x1F) as i32;
        let alpha_only = self.disp3dcnt & 0x40 != 0;
        for i in 0..WIDTH * HEIGHT {
            if !self.fog_flags[i] {
                continue;
            }
            let z = self.depth[i] >> 9;
            let rel = z.saturating_sub(self.fog_offset as u32);
            let pos = rel / step;
            let density = if pos >= 31 {
                self.fog_table[31] as i32
            } else {
                let a = self.fog_table[pos as usize] as i32;
                let b = self.fog_table[pos as usize + 1] as i32;
                let frac = (rel % step) as i32;
                a + (b - a) * frac / step as i32
            };
            let d = if density >= 127 { 128 } else { density };
            let px = self.framebuffer[i];
            let c = rgb555(px);
            let pa = ((px >> 16) & 0x1F) as i32;
            let mix = |p: i32, f: i32| (f * d + p * (128 - d)) >> 7;
            let rgb = if alpha_only { c } else { [mix(c[0], fc[0]), mix(c[1], fc[1]), mix(c[2], fc[2])] };
            let a = mix(pa, fa);
            self.framebuffer[i] = (rgb[0] | rgb[1] << 5 | rgb[2] << 10) as u32 | (a as u32) << 16;
        }
    }
}

fn mat4x3(p: &[u32]) -> Mat {
    let mut m = IDENTITY;
    for r in 0..4 {
        for c in 0..3 {
            m[r * 4 + c] = p[r * 3 + c] as i32;
        }
    }
    m[3] = 0;
    m[7] = 0;
    m[11] = 0;
    m[15] = 4096;
    m
}

fn lerp_vertex(a: &ScreenVertex, b: &ScreenVertex, t: f32) -> ScreenVertex {
    let l = |x: f32, y: f32| x + (y - x) * t;
    ScreenVertex {
        x: l(a.x, b.x),
        y: l(a.y, b.y),
        depth: l(a.depth, b.depth),
        inv_w: l(a.inv_w, b.inv_w),
        color: [l(a.color[0], b.color[0]), l(a.color[1], b.color[1]), l(a.color[2], b.color[2])],
        tex: [l(a.tex[0], b.tex[0]), l(a.tex[1], b.tex[1])],
    }
}

/// Sutherland–Hodgman clipping against -w <= x, y, z <= w.
fn clip_polygon(input: &[ClipVertex]) -> Vec<ClipVertex> {
    let mut poly: Vec<ClipVertex> = input.to_vec();
    for axis in 0..3 {
        for sign in [1i64, -1] {
            if poly.is_empty() {
                return poly;
            }
            let dist = |v: &ClipVertex| v.pos[3] as i64 - sign * v.pos[axis] as i64;
            let mut out = Vec::with_capacity(poly.len() + 2);
            for i in 0..poly.len() {
                let cur = poly[i];
                let next = poly[(i + 1) % poly.len()];
                let dc = dist(&cur);
                let dn = dist(&next);
                if dc >= 0 {
                    out.push(cur);
                }
                if (dc >= 0) != (dn >= 0) {
                    let t = dc as f64 / (dc - dn) as f64;
                    let lerp = |a: i32, b: i32| a + ((b - a) as f64 * t).round() as i32;
                    out.push(ClipVertex {
                        pos: std::array::from_fn(|k| lerp(cur.pos[k], next.pos[k])),
                        color: std::array::from_fn(|k| lerp(cur.color[k], next.color[k])),
                        tex: std::array::from_fn(|k| lerp(cur.tex[k], next.tex[k])),
                    });
                }
            }
            poly = out;
        }
    }
    poly
}

fn wrap_coord(v: i32, size: i32, repeat: bool, flip: bool) -> i32 {
    if !repeat {
        return v.clamp(0, size - 1);
    }
    if flip {
        let period = v.rem_euclid(size * 2);
        if period >= size {
            size * 2 - 1 - period
        } else {
            period
        }
    } else {
        v.rem_euclid(size)
    }
}

/// Fetch a texel: returns (RGB 5-bit each, alpha 0..31) or `None` if
/// transparent.
fn sample_texture(vram: &Vram, param: u32, pal_base: u32, s: i32, t: i32) -> Option<([i32; 3], i32)> {
    let w = 8i32 << ((param >> 20) & 7);
    let h = 8i32 << ((param >> 23) & 7);
    let s = wrap_coord(s >> 4, w, param & (1 << 16) != 0, param & (1 << 18) != 0);
    let t = wrap_coord(t >> 4, h, param & (1 << 17) != 0, param & (1 << 19) != 0);
    let base = (param & 0xFFFF) << 3;
    let texel = (t * w + s) as u32;
    let color0_transparent = param & (1 << 29) != 0;
    let pal = |off: u32| rgb555(vram.read16(View::TexPalette, off) as u32);
    match (param >> 26) & 7 {
        1 => {
            let b = vram.read8(View::Texture, base + texel);
            let a3 = (b >> 5) as i32;
            let a = a3 * 4 + a3 / 2;
            Some((pal((pal_base << 4) + (b as u32 & 31) * 2), a))
        }
        2 => {
            let b = vram.read8(View::Texture, base + texel / 4);
            let idx = ((b >> ((texel & 3) * 2)) & 3) as u32;
            if idx == 0 && color0_transparent {
                return None;
            }
            Some((pal((pal_base << 3) + idx * 2), 31))
        }
        3 => {
            let b = vram.read8(View::Texture, base + texel / 2);
            let idx = if texel & 1 != 0 { b >> 4 } else { b & 0xF } as u32;
            if idx == 0 && color0_transparent {
                return None;
            }
            Some((pal((pal_base << 4) + idx * 2), 31))
        }
        4 => {
            let idx = vram.read8(View::Texture, base + texel) as u32;
            if idx == 0 && color0_transparent {
                return None;
            }
            Some((pal((pal_base << 4) + idx * 2), 31))
        }
        5 => {
            let block = ((t / 4) * (w / 4) + s / 4) as u32;
            let addr = base + block * 4;
            let data = vram.read32(View::Texture, addr);
            let texel_bits = (data >> ((t as u32 & 3) * 8 + (s as u32 & 3) * 2)) & 3;
            let slot1 = 0x20000 + (addr & 0x1FFFF) / 2 + if addr >= 0x40000 { 0x10000 } else { 0 };
            let info = vram.read16(View::Texture, slot1) as u32;
            let pbase = (pal_base << 4) + (info & 0x3FFF) * 4;
            let c = |i: u32| pal(pbase + i * 2);
            let mixc = |a: [i32; 3], b: [i32; 3], wa: i32, wb: i32| {
                [
                    (a[0] * wa + b[0] * wb) / (wa + wb),
                    (a[1] * wa + b[1] * wb) / (wa + wb),
                    (a[2] * wa + b[2] * wb) / (wa + wb),
                ]
            };
            let color = match (info >> 14, texel_bits) {
                (0 | 1, 3) => return None,
                (1, 2) => mixc(c(0), c(1), 1, 1),
                (3, 2) => mixc(c(0), c(1), 5, 3),
                (3, 3) => mixc(c(0), c(1), 3, 5),
                (_, i) => c(i),
            };
            Some((color, 31))
        }
        6 => {
            let b = vram.read8(View::Texture, base + texel);
            Some((pal((pal_base << 4) + (b as u32 & 7) * 2), (b >> 3) as i32))
        }
        7 => {
            let c = vram.read16(View::Texture, base + texel * 2);
            if c & 0x8000 == 0 {
                return None;
            }
            Some((rgb555(c as u32), 31))
        }
        _ => None,
    }
}

impl SaveState for Gpu3d {
    fn save_state(&self, w: &mut StateWriter) {
        w.u16(self.disp3dcnt);
        w.u8(self.mtx_mode);
        let mats: [&Mat; 6] = [&self.proj, &self.proj_stack, &self.pos, &self.vec, &self.tex, &self.tex_stack];
        for m in mats {
            w.u32s(&m.map(|v| v as u32));
        }
        for m in self.pos_stack.iter().chain(self.vec_stack.iter()) {
            w.u32s(&m.map(|v| v as u32));
        }
        w.u32(self.proj_sp);
        w.u32(self.pos_sp);
        w.u32(self.tex_sp);
        w.u32(self.pending_attr);
        w.u32(self.tex_param);
        w.u32(self.pal_base);
        w.u32s(&self.viewport.map(|v| v as u32));
        w.u32(self.clear_color);
        w.u16(self.clear_depth);
        w.u8(self.alpha_ref);
        w.u32(self.fog_color);
        w.u16(self.fog_offset);
        w.bytes(&self.fog_table);
        w.u16s(&self.toon_table);
        w.u8(self.gxstat_irq);
        w.u32s(&self.framebuffer);
    }
    fn load_state(&mut self, r: &mut StateReader<'_>) -> StateResult<()> {
        self.disp3dcnt = r.u16()?;
        self.mtx_mode = r.u8()?;
        let read_mat = |r: &mut StateReader<'_>| -> StateResult<Mat> {
            let mut m = [0u32; 16];
            r.u32s_into(&mut m)?;
            Ok(m.map(|v| v as i32))
        };
        self.proj = read_mat(r)?;
        self.proj_stack = read_mat(r)?;
        self.pos = read_mat(r)?;
        self.vec = read_mat(r)?;
        self.tex = read_mat(r)?;
        self.tex_stack = read_mat(r)?;
        for i in 0..32 {
            self.pos_stack[i] = read_mat(r)?;
        }
        for i in 0..32 {
            self.vec_stack[i] = read_mat(r)?;
        }
        self.proj_sp = r.u32()?;
        self.pos_sp = r.u32()?;
        self.tex_sp = r.u32()?;
        self.pending_attr = r.u32()?;
        self.tex_param = r.u32()?;
        self.pal_base = r.u32()?;
        let mut vp = [0u32; 4];
        r.u32s_into(&mut vp)?;
        self.viewport = vp.map(|v| v as i32);
        self.clear_color = r.u32()?;
        self.clear_depth = r.u16()?;
        self.alpha_ref = r.u8()?;
        self.fog_color = r.u32()?;
        self.fog_offset = r.u16()?;
        r.bytes_into(&mut self.fog_table)?;
        r.u16s_into(&mut self.toon_table)?;
        self.gxstat_irq = r.u8()?;
        r.u32s_into(&mut self.framebuffer)?;
        self.clip_dirty = true;
        self.polygons.clear();
        self.queue.clear();
        self.params.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fx(v: f32) -> u32 {
        ((v * 4096.0) as i32) as u32
    }

    fn vtx16(g: &mut Gpu3d, x: f32, y: f32, z: f32) {
        g.write_port(0x23, (fx(x) & 0xFFFF) | (fx(y) << 16));
        g.write_port(0x23, fx(z) & 0xFFFF);
    }

    fn triangle(g: &mut Gpu3d, color: u32) {
        g.write_port(0x10, 0);
        g.write_port(0x15, 0);
        g.write_port(0x10, 2);
        g.write_port(0x15, 0);
        g.write_port(0x60, 0xBF_FF_00_00);
        g.write_port(0x29, 0x001F_00C0); // alpha 31, front+back
        g.write_port(0x40, 0);
        g.write_port(0x20, color);
        vtx16(g, -0.5, -0.5, 0.0);
        vtx16(g, 0.5, -0.5, 0.0);
        vtx16(g, 0.0, 0.5, 0.0);
        g.write_port(0x41, 0);
        g.write_port(0x50, 0);
    }

    #[test]
    fn renders_flat_triangle() {
        let vram = Vram::new();
        let mut g = Gpu3d::new();
        g.clear_color = 0x7C00 | 31 << 16;
        triangle(&mut g, 0x001F);
        assert!(g.swap_pending());
        assert!(g.on_vblank(&vram));
        assert_eq!(g.rendered_polygons, 1);
        let center = g.framebuffer[96 * WIDTH + 128];
        assert_eq!(center & 0x7FFF, 0x001F, "red in the middle");
        assert_eq!((center >> 16) & 0x1F, 31);
        assert_eq!(g.framebuffer[5 * WIDTH + 5] & 0x7FFF, 0x7C00, "clear colour outside");
        // apex is near the top-middle (y ≈ 48), base near y ≈ 143
        assert_eq!(g.framebuffer[60 * WIDTH + 128] & 0x7FFF, 0x001F);
        assert_eq!(g.framebuffer[60 * WIDTH + 70] & 0x7FFF, 0x7C00);
    }

    #[test]
    fn packed_fifo_matches_direct_ports() {
        let vram = Vram::new();
        let mut a = Gpu3d::new();
        triangle(&mut a, 0x03E0);
        a.on_vblank(&vram);

        let mut b = Gpu3d::new();
        b.write_fifo(0x15_10_15_10); // MTX_MODE, IDENTITY, MTX_MODE, IDENTITY
        b.write_fifo(0); // mode 0 (projection)
        b.write_fifo(2); // mode 2
        b.write_fifo(0x40_29_00_60); // VIEWPORT, (nop), POLYGON_ATTR, BEGIN
        b.write_fifo(0xBF_FF_00_00);
        b.write_fifo(0x001F_00C0);
        b.write_fifo(0);
        b.write_fifo(0x23_23_23_20); // COLOR, VTX16 x3
        b.write_fifo(0x03E0);
        for (x, y) in [(-0.5f32, -0.5f32), (0.5, -0.5), (0.0, 0.5)] {
            b.write_fifo((fx(x) & 0xFFFF) | (fx(y) << 16));
            b.write_fifo(0);
        }
        b.write_fifo(0x50_41); // END, SWAP
        b.write_fifo(0);
        assert!(b.swap_pending());
        b.on_vblank(&vram);
        assert_eq!(a.framebuffer, b.framebuffer);
    }

    #[test]
    fn matrix_stack_push_pop() {
        let mut g = Gpu3d::new();
        g.write_port(0x10, 1);
        g.write_port(0x1C, fx(1.0));
        g.write_port(0x1C, 0);
        g.write_port(0x1C, 0);
        g.write_port(0x11, 0); // push
        g.write_port(0x15, 0); // identity
        assert_eq!(g.pos, IDENTITY);
        g.write_port(0x12, 1); // pop 1
        assert_eq!(g.pos[12], 4096);
        assert_eq!(g.pos_sp, 0);
        g.write_port(0x12, 1); // underflow
        assert!(g.stack_error);
    }

    #[test]
    fn clipping_keeps_inside_and_splits_crossing() {
        let v = |x: i32| ClipVertex { pos: [x, 0, 0, 4096], ..Default::default() };
        let inside = clip_polygon(&[v(0), v(100), v(-100)]);
        assert_eq!(inside.len(), 3);
        let crossing = clip_polygon(&[
            ClipVertex { pos: [0, 0, 0, 4096], ..Default::default() },
            ClipVertex { pos: [8192, 0, 0, 4096], ..Default::default() },
            ClipVertex { pos: [0, 2048, 0, 4096], ..Default::default() },
        ]);
        assert!(crossing.len() >= 3);
        assert!(crossing.iter().all(|c| c.pos[0] <= c.pos[3]));
    }

    #[test]
    fn direct_color_texture() {
        let mut vram = Vram::new();
        vram.set_cnt(0, 0x83); // A → texture slot 0
                               // 8x8 direct-colour texture, every texel opaque green
        for i in 0..64 {
            vram.write16(View::Texture, i * 2, 0x8000 | 0x03E0);
        }
        let param = 7 << 26; // format 7, 8x8, offset 0
        let (c, a) = sample_texture(&vram, param, 0, 3 << 4, 2 << 4).unwrap();
        assert_eq!(c, [0, 31, 0]);
        assert_eq!(a, 31);
        // repeat + flip
        assert_eq!(wrap_coord(9, 8, true, true), 6);
        assert_eq!(wrap_coord(9, 8, true, false), 1);
        assert_eq!(wrap_coord(9, 8, false, false), 7);
    }
}
