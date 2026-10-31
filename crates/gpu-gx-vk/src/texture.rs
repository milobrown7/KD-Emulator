//! GX texture decoders (tiled big-endian formats → linear RGBA8).
//!
//! Output pixels are `u32` with R in the low byte (RGBA8888 in memory),
//! matching the EFB and Vulkan `R8G8B8A8_UNORM` layout.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TexFormat {
    I4 = 0,
    I8 = 1,
    IA4 = 2,
    IA8 = 3,
    Rgb565 = 4,
    Rgb5a3 = 5,
    Rgba8 = 6,
    C4 = 8,
    C8 = 9,
    C14x2 = 10,
    Cmpr = 14,
}

impl TexFormat {
    pub fn from_bits(v: u32) -> Option<Self> {
        Some(match v {
            0 => TexFormat::I4,
            1 => TexFormat::I8,
            2 => TexFormat::IA4,
            3 => TexFormat::IA8,
            4 => TexFormat::Rgb565,
            5 => TexFormat::Rgb5a3,
            6 => TexFormat::Rgba8,
            8 => TexFormat::C4,
            9 => TexFormat::C8,
            10 => TexFormat::C14x2,
            14 => TexFormat::Cmpr,
            _ => return None,
        })
    }

    /// Block dimensions in texels.
    pub fn block(self) -> (usize, usize) {
        match self {
            TexFormat::I4 | TexFormat::C4 | TexFormat::Cmpr => (8, 8),
            TexFormat::I8 | TexFormat::IA4 | TexFormat::C8 => (8, 4),
            _ => (4, 4),
        }
    }

    /// Bytes per block (RGBA8 blocks are stored as two 32-byte halves).
    pub fn block_bytes(self) -> usize {
        if self == TexFormat::Rgba8 {
            64
        } else {
            32
        }
    }

    /// Encoded size of a `w`x`h` texture.
    pub fn size(self, w: usize, h: usize) -> usize {
        let (bw, bh) = self.block();
        w.div_ceil(bw) * h.div_ceil(bh) * self.block_bytes()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlutFormat {
    Ia8 = 0,
    Rgb565 = 1,
    Rgb5a3 = 2,
}

#[inline]
pub fn rgba(r: u32, g: u32, b: u32, a: u32) -> u32 {
    r | g << 8 | b << 16 | a << 24
}

#[inline]
fn ex5(v: u32) -> u32 {
    (v << 3) | (v >> 2)
}
#[inline]
fn ex6(v: u32) -> u32 {
    (v << 2) | (v >> 4)
}
#[inline]
fn ex4(v: u32) -> u32 {
    v * 0x11
}
#[inline]
fn ex3(v: u32) -> u32 {
    (v << 5) | (v << 2) | (v >> 1)
}

pub fn decode_rgb565(v: u16) -> u32 {
    let v = v as u32;
    rgba(ex5(v >> 11), ex6((v >> 5) & 63), ex5(v & 31), 255)
}

pub fn decode_rgb5a3(v: u16) -> u32 {
    let v = v as u32;
    if v & 0x8000 != 0 {
        rgba(ex5((v >> 10) & 31), ex5((v >> 5) & 31), ex5(v & 31), 255)
    } else {
        rgba(ex4((v >> 8) & 15), ex4((v >> 4) & 15), ex4(v & 15), ex3((v >> 12) & 7))
    }
}

pub fn decode_ia8(v: u16) -> u32 {
    let i = (v & 0xFF) as u32;
    rgba(i, i, i, (v >> 8) as u32)
}

fn tlut_color(tlut: &[u8], fmt: TlutFormat, idx: usize) -> u32 {
    let v = tlut.get(idx * 2..idx * 2 + 2).map_or(0, |b| u16::from_be_bytes([b[0], b[1]]));
    match fmt {
        TlutFormat::Ia8 => decode_ia8(v),
        TlutFormat::Rgb565 => decode_rgb565(v),
        TlutFormat::Rgb5a3 => decode_rgb5a3(v),
    }
}

fn be16(d: &[u8], i: usize) -> u16 {
    u16::from_be_bytes([d[i], d[i + 1]])
}

/// Decode one CMPR (S3TC/DXT1, big-endian) 4x4 sub-block.
fn decode_dxt1(src: &[u8], out: &mut [u32; 16]) {
    let c0 = be16(src, 0);
    let c1 = be16(src, 2);
    let p0 = decode_rgb565(c0);
    let p1 = decode_rgb565(c1);
    let ch = |p: u32, s: u32| (p >> s) & 0xFF;
    let mix = |a: u32, b: u32, wa: u32, wb: u32| {
        let m = |s| (ch(a, s) * wa + ch(b, s) * wb) / (wa + wb);
        rgba(m(0), m(8), m(16), 255)
    };
    let palette = if c0 > c1 { [p0, p1, mix(p0, p1, 2, 1), mix(p0, p1, 1, 2)] } else { [p0, p1, mix(p0, p1, 1, 1), 0] };
    for y in 0..4 {
        let row = src[4 + y];
        for x in 0..4 {
            out[y * 4 + x] = palette[((row >> (6 - x * 2)) & 3) as usize];
        }
    }
}

/// Decode a texture. `tlut` is the palette memory (big-endian entries) for
/// C4/C8/C14X2.
pub fn decode(fmt: TexFormat, data: &[u8], w: usize, h: usize, tlut: &[u8], tlut_fmt: TlutFormat) -> Vec<u32> {
    let mut out = vec![0u32; w * h];
    let (bw, bh) = fmt.block();
    let blocks_x = w.div_ceil(bw);
    let bb = fmt.block_bytes();
    let mut put = |x: usize, y: usize, c: u32| {
        if x < w && y < h {
            out[y * w + x] = c;
        }
    };
    for by in 0..h.div_ceil(bh) {
        for bx in 0..blocks_x {
            let off = (by * blocks_x + bx) * bb;
            let Some(blk) = data.get(off..off + bb) else { continue };
            let (x0, y0) = (bx * bw, by * bh);
            match fmt {
                TexFormat::I4 | TexFormat::C4 => {
                    for i in 0..64 {
                        let b = blk[i / 2];
                        let v = if i % 2 == 0 { b >> 4 } else { b & 15 } as u32;
                        let c = if fmt == TexFormat::I4 {
                            let i8v = ex4(v);
                            rgba(i8v, i8v, i8v, i8v)
                        } else {
                            tlut_color(tlut, tlut_fmt, v as usize)
                        };
                        put(x0 + i % 8, y0 + i / 8, c);
                    }
                }
                TexFormat::I8 | TexFormat::C8 => {
                    for (i, &b) in blk.iter().enumerate() {
                        let v = b as u32;
                        let c = if fmt == TexFormat::I8 { rgba(v, v, v, v) } else { tlut_color(tlut, tlut_fmt, v as usize) };
                        put(x0 + i % 8, y0 + i / 8, c);
                    }
                }
                TexFormat::IA4 => {
                    for (i, &b) in blk.iter().enumerate() {
                        let a = ex4((b >> 4) as u32);
                        let l = ex4((b & 15) as u32);
                        put(x0 + i % 8, y0 + i / 8, rgba(l, l, l, a));
                    }
                }
                TexFormat::IA8 | TexFormat::Rgb565 | TexFormat::Rgb5a3 | TexFormat::C14x2 => {
                    for i in 0..16 {
                        let v = be16(blk, i * 2);
                        let c = match fmt {
                            TexFormat::IA8 => decode_ia8(v),
                            TexFormat::Rgb565 => decode_rgb565(v),
                            TexFormat::Rgb5a3 => decode_rgb5a3(v),
                            _ => tlut_color(tlut, tlut_fmt, (v & 0x3FFF) as usize),
                        };
                        put(x0 + i % 4, y0 + i / 4, c);
                    }
                }
                TexFormat::Rgba8 => {
                    for i in 0..16 {
                        let a = blk[i * 2] as u32;
                        let r = blk[i * 2 + 1] as u32;
                        let g = blk[32 + i * 2] as u32;
                        let b = blk[32 + i * 2 + 1] as u32;
                        put(x0 + i % 4, y0 + i / 4, rgba(r, g, b, a));
                    }
                }
                TexFormat::Cmpr => {
                    let mut sub = [0u32; 16];
                    for s in 0..4 {
                        decode_dxt1(&blk[s * 8..s * 8 + 8], &mut sub);
                        let (sx, sy) = (x0 + (s % 2) * 4, y0 + (s / 2) * 4);
                        for (i, &c) in sub.iter().enumerate() {
                            put(sx + i % 4, sy + i / 4, c);
                        }
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb5a3_both_modes() {
        assert_eq!(decode_rgb5a3(0xFC00), rgba(255, 0, 0, 255));
        assert_eq!(decode_rgb5a3(0x7F00), rgba(255, 0, 0, 255), "alpha 7 → 255");
        assert_eq!(decode_rgb5a3(0x0F00) >> 24, 0);
    }

    #[test]
    fn i8_block_layout() {
        // 16x4 I8 texture = two 8x4 blocks; block 1 is filled with 0x80.
        let mut data = vec![0u8; 64];
        data[32..].fill(0x80);
        data[0] = 0xFF;
        let px = decode(TexFormat::I8, &data, 16, 4, &[], TlutFormat::Ia8);
        assert_eq!(px[0], rgba(255, 255, 255, 255));
        assert_eq!(px[1], 0);
        assert_eq!(px[8], rgba(128, 128, 128, 128));
        assert_eq!(px[3 * 16 + 15], rgba(128, 128, 128, 128));
    }

    #[test]
    fn rgba8_split_halves() {
        let mut data = vec![0u8; 64];
        data[0] = 0x11; // A
        data[1] = 0x22; // R
        data[32] = 0x33; // G
        data[33] = 0x44; // B
        let px = decode(TexFormat::Rgba8, &data, 4, 4, &[], TlutFormat::Ia8);
        assert_eq!(px[0], rgba(0x22, 0x33, 0x44, 0x11));
    }

    #[test]
    fn c8_through_rgb565_palette() {
        let mut tlut = vec![0u8; 512];
        tlut[2 * 7..2 * 7 + 2].copy_from_slice(&0x07E0u16.to_be_bytes()); // green
        let data = vec![7u8; 32];
        let px = decode(TexFormat::C8, &data, 8, 4, &tlut, TlutFormat::Rgb565);
        assert!(px.iter().all(|&p| p == rgba(0, 255, 0, 255)));
    }

    #[test]
    fn cmpr_subblocks() {
        // 8x8 CMPR: four DXT1 blocks; first all colour0 (red), others colour1 (blue)
        let mut data = vec![0u8; 32];
        for s in 0..4 {
            let blk = &mut data[s * 8..s * 8 + 8];
            blk[0..2].copy_from_slice(&0xF800u16.to_be_bytes());
            blk[2..4].copy_from_slice(&0x001Fu16.to_be_bytes());
            let idx = if s == 0 { 0x00 } else { 0x55 };
            blk[4..8].fill(idx);
        }
        let px = decode(TexFormat::Cmpr, &data, 8, 8, &[], TlutFormat::Ia8);
        assert_eq!(px[0], rgba(255, 0, 0, 255));
        assert_eq!(px[4], rgba(0, 0, 255, 255));
        assert_eq!(px[4 * 8], rgba(0, 0, 255, 255));
    }

    #[test]
    fn sizes() {
        assert_eq!(TexFormat::I4.size(16, 16), 4 * 32);
        assert_eq!(TexFormat::Rgba8.size(4, 4), 64);
        assert_eq!(TexFormat::Cmpr.size(10, 10), 4 * 32);
    }
}
