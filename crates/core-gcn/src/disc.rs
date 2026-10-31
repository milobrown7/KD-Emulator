//! Disc images and executables: plain ISO/GCM images, the boot header,
//! apploader header and the DOL executable format.

use std::io;

/// Random-access disc image.
pub trait Disc: Send {
    fn read(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()>;
    fn size(&self) -> u64;
}

/// A GameCube/Wii disc held in memory or memory-mapped from a file
/// (.iso / .gcm).
pub struct IsoDisc {
    data: Vec<u8>,
}

impl IsoDisc {
    pub fn new(data: Vec<u8>) -> Self {
        Self { data }
    }
}

impl Disc for IsoDisc {
    fn read(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let start = offset as usize;
        let end = start.checked_add(buf.len()).ok_or_else(|| io::Error::other("overflow"))?;
        if end > self.data.len() {
            // Reads past the end return zeros (the drive pads short images).
            buf.fill(0);
            if start < self.data.len() {
                let n = self.data.len() - start;
                buf[..n].copy_from_slice(&self.data[start..]);
            }
            return Ok(());
        }
        buf.copy_from_slice(&self.data[start..end]);
        Ok(())
    }
    fn size(&self) -> u64 {
        self.data.len() as u64
    }
}

/// A disc image read on demand from an open file (no full copy in RAM —
/// GameCube/Wii images are 1.4–8.5 GB).
pub struct FileDisc {
    file: std::fs::File,
    size: u64,
}

impl FileDisc {
    pub fn new(file: std::fs::File) -> io::Result<Self> {
        let size = file.metadata()?.len();
        Ok(Self { file, size })
    }
}

impl Disc for FileDisc {
    fn read(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        use std::os::unix::fs::FileExt;
        buf.fill(0);
        let mut done = 0;
        while done < buf.len() {
            let n = self.file.read_at(&mut buf[done..], offset + done as u64)?;
            if n == 0 {
                break; // past the end: zero padded
            }
            done += n;
        }
        Ok(())
    }
    fn size(&self) -> u64 {
        self.size
    }
}

pub const GC_MAGIC: u32 = 0xC233_9F3D;
pub const WII_MAGIC: u32 = 0x5D1C_9EA3;

fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// The 0x440-byte disc header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscHeader {
    pub game_id: [u8; 6],
    pub disc_number: u8,
    pub version: u8,
    pub title: String,
    pub is_wii: bool,
    pub is_gamecube: bool,
    pub dol_offset: u32,
    pub fst_offset: u32,
    pub fst_size: u32,
}

impl DiscHeader {
    pub fn read(disc: &mut dyn Disc) -> io::Result<Self> {
        let mut h = vec![0u8; 0x440];
        disc.read(0, &mut h)?;
        let mut game_id = [0u8; 6];
        game_id.copy_from_slice(&h[..6]);
        Ok(Self {
            game_id,
            disc_number: h[6],
            version: h[7],
            title: String::from_utf8_lossy(&h[0x20..0x60]).trim_end_matches('\0').to_string(),
            is_wii: be32(&h, 0x18) == WII_MAGIC,
            is_gamecube: be32(&h, 0x1C) == GC_MAGIC,
            dol_offset: be32(&h, 0x420),
            fst_offset: be32(&h, 0x424),
            fst_size: be32(&h, 0x428),
        })
    }
}

/// A parsed DOL executable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dol {
    /// (load address, bytes) for every non-empty text and data section.
    pub sections: Vec<(u32, Vec<u8>)>,
    pub bss_addr: u32,
    pub bss_size: u32,
    pub entry: u32,
}

impl Dol {
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 0x100 {
            return None;
        }
        let mut sections = Vec::new();
        for i in 0..18 {
            let off = be32(data, i * 4) as usize;
            let addr = be32(data, 0x48 + i * 4);
            let size = be32(data, 0x90 + i * 4) as usize;
            if size == 0 {
                continue;
            }
            let bytes = data.get(off..off.checked_add(size)?)?.to_vec();
            sections.push((addr, bytes));
        }
        Some(Self { sections, bss_addr: be32(data, 0xD8), bss_size: be32(data, 0xDC), entry: be32(data, 0xE0) })
    }

    /// Build a DOL image (used by tests and the homebrew launcher).
    pub fn build(text: &[(u32, &[u8])], data: &[(u32, &[u8])], entry: u32) -> Vec<u8> {
        let mut out = vec![0u8; 0x100];
        let put = |out: &mut Vec<u8>, slot: usize, addr: u32, bytes: &[u8]| {
            let off = out.len() as u32;
            out[slot * 4..slot * 4 + 4].copy_from_slice(&off.to_be_bytes());
            out[0x48 + slot * 4..0x4C + slot * 4].copy_from_slice(&addr.to_be_bytes());
            out[0x90 + slot * 4..0x94 + slot * 4].copy_from_slice(&(bytes.len() as u32).to_be_bytes());
            out.extend_from_slice(bytes);
        };
        for (i, (addr, b)) in text.iter().enumerate().take(7) {
            put(&mut out, i, *addr, b);
        }
        for (i, (addr, b)) in data.iter().enumerate().take(11) {
            put(&mut out, 7 + i, *addr, b);
        }
        out[0xE0..0xE4].copy_from_slice(&entry.to_be_bytes());
        out
    }
}

/// Apploader header at disc offset 0x2440.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApploaderHeader {
    pub entry: u32,
    pub size: u32,
    pub trailer: u32,
}

impl ApploaderHeader {
    pub const OFFSET: u64 = 0x2440;

    pub fn read(disc: &mut dyn Disc) -> io::Result<Self> {
        let mut h = [0u8; 0x20];
        disc.read(Self::OFFSET, &mut h)?;
        Ok(Self { entry: be32(&h, 0x10), size: be32(&h, 0x14), trailer: be32(&h, 0x18) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_detects_animal_crossing_gamecube_disc() {
        let mut img = vec![0u8; 0x2460];
        img[..6].copy_from_slice(b"GAFE01");
        img[0x1C..0x20].copy_from_slice(&GC_MAGIC.to_be_bytes());
        img[0x20..0x2F].copy_from_slice(b"Animal Crossing");
        img[0x420..0x424].copy_from_slice(&0x1_0000u32.to_be_bytes());
        let mut d = IsoDisc::new(img);
        let h = DiscHeader::read(&mut d).unwrap();
        assert_eq!(&h.game_id, b"GAFE01");
        assert!(h.is_gamecube && !h.is_wii);
        assert_eq!(h.title, "Animal Crossing");
        assert_eq!(h.dol_offset, 0x1_0000);
        assert!(kd_core::animal_crossing::lookup(kd_core::System::GameCube, &h.game_id).is_some());
    }

    #[test]
    fn dol_round_trip() {
        let text = [0x60u8, 0, 0, 0];
        let dol = Dol::build(&[(0x8000_3100, &text)], &[(0x8000_4000, &[1, 2, 3, 4])], 0x8000_3100);
        let parsed = Dol::parse(&dol).unwrap();
        assert_eq!(parsed.entry, 0x8000_3100);
        assert_eq!(parsed.sections, vec![(0x8000_3100, text.to_vec()), (0x8000_4000, vec![1, 2, 3, 4])]);
        assert!(Dol::parse(&dol[..0x40]).is_none());
    }

    #[test]
    fn short_reads_pad_with_zero() {
        let mut d = IsoDisc::new(vec![9; 4]);
        let mut b = [1u8; 8];
        d.read(2, &mut b).unwrap();
        assert_eq!(b, [9, 9, 0, 0, 0, 0, 0, 0]);
    }
}
