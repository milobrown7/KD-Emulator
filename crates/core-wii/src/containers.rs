//! Wii image containers: WBFS (sector-mapped disc store) and WAD
//! (installable title package).

use core_gcn::disc::Disc;
use std::io;

fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

const WII_SECTOR: u64 = 0x8000;
const WII_SECTORS_PER_DISC: u64 = 143_432 * 2;

/// A single-disc WBFS image.
pub struct WbfsDisc {
    data: Vec<u8>,
    wbfs_sec_shift: u32,
    /// Disc WBFS-sector → image WBFS-sector (0 = unused).
    wlba: Vec<u16>,
}

impl WbfsDisc {
    pub fn new(data: Vec<u8>) -> io::Result<Self> {
        if data.len() < 0x200 || &data[..4] != b"WBFS" {
            return Err(io::Error::other("not a WBFS image"));
        }
        let hd_sec_shift = data[8] as u32;
        let wbfs_sec_shift = data[9] as u32;
        let hd_sec = 1usize << hd_sec_shift;
        if data[12] == 0 {
            return Err(io::Error::other("WBFS image contains no disc"));
        }
        let n = ((WII_SECTORS_PER_DISC * WII_SECTOR) >> wbfs_sec_shift) as usize;
        // Disc 0's info block starts at the second HD sector: 0x100-byte
        // disc header copy, then the 16-bit sector map.
        let table = hd_sec + 0x100;
        let wlba = (0..n)
            .map(|i| data.get(table + i * 2..table + i * 2 + 2).map_or(0, |b| u16::from_be_bytes([b[0], b[1]])))
            .collect();
        Ok(Self { data, wbfs_sec_shift, wlba })
    }

    /// Build a WBFS image holding `disc` (used by tests and the importer).
    pub fn build(disc: &[u8], hd_sec_shift: u8, wbfs_sec_shift: u8) -> Vec<u8> {
        let hd_sec = 1usize << hd_sec_shift;
        let wsec = 1usize << wbfs_sec_shift;
        let n = ((WII_SECTORS_PER_DISC * WII_SECTOR) >> wbfs_sec_shift) as usize;
        let info_len = (0x100 + n * 2).next_multiple_of(hd_sec);
        let data_start = (hd_sec + info_len).next_multiple_of(wsec);
        let mut out = vec![0u8; data_start];
        out[..4].copy_from_slice(b"WBFS");
        out[8] = hd_sec_shift;
        out[9] = wbfs_sec_shift;
        out[12] = 1;
        out[hd_sec..hd_sec + 0x100].copy_from_slice(&disc[..0x100.min(disc.len())]);
        let mut next = (data_start / wsec) as u16;
        for (i, chunk) in disc.chunks(wsec).enumerate() {
            if chunk.iter().all(|&b| b == 0) {
                continue;
            }
            let at = hd_sec + 0x100 + i * 2;
            out[at..at + 2].copy_from_slice(&next.to_be_bytes());
            out.extend_from_slice(chunk);
            out.resize(out.len().next_multiple_of(wsec), 0);
            next += 1;
        }
        out
    }
}

impl Disc for WbfsDisc {
    fn read(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let mask = (1u64 << self.wbfs_sec_shift) - 1;
        let mut done = 0;
        while done < buf.len() {
            let pos = offset + done as u64;
            let sector = (pos >> self.wbfs_sec_shift) as usize;
            let within = (pos & mask) as usize;
            let n = (buf.len() - done).min((mask as usize + 1) - within);
            let out = &mut buf[done..done + n];
            match self.wlba.get(sector).copied().unwrap_or(0) {
                0 => out.fill(0),
                lba => {
                    let src = ((lba as usize) << self.wbfs_sec_shift) + within;
                    match self.data.get(src..src + n) {
                        Some(s) => out.copy_from_slice(s),
                        None => out.fill(0),
                    }
                }
            }
            done += n;
        }
        Ok(())
    }

    fn size(&self) -> u64 {
        WII_SECTORS_PER_DISC * WII_SECTOR
    }
}

/// One content record from a TMD.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Content {
    pub id: u32,
    pub index: u16,
    pub kind: u16,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wad {
    pub kind: u32,
    pub title_id: u64,
    pub contents: Vec<Content>,
    pub cert_chain: Vec<u8>,
    pub ticket: Vec<u8>,
    pub tmd: Vec<u8>,
    /// Encrypted content data (decryption needs the console common key,
    /// which is never bundled).
    pub data: Vec<u8>,
}

impl Wad {
    pub fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < 0x40 {
            return None;
        }
        let header = be32(b, 0) as usize;
        let kind = be32(b, 4);
        let sizes = [be32(b, 8), be32(b, 16), be32(b, 20), be32(b, 24)].map(|s| s as usize);
        let mut at = header.next_multiple_of(0x40);
        let mut take = |len: usize| -> Option<Vec<u8>> {
            let v = b.get(at..at + len)?.to_vec();
            at = (at + len).next_multiple_of(0x40);
            Some(v)
        };
        let cert_chain = take(sizes[0])?;
        let ticket = take(sizes[1])?;
        let tmd = take(sizes[2])?;
        let data = take(sizes[3])?;
        if tmd.len() < 0x1E4 {
            return None;
        }
        let title_id = u64::from_be_bytes(tmd[0x18C..0x194].try_into().ok()?);
        let n = u16::from_be_bytes([tmd[0x1DE], tmd[0x1DF]]) as usize;
        let contents = (0..n)
            .filter_map(|i| {
                let r = tmd.get(0x1E4 + i * 36..0x1E4 + i * 36 + 36)?;
                Some(Content {
                    id: be32(r, 0),
                    index: u16::from_be_bytes([r[4], r[5]]),
                    kind: u16::from_be_bytes([r[6], r[7]]),
                    size: u64::from_be_bytes(r[8..16].try_into().ok()?),
                })
            })
            .collect();
        Some(Self { kind, title_id, contents, cert_chain, ticket, tmd, data })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wbfs_round_trip_with_sparse_sectors() {
        let mut disc = vec![0u8; 0x80_0000];
        disc[..6].copy_from_slice(b"RUUE01");
        disc[0x18..0x1C].copy_from_slice(&core_gcn::disc::WII_MAGIC.to_be_bytes());
        disc[0x40_0000..0x40_0004].copy_from_slice(b"DATA");
        let img = WbfsDisc::build(&disc, 9, 21);
        assert!(img.len() < disc.len(), "zero sectors are not stored");
        let mut w = WbfsDisc::new(img).unwrap();
        let mut buf = [0u8; 6];
        w.read(0, &mut buf).unwrap();
        assert_eq!(&buf, b"RUUE01");
        let mut b4 = [0u8; 4];
        w.read(0x40_0000, &mut b4).unwrap();
        assert_eq!(&b4, b"DATA");
        w.read(0x20_0000, &mut b4).unwrap();
        assert_eq!(b4, [0; 4]);
        let header = core_gcn::disc::DiscHeader::read(&mut w).unwrap();
        assert!(header.is_wii);
        assert!(kd_core::animal_crossing::lookup(kd_core::System::Wii, &header.game_id).is_some());
    }

    #[test]
    fn wad_parsing() {
        let mut tmd = vec![0u8; 0x1E4 + 36];
        tmd[0x18C..0x194].copy_from_slice(&0x0001_0001_5255_5545u64.to_be_bytes());
        tmd[0x1DE..0x1E0].copy_from_slice(&1u16.to_be_bytes());
        tmd[0x1E4..0x1E8].copy_from_slice(&0x0000_0007u32.to_be_bytes());
        tmd[0x1E4 + 8..0x1E4 + 16].copy_from_slice(&0x40u64.to_be_bytes());
        let mut wad = vec![0u8; 0x40];
        wad[0..4].copy_from_slice(&0x20u32.to_be_bytes());
        wad[4..8].copy_from_slice(&0x4973_0000u32.to_be_bytes());
        wad[8..12].copy_from_slice(&0x10u32.to_be_bytes());
        wad[16..20].copy_from_slice(&0x10u32.to_be_bytes());
        wad[20..24].copy_from_slice(&(tmd.len() as u32).to_be_bytes());
        wad[24..28].copy_from_slice(&0x40u32.to_be_bytes());
        wad.extend([0xC1; 0x10]);
        wad.resize(0x80, 0);
        wad.extend([0x71; 0x10]);
        wad.resize(0xC0, 0);
        wad.extend(&tmd);
        wad.resize(wad.len().next_multiple_of(0x40), 0);
        wad.extend([0xDA; 0x40]);
        let w = Wad::parse(&wad).unwrap();
        assert_eq!(w.title_id, 0x0001_0001_5255_5545);
        assert_eq!(w.contents, vec![Content { id: 7, index: 0, kind: 0, size: 0x40 }]);
        assert_eq!(w.data, vec![0xDA; 0x40]);
    }
}
