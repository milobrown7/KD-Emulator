//! GX command FIFO parser (the stream the CPU writes to 0xCC008000 and
//! display lists replay).

use crate::vertex::CpState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Primitive {
    Quads,
    Quads2,
    Triangles,
    TriangleStrip,
    TriangleFan,
    Lines,
    LineStrip,
    Points,
}

impl Primitive {
    fn from_opcode(op: u8) -> Self {
        match (op >> 3) & 7 {
            0 => Primitive::Quads,
            1 => Primitive::Quads2,
            2 => Primitive::Triangles,
            3 => Primitive::TriangleStrip,
            4 => Primitive::TriangleFan,
            5 => Primitive::Lines,
            6 => Primitive::LineStrip,
            _ => Primitive::Points,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GxCommand {
    Nop,
    LoadCp { reg: u8, value: u32 },
    LoadXf { addr: u16, values: Vec<u32> },
    /// LOAD_INDX_A..D: copy `len` words from array `array` element `index`
    /// into XF memory at `addr`.
    LoadIndexed { array: u8, index: u16, len: u8, addr: u16 },
    CallDisplayList { addr: u32, size: u32 },
    InvalidateVertexCache,
    LoadBp { reg: u8, value: u32 },
    Draw { primitive: Primitive, vat: u8, count: u16, data: Vec<u8> },
    Unknown(u8),
}

/// Incremental parser: push bytes, pull complete commands. Draw commands
/// need the current CP state to know the vertex size, so the caller must
/// apply each command before asking for the next one.
#[derive(Debug, Default, Clone)]
pub struct FifoParser {
    buf: Vec<u8>,
    pos: usize,
}

fn be16(b: &[u8]) -> u16 {
    u16::from_be_bytes([b[0], b[1]])
}
fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

impl FifoParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, bytes: &[u8]) {
        if self.pos > 0 && self.pos == self.buf.len() {
            self.buf.clear();
            self.pos = 0;
        }
        self.buf.extend_from_slice(bytes);
    }

    /// Bytes waiting to be parsed.
    pub fn pending(&self) -> usize {
        self.buf.len() - self.pos
    }

    pub fn next(&mut self, cp: &CpState) -> Option<GxCommand> {
        let b = &self.buf[self.pos..];
        let op = *b.first()?;
        let (cmd, used) = match op {
            0x00 => (GxCommand::Nop, 1),
            0x08 => {
                if b.len() < 6 {
                    return None;
                }
                (GxCommand::LoadCp { reg: b[1], value: be32(&b[2..]) }, 6)
            }
            0x10 => {
                if b.len() < 5 {
                    return None;
                }
                let hdr = be32(&b[1..]);
                let n = ((hdr >> 16) & 0xF) as usize + 1;
                if b.len() < 5 + n * 4 {
                    return None;
                }
                let values = (0..n).map(|i| be32(&b[5 + i * 4..])).collect();
                (GxCommand::LoadXf { addr: hdr as u16, values }, 5 + n * 4)
            }
            0x20 | 0x28 | 0x30 | 0x38 => {
                if b.len() < 5 {
                    return None;
                }
                let v = be32(&b[1..]);
                (
                    GxCommand::LoadIndexed {
                        array: (op - 0x20) / 8,
                        index: (v >> 16) as u16,
                        len: ((v >> 12) & 0xF) as u8 + 1,
                        addr: (v & 0xFFF) as u16,
                    },
                    5,
                )
            }
            0x40 => {
                if b.len() < 9 {
                    return None;
                }
                (GxCommand::CallDisplayList { addr: be32(&b[1..]), size: be32(&b[5..]) }, 9)
            }
            0x48 => (GxCommand::InvalidateVertexCache, 1),
            0x61 => {
                if b.len() < 5 {
                    return None;
                }
                let v = be32(&b[1..]);
                (GxCommand::LoadBp { reg: (v >> 24) as u8, value: v & 0xFF_FFFF }, 5)
            }
            0x80..=0xBF => {
                if b.len() < 3 {
                    return None;
                }
                let vat = op & 7;
                let count = be16(&b[1..]);
                let size = cp.vertex_size(vat as usize) * count as usize;
                if b.len() < 3 + size {
                    return None;
                }
                let data = b[3..3 + size].to_vec();
                (GxCommand::Draw { primitive: Primitive::from_opcode(op), vat, count, data }, 3 + size)
            }
            _ => (GxCommand::Unknown(op), 1),
        };
        self.pos += used;
        Some(cmd)
    }
}

/// Split primitives into triangles (vertex index triples), honouring the
/// GX winding of strips and fans.
pub fn triangulate(primitive: Primitive, n: usize) -> Vec<[usize; 3]> {
    let mut t = Vec::new();
    match primitive {
        Primitive::Triangles => {
            for i in (0..n / 3 * 3).step_by(3) {
                t.push([i, i + 1, i + 2]);
            }
        }
        Primitive::TriangleStrip => {
            for i in 2..n {
                if i % 2 == 0 {
                    t.push([i - 2, i - 1, i]);
                } else {
                    t.push([i - 1, i - 2, i]);
                }
            }
        }
        Primitive::TriangleFan => {
            for i in 2..n {
                t.push([0, i - 1, i]);
            }
        }
        Primitive::Quads | Primitive::Quads2 => {
            for q in (0..n / 4 * 4).step_by(4) {
                t.push([q, q + 1, q + 2]);
                t.push([q, q + 2, q + 3]);
            }
        }
        _ => {}
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_register_loads_and_draws_across_partial_pushes() {
        let mut cp = CpState::default();
        let mut p = FifoParser::new();
        let mut stream = vec![0x08, 0x50];
        stream.extend((1u32 << 9).to_be_bytes()); // VCD: POS direct
        stream.extend([0x08, 0x70]);
        stream.extend(0u32.to_be_bytes()); // VAT0: XY u8 → 2 bytes/vertex
        stream.extend([0x61, 0x28, 0x00, 0x00, 0x49]);
        stream.extend([0x90, 0x00, 0x03, 1, 2, 3, 4, 5, 6]); // 3 triangle vertices
        stream.extend([0x10, 0x00, 0x01, 0x10, 0x1A]); // XF load 2 words at 0x101A
        stream.extend(1.0f32.to_be_bytes());
        stream.extend(2.0f32.to_be_bytes());
        // push in awkward chunks
        let mut cmds = vec![];
        for chunk in stream.chunks(4) {
            p.push(chunk);
            while let Some(c) = p.next(&cp) {
                if let GxCommand::LoadCp { reg, value } = c {
                    cp.write(reg, value);
                }
                cmds.push(c);
            }
        }
        assert_eq!(cmds.len(), 5);
        assert_eq!(cmds[2], GxCommand::LoadBp { reg: 0x28, value: 0x49 });
        assert_eq!(
            cmds[3],
            GxCommand::Draw { primitive: Primitive::Triangles, vat: 0, count: 3, data: vec![1, 2, 3, 4, 5, 6] }
        );
        match &cmds[4] {
            GxCommand::LoadXf { addr, values } => {
                assert_eq!(*addr, 0x101A);
                assert_eq!(values, &vec![1.0f32.to_bits(), 2.0f32.to_bits()]);
            }
            c => panic!("{c:?}"),
        }
        assert_eq!(p.pending(), 0);
    }

    #[test]
    fn triangulation() {
        assert_eq!(triangulate(Primitive::Quads, 4), vec![[0, 1, 2], [0, 2, 3]]);
        assert_eq!(triangulate(Primitive::TriangleStrip, 4), vec![[0, 1, 2], [2, 1, 3]]);
        assert_eq!(triangulate(Primitive::TriangleFan, 4), vec![[0, 1, 2], [0, 2, 3]]);
    }
}
