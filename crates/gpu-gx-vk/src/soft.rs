//! Software reference rasteriser for GX: renders into an emulated EFB with
//! the CPU TEV evaluator. It is the ground truth for tests and the
//! fallback when no Vulkan device is available.

use crate::tev::{evaluate, TevConfig, TevInputs};
use crate::xf::ClipVertex;

pub const EFB_WIDTH: usize = 640;
pub const EFB_HEIGHT: usize = 528;

/// Per-draw fixed-function state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RasterState {
    pub depth_test: bool,
    pub depth_func: u8,
    pub depth_write: bool,
    pub blend: bool,
    pub blend_src: u8,
    pub blend_dst: u8,
    pub blend_subtract: bool,
    pub color_update: bool,
    pub alpha_update: bool,
    /// 0 none, 1 back (clockwise), 2 front, 3 all
    pub cull: u8,
    pub scissor: [i32; 4],
}

impl Default for RasterState {
    fn default() -> Self {
        Self {
            depth_test: false,
            depth_func: 7,
            depth_write: false,
            blend: false,
            blend_src: 1,
            blend_dst: 0,
            blend_subtract: false,
            color_update: true,
            alpha_update: true,
            cull: 0,
            scissor: [0, 0, EFB_WIDTH as i32 - 1, EFB_HEIGHT as i32 - 1],
        }
    }
}

impl RasterState {
    /// Build from BP registers (ZMODE 0x40, BLENDMODE 0x41, GENMODE 0x00,
    /// scissor 0x20/0x21 with the 342 offset).
    pub fn from_bp(bp: &[u32; 256]) -> Self {
        let z = bp[0x40];
        let b = bp[0x41];
        let tl = bp[0x20];
        let br = bp[0x21];
        Self {
            depth_test: z & 1 != 0,
            depth_func: ((z >> 1) & 7) as u8,
            depth_write: z & 0x10 != 0,
            blend: b & 1 != 0,
            blend_src: ((b >> 8) & 7) as u8,
            blend_dst: ((b >> 5) & 7) as u8,
            blend_subtract: b & 0x800 != 0,
            color_update: b & 0x8 != 0,
            alpha_update: b & 0x10 != 0,
            cull: ((bp[0x00] >> 14) & 3) as u8,
            scissor: [
                (tl >> 12 & 0x7FF) as i32 - 342,
                (tl & 0x7FF) as i32 - 342,
                (br >> 12 & 0x7FF) as i32 - 342,
                (br & 0x7FF) as i32 - 342,
            ],
        }
    }
}

/// A screen-space vertex ready for rasterisation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WinVertex {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub inv_w: f32,
    pub color: [[f32; 4]; 2],
    pub tex: [[f32; 2]; 8],
}

impl WinVertex {
    pub fn new(win: [f32; 3], c: &ClipVertex) -> Self {
        let w = if c.clip[3] == 0.0 { 1.0 } else { c.clip[3] };
        Self { x: win[0], y: win[1], z: win[2], inv_w: 1.0 / w, color: c.color, tex: c.tex }
    }
}

/// Sampled texture for the software path (RGBA8, R in the low byte).
#[derive(Debug, Clone)]
pub struct SoftTexture {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u32>,
    /// 0 clamp, 1 repeat, 2 mirror
    pub wrap: [u8; 2],
}

impl SoftTexture {
    fn sample(&self, s: f32, t: f32) -> [f32; 4] {
        let coord = |v: f32, size: usize, wrap: u8| -> usize {
            let i = (v * size as f32).floor() as i64;
            let n = size as i64;
            (match wrap {
                1 => i.rem_euclid(n),
                2 => {
                    let p = i.rem_euclid(2 * n);
                    if p >= n {
                        2 * n - 1 - p
                    } else {
                        p
                    }
                }
                _ => i.clamp(0, n - 1),
            }) as usize
        };
        let p = self.pixels[coord(t, self.height, self.wrap[1]) * self.width + coord(s, self.width, self.wrap[0])];
        [
            (p & 0xFF) as f32 / 255.0,
            ((p >> 8) & 0xFF) as f32 / 255.0,
            ((p >> 16) & 0xFF) as f32 / 255.0,
            (p >> 24) as f32 / 255.0,
        ]
    }
}

pub struct SoftRenderer {
    /// RGBA8 colour buffer (R in the low byte).
    pub color: Vec<u32>,
    pub depth: Vec<f32>,
    pub triangles: u64,
    pub pixels_written: u64,
}

impl Default for SoftRenderer {
    fn default() -> Self {
        Self::new()
    }
}

fn depth_pass(func: u8, z: f32, stored: f32) -> bool {
    match func {
        0 => false,
        1 => z < stored,
        2 => z == stored,
        3 => z <= stored,
        4 => z > stored,
        5 => z != stored,
        6 => z >= stored,
        _ => true,
    }
}

fn factor(f: u8, src: [f32; 4], dst: [f32; 4], for_src: bool) -> [f32; 4] {
    let other = if for_src { dst } else { src };
    match f {
        0 => [0.0; 4],
        1 => [1.0; 4],
        2 => other,
        3 => other.map(|c| 1.0 - c),
        4 => [src[3]; 4],
        5 => [1.0 - src[3]; 4],
        6 => [dst[3]; 4],
        _ => [1.0 - dst[3]; 4],
    }
}

impl SoftRenderer {
    pub fn new() -> Self {
        Self {
            color: vec![0xFF00_0000; EFB_WIDTH * EFB_HEIGHT],
            depth: vec![1.0; EFB_WIDTH * EFB_HEIGHT],
            triangles: 0,
            pixels_written: 0,
        }
    }

    pub fn clear(&mut self, rgba: u32, z: f32) {
        self.color.fill(rgba);
        self.depth.fill(z);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw_triangle(
        &mut self,
        v: [WinVertex; 3],
        tev: &TevConfig,
        regs: &[[f32; 4]; 4],
        konst: &[[f32; 4]; 4],
        textures: &[Option<SoftTexture>; 8],
        rs: &RasterState,
    ) {
        let area = (v[1].x - v[0].x) * (v[2].y - v[0].y) - (v[2].x - v[0].x) * (v[1].y - v[0].y);
        if area == 0.0 {
            return;
        }
        // y grows downward: area > 0 is clockwise on screen.
        let clockwise = area > 0.0;
        match rs.cull {
            1 if clockwise => return,
            2 if !clockwise => return,
            3 => return,
            _ => {}
        }
        self.triangles += 1;
        let [sx0, sy0, sx1, sy1] = rs.scissor;
        let minx = v.iter().map(|p| p.x).fold(f32::MAX, f32::min).floor().max(sx0.max(0) as f32) as i32;
        let maxx =
            v.iter().map(|p| p.x).fold(f32::MIN, f32::max).ceil().min((sx1.min(EFB_WIDTH as i32 - 1)) as f32) as i32;
        let miny = v.iter().map(|p| p.y).fold(f32::MAX, f32::min).floor().max(sy0.max(0) as f32) as i32;
        let maxy =
            v.iter().map(|p| p.y).fold(f32::MIN, f32::max).ceil().min((sy1.min(EFB_HEIGHT as i32 - 1)) as f32) as i32;
        let edge = |a: &WinVertex, b: &WinVertex, x: f32, y: f32| (b.x - a.x) * (y - a.y) - (b.y - a.y) * (x - a.x);
        for y in miny..=maxy {
            for x in minx..=maxx {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let w0 = edge(&v[1], &v[2], px, py) / area;
                let w1 = edge(&v[2], &v[0], px, py) / area;
                let w2 = edge(&v[0], &v[1], px, py) / area;
                if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                    continue;
                }
                let idx = y as usize * EFB_WIDTH + x as usize;
                let z = w0 * v[0].z + w1 * v[1].z + w2 * v[2].z;
                if rs.depth_test && !depth_pass(rs.depth_func, z, self.depth[idx]) {
                    continue;
                }
                let iw = w0 * v[0].inv_w + w1 * v[1].inv_w + w2 * v[2].inv_w;
                let persp = |f: &dyn Fn(&WinVertex) -> f32| {
                    (w0 * f(&v[0]) * v[0].inv_w + w1 * f(&v[1]) * v[1].inv_w + w2 * f(&v[2]) * v[2].inv_w) / iw
                };
                let mut inp = TevInputs { regs: *regs, konst: *konst, ..Default::default() };
                for ch in 0..2 {
                    for k in 0..4 {
                        inp.ras[ch][k] = persp(&|p: &WinVertex| p.color[ch][k]);
                    }
                }
                for st in &tev.stages {
                    if !st.tex_enabled {
                        continue;
                    }
                    let c = st.texcoord as usize;
                    let s = persp(&|p: &WinVertex| p.tex[c][0]);
                    let t = persp(&|p: &WinVertex| p.tex[c][1]);
                    if let Some(tex) = &textures[st.texmap as usize] {
                        inp.tex[st.texmap as usize] = tex.sample(s, t);
                    }
                }
                let (src, pass) = evaluate(tev, &inp);
                if !pass {
                    continue;
                }
                let dst_px = self.color[idx];
                let dst = [
                    (dst_px & 0xFF) as f32 / 255.0,
                    ((dst_px >> 8) & 0xFF) as f32 / 255.0,
                    ((dst_px >> 16) & 0xFF) as f32 / 255.0,
                    (dst_px >> 24) as f32 / 255.0,
                ];
                let out = if rs.blend {
                    let sf = factor(rs.blend_src, src, dst, true);
                    let df = factor(rs.blend_dst, src, dst, false);
                    std::array::from_fn(|i| {
                        let v = if rs.blend_subtract { dst[i] - src[i] } else { src[i] * sf[i] + dst[i] * df[i] };
                        v.clamp(0.0, 1.0)
                    })
                } else {
                    src
                };
                let q = |f: f32| (f * 255.0).round() as u32;
                let mut px = dst_px;
                if rs.color_update {
                    px = (px & 0xFF00_0000) | q(out[0]) | q(out[1]) << 8 | q(out[2]) << 16;
                }
                if rs.alpha_update {
                    px = (px & 0x00FF_FFFF) | q(out[3]) << 24;
                }
                self.color[idx] = px;
                if rs.depth_test && rs.depth_write {
                    self.depth[idx] = z;
                }
                self.pixels_written += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wv(x: f32, y: f32, c: [f32; 4]) -> WinVertex {
        WinVertex { x, y, z: 0.5, inv_w: 1.0, color: [c, [0.0; 4]], tex: [[0.0; 2]; 8] }
    }

    #[test]
    fn fills_triangle_and_respects_scissor_and_cull() {
        let mut r = SoftRenderer::new();
        let tev = TevConfig::passthrough();
        let none: [Option<SoftTexture>; 8] = Default::default();
        let red = [1.0, 0.0, 0.0, 1.0];
        let tri = [wv(100.0, 100.0, red), wv(200.0, 100.0, red), wv(150.0, 200.0, red)];
        let rs = RasterState::default();
        r.draw_triangle(tri, &tev, &[[0.0; 4]; 4], &[[0.0; 4]; 4], &none, &rs);
        assert_eq!(r.color[150 * EFB_WIDTH + 150], 0xFF00_00FF);
        assert_eq!(r.color[50 * EFB_WIDTH + 50], 0xFF00_0000);
        // the same triangle is clockwise on screen → culled with cull = back
        let mut culled = SoftRenderer::new();
        culled.draw_triangle(tri, &tev, &[[0.0; 4]; 4], &[[0.0; 4]; 4], &none, &RasterState { cull: 1, ..rs });
        assert_eq!(culled.pixels_written, 0);
        let mut sc = SoftRenderer::new();
        sc.draw_triangle(
            tri,
            &tev,
            &[[0.0; 4]; 4],
            &[[0.0; 4]; 4],
            &none,
            &RasterState { scissor: [0, 0, 120, 527], ..rs },
        );
        assert_eq!(sc.color[150 * EFB_WIDTH + 150], 0xFF00_0000);
        assert_eq!(sc.color[110 * EFB_WIDTH + 110], 0xFF00_00FF);
    }

    #[test]
    fn depth_test_and_alpha_blend() {
        let mut r = SoftRenderer::new();
        let tev = TevConfig::passthrough();
        let none: [Option<SoftTexture>; 8] = Default::default();
        let quad = |c: [f32; 4], z: f32| {
            let mut t = [wv(0.0, 0.0, c), wv(64.0, 0.0, c), wv(0.0, 64.0, c)];
            for p in &mut t {
                p.z = z;
            }
            t
        };
        let depth = RasterState { depth_test: true, depth_func: 1, depth_write: true, ..Default::default() };
        r.draw_triangle(quad([0.0, 1.0, 0.0, 1.0], 0.3), &tev, &[[0.0; 4]; 4], &[[0.0; 4]; 4], &none, &depth);
        r.draw_triangle(quad([1.0, 0.0, 0.0, 1.0], 0.6), &tev, &[[0.0; 4]; 4], &[[0.0; 4]; 4], &none, &depth);
        assert_eq!(r.color[10 * EFB_WIDTH + 10], 0xFF00_FF00, "farther red is rejected");
        let blend = RasterState { blend: true, blend_src: 4, blend_dst: 5, ..Default::default() };
        r.draw_triangle(quad([0.0, 0.0, 1.0, 0.5], 0.1), &tev, &[[0.0; 4]; 4], &[[0.0; 4]; 4], &none, &blend);
        let p = r.color[10 * EFB_WIDTH + 10];
        assert_eq!(((p >> 8) & 0xFF, (p >> 16) & 0xFF), (128, 128), "50% blue over green");
    }
}
