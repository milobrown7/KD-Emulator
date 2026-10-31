//! DS game card: header, the post-boot (KEY2) command protocol used after a
//! direct boot (`0xB7` data read, `0xB8` chip ID) and the AUXSPI save chip
//! (EEPROM / FLASH / FRAM).

use kd_core::{SaveState, StateReader, StateResult, StateWriter};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub title: String,
    pub game_code: [u8; 4],
    pub maker: [u8; 2],
    pub arm9_rom_offset: u32,
    pub arm9_entry: u32,
    pub arm9_ram_addr: u32,
    pub arm9_size: u32,
    pub arm7_rom_offset: u32,
    pub arm7_entry: u32,
    pub arm7_ram_addr: u32,
    pub arm7_size: u32,
    pub header_crc: u16,
}

fn le32(rom: &[u8], at: usize) -> u32 {
    rom.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).unwrap_or(0)
}

impl Header {
    pub fn parse(rom: &[u8]) -> Self {
        let mut game_code = [0u8; 4];
        let mut maker = [0u8; 2];
        if rom.len() >= 0x12 {
            game_code.copy_from_slice(&rom[0x0C..0x10]);
            maker.copy_from_slice(&rom[0x10..0x12]);
        }
        Self {
            title: String::from_utf8_lossy(rom.get(0..12).unwrap_or(&[])).trim_end_matches('\0').to_string(),
            game_code,
            maker,
            arm9_rom_offset: le32(rom, 0x20),
            arm9_entry: le32(rom, 0x24),
            arm9_ram_addr: le32(rom, 0x28),
            arm9_size: le32(rom, 0x2C),
            arm7_rom_offset: le32(rom, 0x30),
            arm7_entry: le32(rom, 0x34),
            arm7_ram_addr: le32(rom, 0x38),
            arm7_size: le32(rom, 0x3C),
            header_crc: rom.get(0x15E..0x160).map(|b| u16::from_le_bytes([b[0], b[1]])).unwrap_or(0),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveKind {
    None,
    /// 512 B EEPROM: 1 address byte, bit 3 of the command is A8.
    Eeprom512,
    /// 8 KiB / 64 KiB EEPROM or FRAM: 2 address bytes.
    Eeprom(usize),
    /// 256 KiB+ FLASH: 3 address bytes.
    Flash(usize),
}

impl SaveKind {
    pub fn size(self) -> usize {
        match self {
            SaveKind::None => 0,
            SaveKind::Eeprom512 => 512,
            SaveKind::Eeprom(n) | SaveKind::Flash(n) => n,
        }
    }

    fn addr_bytes(self) -> usize {
        match self {
            SaveKind::Eeprom512 => 1,
            SaveKind::Eeprom(_) => 2,
            _ => 3,
        }
    }

    /// Guess from a save file's size.
    pub fn from_size(n: usize) -> Self {
        match n {
            0 => SaveKind::None,
            1..=512 => SaveKind::Eeprom512,
            513..=0x10000 => SaveKind::Eeprom(n.next_power_of_two()),
            _ => SaveKind::Flash(n.next_power_of_two()),
        }
    }
}

/// Save chip hints for titles where auto-detection is not enough.
pub fn save_hint(game_code: &[u8; 4]) -> Option<SaveKind> {
    match &game_code[..3] {
        // Animal Crossing: Wild World / Oideyo Dōbutsu no Mori
        b"ADM" => Some(SaveKind::Flash(0x40000)),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub struct SaveChip {
    pub kind: SaveKind,
    pub data: Vec<u8>,
    pub dirty: bool,
    cmd: u8,
    addr: u32,
    addr_left: usize,
    write_enable: bool,
    active: bool,
}

impl SaveChip {
    pub fn new(kind: SaveKind) -> Self {
        Self {
            kind,
            data: vec![0xFF; kind.size()],
            dirty: false,
            cmd: 0,
            addr: 0,
            addr_left: 0,
            write_enable: false,
            active: false,
        }
    }

    pub fn release(&mut self) {
        self.active = false;
        if matches!(self.cmd, 0x02 | 0x0A | 0xDB | 0xD8) {
            self.write_enable = false;
        }
        self.cmd = 0;
    }

    pub fn transfer(&mut self, v: u8) -> u8 {
        if self.kind == SaveKind::None {
            return 0xFF;
        }
        if !self.active {
            self.active = true;
            self.cmd = v;
            self.addr = 0;
            self.addr_left = 0;
            match v {
                0x06 => self.write_enable = true,
                0x04 => self.write_enable = false,
                0x03 | 0x0B | 0x02 | 0x0A | 0xDB | 0xD8 => {
                    self.addr_left = self.kind.addr_bytes();
                    if self.kind == SaveKind::Eeprom512 && v & 0x08 != 0 {
                        self.addr = 0x100;
                    }
                }
                _ => {}
            }
            return 0xFF;
        }
        if self.addr_left > 0 {
            self.addr = if self.kind == SaveKind::Eeprom512 {
                (self.addr & 0x100) | v as u32
            } else {
                (self.addr << 8) | v as u32
            };
            self.addr_left -= 1;
            if self.addr_left == 0 && matches!(self.cmd, 0xDB | 0xD8) && self.write_enable {
                let size = if self.cmd == 0xDB { 0x100 } else { 0x10000 };
                let start = (self.addr as usize & !(size - 1)) % self.data.len();
                let end = (start + size).min(self.data.len());
                self.data[start..end].fill(0xFF);
                self.dirty = true;
            }
            return 0xFF;
        }
        let len = self.data.len();
        match self.cmd {
            0x05 => (self.write_enable as u8) << 1,
            0x9F => 0xFF,
            0x03 | 0x0B => {
                let b = self.data[self.addr as usize % len];
                self.addr += 1;
                b
            }
            0x02 | 0x0A if self.write_enable => {
                self.data[self.addr as usize % len] = v;
                self.addr += 1;
                self.dirty = true;
                0xFF
            }
            _ => 0xFF,
        }
    }
}

pub struct GameCard {
    pub rom: Vec<u8>,
    pub header: Header,
    pub romctrl: u32,
    pub command: [u8; 8],
    buffer: Vec<u8>,
    pos: usize,
    pub auxspicnt: u16,
    aux_data: u8,
    pub save: SaveChip,
    pub chip_id: u32,
}

impl GameCard {
    pub fn new(rom: Vec<u8>) -> Self {
        let header = Header::parse(&rom);
        let size_mb = (rom.len().max(1) as u32).div_ceil(1 << 20).next_power_of_two();
        let chip_id = 0xC2 | (size_mb.saturating_sub(1) & 0xFF) << 8;
        let kind = save_hint(&header.game_code).unwrap_or(SaveKind::Flash(0x80000));
        Self {
            rom,
            header,
            romctrl: 0,
            command: [0; 8],
            buffer: Vec::new(),
            pos: 0,
            auxspicnt: 0,
            aux_data: 0,
            save: SaveChip::new(kind),
            chip_id,
        }
    }

    /// Replace the save chip to match an existing save file.
    pub fn load_save(&mut self, data: &[u8]) {
        if save_hint(&self.header.game_code).is_none() && data.len() != self.save.data.len() {
            self.save = SaveChip::new(SaveKind::from_size(data.len()));
        }
        let n = self.save.data.len().min(data.len());
        self.save.data[..n].copy_from_slice(&data[..n]);
    }

    pub fn data_ready(&self) -> bool {
        self.romctrl & (1 << 23) != 0
    }

    /// ROMCTRL write; returns true when a transfer finished immediately
    /// (zero-length) so the caller can raise the IRQ.
    pub fn write_romctrl(&mut self, v: u32) -> bool {
        let start = v & (1 << 31) != 0 && self.romctrl & (1 << 31) == 0;
        self.romctrl = (self.romctrl & (1 << 23)) | (v & !(1 << 23));
        if !start {
            return false;
        }
        let size = match (v >> 24) & 7 {
            0 => 0,
            7 => 4,
            n => 0x100 << n,
        };
        self.buffer.clear();
        self.pos = 0;
        match self.command[0] {
            0xB7 => {
                let mut addr = u32::from_be_bytes([self.command[1], self.command[2], self.command[3], self.command[4]]);
                if addr < 0x8000 {
                    addr = 0x8000 + (addr & 0x1FF);
                }
                for i in 0..size {
                    let a = (addr as usize + i) & (self.rom.len().next_power_of_two() - 1);
                    self.buffer.push(self.rom.get(a).copied().unwrap_or(0xFF));
                }
            }
            0xB8 | 0x90 | 0xB0 => {
                for _ in 0..size / 4 {
                    self.buffer.extend_from_slice(&self.chip_id.to_le_bytes());
                }
            }
            0x00 => {
                for i in 0..size {
                    self.buffer.push(self.rom.get(i & 0xFFF).copied().unwrap_or(0));
                }
            }
            _ => self.buffer.resize(size, 0xFF),
        }
        if self.buffer.is_empty() {
            self.romctrl &= !(1 << 31);
            return true;
        }
        self.romctrl |= 1 << 23;
        false
    }

    /// Read the next data word; returns (word, transfer_finished).
    pub fn read_data(&mut self) -> (u32, bool) {
        if !self.data_ready() {
            return (0xFFFF_FFFF, false);
        }
        let mut w = [0xFFu8; 4];
        for b in &mut w {
            if self.pos < self.buffer.len() {
                *b = self.buffer[self.pos];
                self.pos += 1;
            }
        }
        let done = self.pos >= self.buffer.len();
        if done {
            self.romctrl &= !((1 << 31) | (1 << 23));
        }
        (u32::from_le_bytes(w), done)
    }

    pub fn irq_enabled(&self) -> bool {
        self.auxspicnt & 0x4000 != 0
    }

    pub fn write_auxspicnt(&mut self, v: u16) {
        self.auxspicnt = v;
        if v & 0x2000 == 0 {
            self.save.release();
        }
    }

    pub fn read_auxspidata(&self) -> u8 {
        self.aux_data
    }

    pub fn write_auxspidata(&mut self, v: u8) {
        if self.auxspicnt & 0x8000 == 0 || self.auxspicnt & 0x2000 == 0 {
            return;
        }
        self.aux_data = self.save.transfer(v);
        if self.auxspicnt & 0x40 == 0 {
            self.save.release();
        }
    }
}

impl SaveState for GameCard {
    fn save_state(&self, w: &mut StateWriter) {
        w.u32(self.romctrl);
        w.bytes(&self.command);
        w.bytes(&self.buffer);
        w.u32(self.pos as u32);
        w.u16(self.auxspicnt);
        w.bytes(&self.save.data);
    }
    fn load_state(&mut self, r: &mut StateReader<'_>) -> StateResult<()> {
        self.romctrl = r.u32()?;
        r.bytes_into(&mut self.command)?;
        self.buffer = r.bytes()?.to_vec();
        self.pos = r.u32()? as usize;
        self.auxspicnt = r.u16()?;
        let data = r.bytes()?;
        if data.len() == self.save.data.len() {
            self.save.data.copy_from_slice(data);
        }
        self.save.release();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rom() -> Vec<u8> {
        let mut r = vec![0u8; 0x20000];
        r[0x0C..0x10].copy_from_slice(b"ADME");
        for (i, b) in r[0x8000..].iter_mut().enumerate() {
            *b = i as u8;
        }
        r
    }

    #[test]
    fn b7_data_read() {
        let mut c = GameCard::new(rom());
        c.command = [0xB7, 0, 0, 0x90, 0x00, 0, 0, 0];
        assert!(!c.write_romctrl(0x8000_0000 | 1 << 24)); // 0x200 bytes
        assert!(c.data_ready());
        let (w, done) = c.read_data();
        assert_eq!(w, u32::from_le_bytes([0x00, 0x01, 0x02, 0x03]));
        assert!(!done);
        for _ in 1..0x80 {
            c.read_data();
        }
        assert!(!c.data_ready());
        assert_eq!(c.romctrl & (1 << 31), 0);
    }

    #[test]
    fn animal_crossing_uses_flash_hint() {
        let c = GameCard::new(rom());
        assert_eq!(c.save.kind, SaveKind::Flash(0x40000));
    }

    #[test]
    fn flash_write_read_cycle() {
        let mut c = GameCard::new(rom());
        let xfer = |c: &mut GameCard, bytes: &[u8]| -> Vec<u8> {
            let mut out = vec![];
            for (i, &b) in bytes.iter().enumerate() {
                let hold = if i + 1 < bytes.len() { 0x40 } else { 0 };
                c.write_auxspicnt(0xA000 | hold);
                c.write_auxspidata(b);
                out.push(c.read_auxspidata());
            }
            out
        };
        xfer(&mut c, &[0x06]);
        xfer(&mut c, &[0x0A, 0x00, 0x01, 0x00, b'K', b'D']);
        let out = xfer(&mut c, &[0x03, 0x00, 0x01, 0x00, 0, 0]);
        assert_eq!(&out[4..], b"KD");
        assert!(c.save.dirty);
        let status = xfer(&mut c, &[0x05, 0]);
        assert_eq!(status[1] & 2, 0, "write enable cleared after program");
    }

    #[test]
    fn eeprom512_high_page_via_command_bit() {
        let mut s = SaveChip::new(SaveKind::Eeprom512);
        s.transfer(0x06);
        s.release();
        s.transfer(0x0A); // write, A8 = 1
        s.transfer(0x10);
        s.transfer(0x77);
        s.release();
        assert_eq!(s.data[0x110], 0x77);
    }
}
