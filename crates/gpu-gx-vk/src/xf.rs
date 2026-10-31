//! Transform unit (XF): matrix memory, lights, projection and viewport,
//! and the vertex transform used by both the software and Vulkan paths.

use crate::vertex::Vertex;

pub const XF_SIZE: usize = 0x1058;

#[derive(Debug, Clone)]
pub struct XfState {
    pub mem: Vec<u32>,
}

impl Default for XfState {
    fn default() -> Self {
        Self { mem: vec![0; XF_SIZE] }
    }
}

/// Clip-space vertex plus the attributes the rasteriser interpolates.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ClipVertex {
    pub clip: [f32; 4],
    pub color: [[f32; 4]; 2],
    pub tex: [[f32; 2]; 8],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    pub wd: f32,
    pub ht: f32,
    pub zrange: f32,
    pub xorig: f32,
    pub yorig: f32,
    pub farz: f32,
}

impl XfState {
    pub fn write(&mut self, addr: u16, values: &[u32]) {
        for (i, &v) in values.iter().enumerate() {
            let a = addr as usize + i;
            if a < XF_SIZE {
                self.mem[a] = v;
            }
        }
    }

    #[inline]
    pub fn f(&self, addr: usize) -> f32 {
        f32::from_bits(self.mem[addr])
    }

    /// 3x4 position matrix at row index `idx` (multiples of 3 select
    /// matrices; the address is `idx * 4`).
    pub fn pos_matrix(&self, idx: u8) -> [[f32; 4]; 3] {
        let base = (idx as usize & 0x3F) * 4;
        std::array::from_fn(|r| std::array::from_fn(|c| self.f(base + r * 4 + c)))
    }

    pub fn viewport(&self) -> Viewport {
        Viewport {
            wd: self.f(0x101A),
            ht: self.f(0x101B),
            zrange: self.f(0x101C),
            xorig: self.f(0x101D),
            yorig: self.f(0x101E),
            farz: self.f(0x101F),
        }
    }

    pub fn orthographic(&self) -> bool {
        self.mem[0x1026] != 0
    }

    pub fn num_texgens(&self) -> usize {
        (self.mem[0x103F] & 0xF) as usize
    }

    /// Channel material colour (register 0x100C/0x100D) when the channel
    /// control selects the register as source.
    fn channel_color(&self, ch: usize, vertex: &Vertex) -> [f32; 4] {
        let ctrl = self.mem[0x100E + ch];
        let from_vertex = ctrl & 1 != 0;
        let c = if from_vertex && vertex.has_color[ch] {
            vertex.color[ch]
        } else if from_vertex && ch == 1 && vertex.has_color[0] {
            vertex.color[0]
        } else {
            self.mem[0x100C + ch]
        };
        [
            (c >> 24) as f32 / 255.0,
            ((c >> 16) & 0xFF) as f32 / 255.0,
            ((c >> 8) & 0xFF) as f32 / 255.0,
            (c & 0xFF) as f32 / 255.0,
        ]
    }

    /// Model → clip space.
    pub fn transform(&self, v: &Vertex) -> ClipVertex {
        let m = self.pos_matrix(v.pnmtx);
        let p = v.pos;
        let eye: [f32; 3] = std::array::from_fn(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + m[r][3]);
        let pr: [f32; 6] = std::array::from_fn(|i| self.f(0x1020 + i));
        let clip = if self.orthographic() {
            [pr[0] * eye[0] + pr[1], pr[2] * eye[1] + pr[3], pr[4] * eye[2] + pr[5], 1.0]
        } else {
            [pr[0] * eye[0] + pr[1] * eye[2], pr[2] * eye[1] + pr[3] * eye[2], pr[4] * eye[2] + pr[5], -eye[2]]
        };
        let mut tex = [[0.0; 2]; 8];
        for (i, t) in tex.iter_mut().enumerate().take(self.num_texgens().max(1)) {
            // Texgen with a 2x4 texture matrix when one is selected.
            let idx = v.texmtx[i] as usize;
            let src = v.tex[i];
            if (30..60).contains(&idx) && idx % 3 == 0 {
                let base = idx * 4;
                let row = |r: usize| {
                    self.f(base + r * 4) * src[0]
                        + self.f(base + r * 4 + 1) * src[1]
                        + self.f(base + r * 4 + 2)
                        + self.f(base + r * 4 + 3)
                };
                let (s, tt) = (row(0), row(1));
                *t = if s.is_finite() && tt.is_finite() && (s != 0.0 || tt != 0.0) { [s, tt] } else { src };
            } else {
                *t = src;
            }
        }
        ClipVertex { clip, color: [self.channel_color(0, v), self.channel_color(1, v)], tex }
    }

    /// Clip → window coordinates (EFB pixels, z in 0..1 range of 24 bits).
    pub fn to_window(&self, c: &ClipVertex) -> [f32; 3] {
        let vp = self.viewport();
        let w = if c.clip[3] == 0.0 { 1e-6 } else { c.clip[3] };
        let x = c.clip[0] / w * vp.wd + (vp.xorig - 342.0);
        let y = c.clip[1] / w * vp.ht + (vp.yorig - 342.0);
        let z = (c.clip[2] / w * vp.zrange + vp.farz) / 16_777_215.0;
        [x, y, z]
    }
}

/// Set up an XF state with an identity position matrix, orthographic
/// projection mapping [-1,1] to a 640x480 viewport, and vertex colours.
pub fn standard_2d(xf: &mut XfState) {
    let one = 1.0f32.to_bits();
    xf.write(0, &[one, 0, 0, 0, 0, one, 0, 0, 0, 0, one, 0]);
    let vp = [320.0f32, -240.0, 16_777_215.0, 342.0 + 320.0, 342.0 + 240.0, 16_777_215.0];
    xf.write(0x101A, &vp.map(f32::to_bits));
    let proj = [1.0f32, 0.0, 1.0, 0.0, -1.0, 0.0];
    xf.write(0x1020, &proj.map(f32::to_bits));
    xf.write(0x1026, &[1]);
    xf.write(0x100E, &[1, 1]);
    xf.write(0x103F, &[1]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orthographic_maps_to_viewport() {
        let mut xf = XfState::default();
        standard_2d(&mut xf);
        let v =
            Vertex { pos: [0.0, 0.0, 0.0], color: [0xFF00_00FF, 0], has_color: [true, false], ..Default::default() };
        let c = xf.transform(&v);
        let w = xf.to_window(&c);
        assert_eq!([w[0], w[1]], [320.0, 240.0]);
        assert_eq!(c.color[0], [1.0, 0.0, 0.0, 1.0]);
        let corner = xf.to_window(&xf.transform(&Vertex { pos: [-1.0, 1.0, 0.0], ..Default::default() }));
        assert_eq!([corner[0], corner[1]], [0.0, 0.0], "top-left");
    }

    #[test]
    fn perspective_divides_by_minus_z() {
        let mut xf = XfState::default();
        standard_2d(&mut xf);
        xf.write(0x1026, &[0]);
        xf.write(0x1020, &[1.0f32, 0.0, 1.0, 0.0, -1.0, -2.0].map(f32::to_bits));
        let c = xf.transform(&Vertex { pos: [2.0, 0.0, -4.0], ..Default::default() });
        assert_eq!(c.clip[3], 4.0);
        assert_eq!(c.clip[0] / c.clip[3], 0.5);
    }
}
