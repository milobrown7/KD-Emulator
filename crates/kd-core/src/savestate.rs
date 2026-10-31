//! Versioned binary save states.
//!
//! Layout:
//! ```text
//! "KDST" | u16 format version | u8 system | u8 reserved | 6-byte game id |
//! u32 payload length | u32 CRC-32 of payload | payload...
//! ```
//! The payload is a sequence of tagged sections (`u32` FourCC + `u32` length)
//! so newer builds can skip sections they do not understand and older builds
//! fail loudly instead of silently misreading.

use std::fmt;

pub const MAGIC: [u8; 4] = *b"KDST";
pub const FORMAT_VERSION: u16 = 1;
const HEADER_LEN: usize = 22;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateError {
    BadMagic,
    UnsupportedVersion(u16),
    WrongSystem { expected: u8, found: u8 },
    Truncated,
    ChecksumMismatch,
    MissingSection([u8; 4]),
    Invalid(&'static str),
}

impl fmt::Display for StateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StateError::BadMagic => write!(f, "not a KD-Emulator save state"),
            StateError::UnsupportedVersion(v) => write!(f, "unsupported save state version {v}"),
            StateError::WrongSystem { expected, found } => {
                write!(f, "save state is for system {found}, expected {expected}")
            }
            StateError::Truncated => write!(f, "save state is truncated"),
            StateError::ChecksumMismatch => write!(f, "save state checksum mismatch"),
            StateError::MissingSection(t) => write!(f, "missing section {}", String::from_utf8_lossy(t)),
            StateError::Invalid(why) => write!(f, "invalid save state: {why}"),
        }
    }
}

impl std::error::Error for StateError {}

pub type StateResult<T> = Result<T, StateError>;

/// Anything that can be captured in a save state.
pub trait SaveState {
    fn save_state(&self, w: &mut StateWriter);
    fn load_state(&mut self, r: &mut StateReader<'_>) -> StateResult<()>;
}

/// Little-endian payload writer.
#[derive(Debug, Default, Clone)]
pub struct StateWriter {
    buf: Vec<u8>,
    open_sections: Vec<usize>,
}

impl StateWriter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    pub fn bool(&mut self, v: bool) {
        self.buf.push(v as u8);
    }
    pub fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn i32(&mut self, v: i32) {
        self.u32(v as u32);
    }
    pub fn i64(&mut self, v: i64) {
        self.u64(v as u64);
    }
    pub fn f32(&mut self, v: f32) {
        self.u32(v.to_bits());
    }
    pub fn f64(&mut self, v: f64) {
        self.u64(v.to_bits());
    }
    /// Raw bytes with a length prefix.
    pub fn bytes(&mut self, v: &[u8]) {
        self.u32(v.len() as u32);
        self.buf.extend_from_slice(v);
    }
    pub fn u16s(&mut self, v: &[u16]) {
        self.u32(v.len() as u32);
        for &x in v {
            self.u16(x);
        }
    }
    pub fn u32s(&mut self, v: &[u32]) {
        self.u32(v.len() as u32);
        for &x in v {
            self.u32(x);
        }
    }

    /// Open a tagged section; must be paired with [`StateWriter::end_section`].
    pub fn begin_section(&mut self, tag: [u8; 4]) {
        self.buf.extend_from_slice(&tag);
        self.open_sections.push(self.buf.len());
        self.u32(0);
    }

    pub fn end_section(&mut self) {
        let at = self.open_sections.pop().expect("end_section without begin_section");
        let len = (self.buf.len() - at - 4) as u32;
        self.buf[at..at + 4].copy_from_slice(&len.to_le_bytes());
    }

    /// Write `value` inside its own tagged section.
    pub fn section<T: SaveState + ?Sized>(&mut self, tag: [u8; 4], value: &T) {
        self.begin_section(tag);
        value.save_state(self);
        self.end_section();
    }

    pub fn into_payload(self) -> Vec<u8> {
        assert!(self.open_sections.is_empty(), "unterminated save state section");
        self.buf
    }

    /// Wrap the payload with the file header.
    pub fn finish(self, system: u8, game_id: [u8; 6]) -> Vec<u8> {
        let payload = self.into_payload();
        let mut out = Vec::with_capacity(payload.len() + HEADER_LEN);
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        out.push(system);
        out.push(0);
        out.extend_from_slice(&game_id);
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&crc32(&payload).to_le_bytes());
        out.extend_from_slice(&payload);
        out
    }
}

/// Header information parsed from a save state file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateHeader {
    pub version: u16,
    pub system: u8,
    pub game_id: [u8; 6],
}

/// Little-endian payload reader.
#[derive(Debug, Clone)]
pub struct StateReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> StateReader<'a> {
    pub fn new(payload: &'a [u8]) -> Self {
        Self { data: payload, pos: 0 }
    }

    /// Validate the header and checksum of a full state file.
    pub fn open(file: &'a [u8], expected_system: u8) -> StateResult<(StateHeader, Self)> {
        if file.len() < HEADER_LEN {
            return Err(StateError::Truncated);
        }
        if file[0..4] != MAGIC {
            return Err(StateError::BadMagic);
        }
        let version = u16::from_le_bytes([file[4], file[5]]);
        if version != FORMAT_VERSION {
            return Err(StateError::UnsupportedVersion(version));
        }
        let system = file[6];
        if system != expected_system {
            return Err(StateError::WrongSystem { expected: expected_system, found: system });
        }
        let mut game_id = [0u8; 6];
        game_id.copy_from_slice(&file[8..14]);
        let len = u32::from_le_bytes(file[14..18].try_into().unwrap()) as usize;
        let crc = u32::from_le_bytes(file[18..22].try_into().unwrap());
        let payload = file.get(22..22 + len).ok_or(StateError::Truncated)?;
        if crc32(payload) != crc {
            return Err(StateError::ChecksumMismatch);
        }
        Ok((StateHeader { version, system, game_id }, Self::new(payload)))
    }

    fn take(&mut self, n: usize) -> StateResult<&'a [u8]> {
        let s = self.data.get(self.pos..self.pos + n).ok_or(StateError::Truncated)?;
        self.pos += n;
        Ok(s)
    }

    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    pub fn u8(&mut self) -> StateResult<u8> {
        Ok(self.take(1)?[0])
    }
    pub fn bool(&mut self) -> StateResult<bool> {
        Ok(self.u8()? != 0)
    }
    pub fn u16(&mut self) -> StateResult<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    pub fn u32(&mut self) -> StateResult<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub fn u64(&mut self) -> StateResult<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    pub fn i32(&mut self) -> StateResult<i32> {
        Ok(self.u32()? as i32)
    }
    pub fn i64(&mut self) -> StateResult<i64> {
        Ok(self.u64()? as i64)
    }
    pub fn f32(&mut self) -> StateResult<f32> {
        Ok(f32::from_bits(self.u32()?))
    }
    pub fn f64(&mut self) -> StateResult<f64> {
        Ok(f64::from_bits(self.u64()?))
    }
    pub fn bytes(&mut self) -> StateResult<&'a [u8]> {
        let n = self.u32()? as usize;
        self.take(n)
    }
    /// Read a length-prefixed byte blob into a fixed-size destination.
    pub fn bytes_into(&mut self, dst: &mut [u8]) -> StateResult<()> {
        let src = self.bytes()?;
        if src.len() != dst.len() {
            return Err(StateError::Invalid("byte array length mismatch"));
        }
        dst.copy_from_slice(src);
        Ok(())
    }
    pub fn u16s_into(&mut self, dst: &mut [u16]) -> StateResult<()> {
        let n = self.u32()? as usize;
        if n != dst.len() {
            return Err(StateError::Invalid("u16 array length mismatch"));
        }
        for d in dst.iter_mut() {
            *d = self.u16()?;
        }
        Ok(())
    }
    pub fn u32s_into(&mut self, dst: &mut [u32]) -> StateResult<()> {
        let n = self.u32()? as usize;
        if n != dst.len() {
            return Err(StateError::Invalid("u32 array length mismatch"));
        }
        for d in dst.iter_mut() {
            *d = self.u32()?;
        }
        Ok(())
    }

    /// Enter the next section, which must carry `tag`. Returns a reader
    /// limited to that section's body.
    pub fn expect_section(&mut self, tag: [u8; 4]) -> StateResult<StateReader<'a>> {
        let found: [u8; 4] = self.take(4)?.try_into().unwrap();
        if found != tag {
            return Err(StateError::MissingSection(tag));
        }
        let len = self.u32()? as usize;
        Ok(StateReader::new(self.take(len)?))
    }

    /// Load `value` from the next section, which must carry `tag`.
    pub fn section<T: SaveState + ?Sized>(&mut self, tag: [u8; 4], value: &mut T) -> StateResult<()> {
        let mut sub = self.expect_section(tag)?;
        value.load_state(&mut sub)
    }
}

/// CRC-32 (IEEE 802.3, reflected, poly 0xEDB88320).
pub fn crc32(data: &[u8]) -> u32 {
    const TABLE: [u32; 256] = {
        let mut t = [0u32; 256];
        let mut i = 0;
        while i < 256 {
            let mut c = i as u32;
            let mut k = 0;
            while k < 8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
                k += 1;
            }
            t[i] = c;
            i += 1;
        }
        t
    };
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc = TABLE[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Default, PartialEq)]
    struct Regs {
        r: [u32; 16],
        flag: bool,
        ram: Vec<u8>,
    }

    impl SaveState for Regs {
        fn save_state(&self, w: &mut StateWriter) {
            w.u32s(&self.r);
            w.bool(self.flag);
            w.bytes(&self.ram);
        }
        fn load_state(&mut self, r: &mut StateReader<'_>) -> StateResult<()> {
            r.u32s_into(&mut self.r)?;
            self.flag = r.bool()?;
            self.ram = r.bytes()?.to_vec();
            Ok(())
        }
    }

    #[test]
    fn crc32_known_vector() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn round_trip_with_sections() {
        let mut a = Regs::default();
        for (i, r) in a.r.iter_mut().enumerate() {
            *r = (i as u32) * 0x1111_1111;
        }
        a.flag = true;
        a.ram = (0..=255).collect();

        let mut w = StateWriter::new();
        w.section(*b"CPU0", &a);
        w.begin_section(*b"MISC");
        w.u64(0xDEAD_BEEF_CAFE_F00D);
        w.f32(1.5);
        w.end_section();
        let file = w.finish(3, *b"GAFE01");

        let (hdr, mut r) = StateReader::open(&file, 3).unwrap();
        assert_eq!(&hdr.game_id, b"GAFE01");
        let mut b = Regs::default();
        r.section(*b"CPU0", &mut b).unwrap();
        assert_eq!(a, b);
        let mut misc = r.expect_section(*b"MISC").unwrap();
        assert_eq!(misc.u64().unwrap(), 0xDEAD_BEEF_CAFE_F00D);
        assert_eq!(misc.f32().unwrap(), 1.5);
        assert_eq!(r.remaining(), 0);
    }

    #[test]
    fn detects_corruption_and_wrong_system() {
        let mut w = StateWriter::new();
        w.u32(42);
        let mut file = w.finish(1, *b"ADME\0\0");
        assert_eq!(StateReader::open(&file, 2).unwrap_err(), StateError::WrongSystem { expected: 2, found: 1 });
        let last = file.len() - 1;
        file[last] ^= 0xFF;
        assert_eq!(StateReader::open(&file, 1).unwrap_err(), StateError::ChecksumMismatch);
        assert_eq!(StateReader::open(&file[..10], 1).unwrap_err(), StateError::Truncated);
    }

    #[test]
    fn wrong_section_tag_is_reported() {
        let mut w = StateWriter::new();
        w.begin_section(*b"AAAA");
        w.end_section();
        let payload = w.into_payload();
        let mut r = StateReader::new(&payload);
        assert_eq!(r.expect_section(*b"BBBB").unwrap_err(), StateError::MissingSection(*b"BBBB"));
    }
}
