//! Game Pak: ROM, header, backup media (SRAM, Flash 64K/128K, EEPROM
//! 512B/8K), GPIO port (S-3511 RTC and Boktai solar sensor) and the
//! ADXL202E tilt sensor of Yoshi Topsy-Turvy / Koro Koro Puzzle.

use kd_core::clock::{to_bcd, GuestClock};
use kd_core::{SaveState, StateReader, StateResult, StateWriter};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupKind {
    None,
    Sram,
    Flash64,
    Flash128,
    Eeprom,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub title: String,
    pub game_code: [u8; 4],
    pub maker: [u8; 2],
    pub version: u8,
    pub checksum_ok: bool,
}

impl Header {
    pub fn parse(rom: &[u8]) -> Self {
        let get = |r: std::ops::Range<usize>| rom.get(r).unwrap_or(&[]).to_vec();
        let title = String::from_utf8_lossy(&get(0xA0..0xAC)).trim_end_matches('\0').trim().to_string();
        let mut game_code = [0u8; 4];
        game_code.copy_from_slice(&get(0xAC..0xB0).iter().copied().chain(std::iter::repeat(0)).take(4).collect::<Vec<_>>());
        let mut maker = [0u8; 2];
        maker.copy_from_slice(&get(0xB0..0xB2).iter().copied().chain(std::iter::repeat(0)).take(2).collect::<Vec<_>>());
        let checksum_ok = rom.len() >= 0xC0 && {
            let sum = rom[0xA0..0xBD].iter().fold(0u8, |acc, &b| acc.wrapping_sub(b));
            sum.wrapping_sub(0x19) == rom[0xBD]
        };
        Self { title, game_code, maker, version: rom.get(0xBC).copied().unwrap_or(0), checksum_ok }
    }

    /// Compute the complement check byte for a header (for building test ROMs).
    pub fn complement(rom: &[u8]) -> u8 {
        rom[0xA0..0xBD].iter().fold(0u8, |acc, &b| acc.wrapping_sub(b)).wrapping_sub(0x19)
    }
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Detect backup type from the SDK library ID strings embedded in the ROM.
pub fn detect_backup(rom: &[u8]) -> BackupKind {
    if contains(rom, b"EEPROM_V") {
        BackupKind::Eeprom
    } else if contains(rom, b"FLASH1M_V") {
        BackupKind::Flash128
    } else if contains(rom, b"FLASH_V") || contains(rom, b"FLASH512_V") {
        BackupKind::Flash64
    } else if contains(rom, b"SRAM_V") || contains(rom, b"SRAM_F_V") {
        BackupKind::Sram
    } else {
        BackupKind::None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlashState {
    Ready,
    Cmd1,
    Cmd2,
    Write,
    BankSelect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EepromState {
    Idle,
    Command { bits: u32, count: u32 },
    Address { read: bool, addr: u32, count: u32 },
    WriteData { addr: u32, data: u64, count: u32 },
    WriteStop,
    ReadStop { addr: u32 },
    Reading { data: u64, count: u32 },
}

/// The backup chip.
pub struct Backup {
    pub kind: BackupKind,
    pub data: Vec<u8>,
    pub dirty: bool,
    flash_state: FlashState,
    flash_id_mode: bool,
    flash_erase_armed: bool,
    flash_bank: usize,
    eeprom_state: EepromState,
    /// 6 (512 B) or 14 (8 KiB) address bits; 0 = not yet known.
    pub eeprom_addr_bits: u32,
}

impl Backup {
    pub fn new(kind: BackupKind) -> Self {
        let size = match kind {
            BackupKind::None => 0,
            BackupKind::Sram => 0x8000,
            BackupKind::Flash64 => 0x10000,
            BackupKind::Flash128 => 0x20000,
            BackupKind::Eeprom => 0x2000,
        };
        let fill = if kind == BackupKind::Sram { 0 } else { 0xFF };
        Self {
            kind,
            data: vec![fill; size],
            dirty: false,
            flash_state: FlashState::Ready,
            flash_id_mode: false,
            flash_erase_armed: false,
            flash_bank: 0,
            eeprom_state: EepromState::Idle,
            eeprom_addr_bits: 0,
        }
    }

    /// 0x0E000000 region read (8-bit bus).
    pub fn read8(&self, addr: u32) -> u8 {
        let a = addr as usize & 0xFFFF;
        match self.kind {
            BackupKind::Sram => self.data[a & 0x7FFF],
            BackupKind::Flash64 | BackupKind::Flash128 => {
                if self.flash_id_mode && a < 2 {
                    let id: [u8; 2] = if self.kind == BackupKind::Flash128 { [0x62, 0x13] } else { [0x32, 0x1B] };
                    id[a]
                } else {
                    self.data[self.flash_bank * 0x10000 + a]
                }
            }
            _ => 0xFF,
        }
    }

    pub fn write8(&mut self, addr: u32, value: u8) {
        let a = addr as usize & 0xFFFF;
        match self.kind {
            BackupKind::Sram => {
                self.data[a & 0x7FFF] = value;
                self.dirty = true;
            }
            BackupKind::Flash64 | BackupKind::Flash128 => self.flash_write(a, value),
            _ => {}
        }
    }

    fn flash_write(&mut self, a: usize, value: u8) {
        match self.flash_state {
            FlashState::Write => {
                let i = self.flash_bank * 0x10000 + a;
                self.data[i] &= value;
                self.dirty = true;
                self.flash_state = FlashState::Ready;
            }
            FlashState::BankSelect => {
                if a == 0 && self.kind == BackupKind::Flash128 {
                    self.flash_bank = (value & 1) as usize;
                }
                self.flash_state = FlashState::Ready;
            }
            FlashState::Ready if a == 0x5555 && value == 0xAA => self.flash_state = FlashState::Cmd1,
            FlashState::Cmd1 if a == 0x2AAA && value == 0x55 => self.flash_state = FlashState::Cmd2,
            FlashState::Cmd2 => {
                self.flash_state = FlashState::Ready;
                if self.flash_erase_armed {
                    self.flash_erase_armed = false;
                    if a == 0x5555 && value == 0x10 {
                        self.data.fill(0xFF);
                        self.dirty = true;
                    } else if value == 0x30 {
                        let start = self.flash_bank * 0x10000 + (a & 0xF000);
                        self.data[start..start + 0x1000].fill(0xFF);
                        self.dirty = true;
                    }
                    return;
                }
                if a != 0x5555 {
                    return;
                }
                match value {
                    0x90 => self.flash_id_mode = true,
                    0xF0 => self.flash_id_mode = false,
                    0x80 => self.flash_erase_armed = true,
                    0xA0 => self.flash_state = FlashState::Write,
                    0xB0 => self.flash_state = FlashState::BankSelect,
                    _ => tracing::warn!("unknown flash command {value:#04x}"),
                }
            }
            _ => self.flash_state = FlashState::Ready,
        }
    }

    /// EEPROM serial write (one bit per halfword, usually via DMA3).
    /// `dma_len` is the transfer length, used to infer the chip size.
    pub fn eeprom_write_bit(&mut self, bit: u16, dma_len: Option<u32>) {
        if self.eeprom_addr_bits == 0 {
            self.eeprom_addr_bits = match dma_len {
                Some(9) | Some(73) => 6,
                Some(17) | Some(81) => 14,
                _ => 0,
            };
        }
        let bits = if self.eeprom_addr_bits == 0 { 14 } else { self.eeprom_addr_bits };
        let b = (bit & 1) as u32;
        self.eeprom_state = match self.eeprom_state {
            EepromState::ReadStop { addr } => {
                // Stop bit of a read request: latch the 64-bit block.
                let off = addr as usize * 8;
                let mut buf = [0xFFu8; 8];
                if off + 8 <= self.data.len() {
                    buf.copy_from_slice(&self.data[off..off + 8]);
                }
                EepromState::Reading { data: u64::from_be_bytes(buf), count: 0 }
            }
            EepromState::WriteStop => EepromState::Idle,
            EepromState::Idle | EepromState::Reading { .. } => {
                if b == 1 {
                    EepromState::Command { bits: 1, count: 1 }
                } else {
                    EepromState::Idle
                }
            }
            EepromState::Command { bits: c, count } => {
                let c = (c << 1) | b;
                if count + 1 == 2 {
                    EepromState::Address { read: c == 0b11, addr: 0, count: 0 }
                } else {
                    EepromState::Command { bits: c, count: count + 1 }
                }
            }
            EepromState::Address { read, addr, count } => {
                let addr = (addr << 1) | b;
                if count + 1 == bits {
                    let addr = addr & 0x3FF;
                    if read {
                        EepromState::ReadStop { addr }
                    } else {
                        EepromState::WriteData { addr, data: 0, count: 0 }
                    }
                } else {
                    EepromState::Address { read, addr, count: count + 1 }
                }
            }
            EepromState::WriteData { addr, data, count } => {
                let data = (data << 1) | b as u64;
                if count + 1 == 64 {
                    let off = addr as usize * 8;
                    if off + 8 <= self.data.len() {
                        self.data[off..off + 8].copy_from_slice(&data.to_be_bytes());
                        self.dirty = true;
                    }
                    EepromState::WriteStop
                } else {
                    EepromState::WriteData { addr, data, count: count + 1 }
                }
            }
        };
    }

    /// EEPROM serial read: 4 junk bits then 64 data bits, MSB first.
    pub fn eeprom_read_bit(&mut self) -> u16 {
        match self.eeprom_state {
            EepromState::Reading { data, count } => {
                let out = if count < 4 { 0 } else { ((data >> (63 - (count - 4))) & 1) as u16 };
                self.eeprom_state = if count + 1 == 68 {
                    EepromState::Idle
                } else {
                    EepromState::Reading { data, count: count + 1 }
                };
                out
            }
            _ => 1, // ready
        }
    }
}

/// GPIO-attached peripherals on the cartridge bus (0x080000C4..0x080000C9).
#[derive(Debug, Clone)]
pub struct Gpio {
    pub has_rtc: bool,
    pub has_solar: bool,
    data: u8,
    direction: u8,
    pub readable: bool,
    rtc: Rtc,
    solar_counter: u8,
    /// Ambient light 0..=255, fed from the Android light sensor.
    pub light: u8,
}

#[derive(Debug, Clone, Default)]
struct Rtc {
    active: bool,
    command: Option<u8>,
    shift: u8,
    bit_count: u8,
    out_bytes: Vec<u8>,
    out_pos: usize,
    in_bytes_expected: usize,
    in_bytes: Vec<u8>,
    control: u8,
    sck: bool,
    out_bit: u8,
}

impl Gpio {
    pub fn new(has_rtc: bool, has_solar: bool) -> Self {
        Self {
            has_rtc,
            has_solar,
            data: 0,
            direction: 0,
            readable: false,
            rtc: Rtc { control: 0x40, ..Default::default() },
            solar_counter: 0,
            light: 0x80,
        }
    }

    pub fn present(&self) -> bool {
        self.has_rtc || self.has_solar
    }

    pub fn read(&self, offset: u32) -> Option<u16> {
        if !self.readable {
            return None;
        }
        Some(match offset {
            0xC4 => {
                let mut v = self.data & self.direction;
                if self.has_rtc && self.direction & 2 == 0 {
                    v |= self.rtc.out_bit << 1;
                }
                if self.has_solar && self.direction & 8 == 0 {
                    // Flag goes high once the counter passes the light level.
                    let threshold = 0xFF - self.light;
                    if self.solar_counter >= threshold {
                        v |= 8;
                    }
                }
                v as u16
            }
            0xC6 => self.direction as u16,
            0xC8 => self.readable as u16,
            _ => 0,
        })
    }

    pub fn write(&mut self, offset: u32, value: u16, clock: &GuestClock) {
        match offset {
            0xC4 => {
                self.data = (value as u8) & 0xF;
                if self.has_rtc {
                    self.rtc_pins(clock);
                }
                if self.has_solar {
                    if self.data & 2 != 0 {
                        self.solar_counter = 0;
                    } else if self.data & 1 != 0 {
                        self.solar_counter = self.solar_counter.saturating_add(1);
                    }
                }
            }
            0xC6 => self.direction = (value as u8) & 0xF,
            0xC8 => self.readable = value & 1 != 0,
            _ => {}
        }
    }

    /// S-3511 serial protocol: pins SCK=bit0, SIO=bit1, CS=bit2. Bits are
    /// shifted LSB first on SCK rising edges while CS is high.
    fn rtc_pins(&mut self, clock: &GuestClock) {
        let sck = self.data & 1 != 0;
        let sio = (self.data >> 1) & 1;
        let cs = self.data & 4 != 0;
        let r = &mut self.rtc;
        if !cs {
            *r = Rtc { control: r.control, ..Default::default() };
            r.sck = sck;
            return;
        }
        if !r.active {
            r.active = true;
            r.sck = sck;
            return;
        }
        let rising = sck && !r.sck;
        r.sck = sck;
        if !rising {
            return;
        }
        if r.out_pos < r.out_bytes.len() {
            let byte = r.out_bytes[r.out_pos];
            r.out_bit = (byte >> r.bit_count) & 1;
            r.bit_count += 1;
            if r.bit_count == 8 {
                r.bit_count = 0;
                r.out_pos += 1;
            }
            return;
        }
        r.shift |= sio << r.bit_count;
        r.bit_count += 1;
        if r.bit_count < 8 {
            return;
        }
        let byte = r.shift;
        r.shift = 0;
        r.bit_count = 0;
        match r.command {
            None => {
                // Some games send the command MSB first; normalise.
                let cmd = if byte & 0x0F == 0x06 { byte } else { byte.reverse_bits() };
                if cmd & 0x0F != 0x06 {
                    return;
                }
                let reg = (cmd >> 4) & 7;
                let reading = cmd & 0x80 != 0;
                r.command = Some(reg);
                if reading {
                    let now = clock.now();
                    let pm = if r.control & 0x40 == 0 && now.hour >= 12 { 0x80 } else { 0 };
                    let hour = if r.control & 0x40 != 0 { now.hour } else { now.hour % 12 };
                    let time = [to_bcd(hour) | pm, to_bcd(now.minute), to_bcd(now.second)];
                    r.out_bytes = match reg {
                        2 => vec![
                            to_bcd((now.year.rem_euclid(100)) as u8),
                            to_bcd(now.month),
                            to_bcd(now.day),
                            now.weekday,
                            time[0],
                            time[1],
                            time[2],
                        ],
                        4 => vec![r.control],
                        6 => time.to_vec(),
                        _ => vec![],
                    };
                    r.out_pos = 0;
                } else {
                    r.in_bytes_expected = match reg {
                        0 => {
                            r.control = 0;
                            0
                        }
                        4 => 1,
                        2 => 7,
                        6 => 3,
                        _ => 0,
                    };
                }
            }
            Some(reg) => {
                r.in_bytes.push(byte);
                if r.in_bytes.len() == r.in_bytes_expected {
                    if reg == 4 {
                        r.control = byte;
                    }
                    // Writes to the date/time registers are accepted but the
                    // clock is owned by the time-travel GuestClock.
                    r.in_bytes.clear();
                }
            }
        }
    }
}

/// Two-axis accelerometer at 0x0E008000.
#[derive(Debug, Clone, Default)]
pub struct Tilt {
    pub enabled: bool,
    latch_stage: u8,
    x: u16,
    y: u16,
    /// Latest device tilt in g, set by the front-end.
    pub input: (f32, f32),
}

impl Tilt {
    pub fn write(&mut self, addr: u32, value: u8) {
        match (addr & 0xFFFF, value) {
            (0x8000, 0x55) => self.latch_stage = 1,
            (0x8100, 0xAA) if self.latch_stage == 1 => {
                // ~0x3A0 at rest, roughly ±0x100 per g.
                self.x = (0x3A0 as f32 - self.input.0 * 256.0).clamp(0.0, 0xFFF as f32) as u16;
                self.y = (0x3A0 as f32 + self.input.1 * 256.0).clamp(0.0, 0xFFF as f32) as u16;
                self.latch_stage = 2;
            }
            _ => {}
        }
    }

    pub fn read(&self, addr: u32) -> u8 {
        match addr & 0xFFFF {
            0x8200 => self.x as u8,
            0x8300 => ((self.x >> 8) as u8 & 0xF) | 0x80,
            0x8400 => self.y as u8,
            0x8500 => (self.y >> 8) as u8 & 0xF,
            _ => 0,
        }
    }
}

/// Game codes (first three letters) of carts with special hardware.
const RTC_GAMES: &[&[u8; 3]] = &[b"AXV", b"AXP", b"BPE", b"U3I", b"U32", b"U33", b"BR4"];
const SOLAR_GAMES: &[&[u8; 3]] = &[b"U3I", b"U32", b"U33"];
const TILT_GAMES: &[&[u8; 3]] = &[b"KYG", b"KHP"];

pub struct Cartridge {
    pub rom: Vec<u8>,
    pub header: Header,
    pub backup: Backup,
    pub gpio: Gpio,
    pub tilt: Tilt,
}

impl Cartridge {
    pub fn new(mut rom: Vec<u8>) -> Self {
        let header = Header::parse(&rom);
        let backup = Backup::new(detect_backup(&rom));
        let code3: [u8; 3] = [header.game_code[0], header.game_code[1], header.game_code[2]];
        let has_rtc = RTC_GAMES.iter().any(|c| **c == code3) || contains(&rom, b"SIIRTC_V");
        let has_solar = SOLAR_GAMES.iter().any(|c| **c == code3);
        let tilt = Tilt { enabled: TILT_GAMES.iter().any(|c| **c == code3), ..Default::default() };
        // Pad to a power of two so mirroring is a mask.
        let size = rom.len().max(0x100).next_power_of_two().min(0x0200_0000);
        rom.resize(size, 0);
        Self { rom, header, backup, gpio: Gpio::new(has_rtc, has_solar), tilt }
    }

    #[inline]
    pub fn rom_read16(&self, addr: u32) -> u16 {
        let a = (addr as usize & 0x01FF_FFFE) & (self.rom.len() - 1);
        u16::from_le_bytes([self.rom[a], self.rom[a + 1]])
    }

    /// Is `addr` (0x0D region) the EEPROM?
    pub fn is_eeprom_addr(&self, addr: u32) -> bool {
        self.backup.kind == BackupKind::Eeprom
            && (addr & 0x0F00_0000) == 0x0D00_0000
            && (self.rom.len() <= 0x0100_0000 || addr & 0x00FF_FF00 == 0x00FF_FF00)
    }
}

impl SaveState for Cartridge {
    fn save_state(&self, w: &mut StateWriter) {
        w.bytes(&self.backup.data);
        w.u8(self.backup.flash_bank as u8);
        w.bool(self.backup.flash_id_mode);
        w.u32(self.backup.eeprom_addr_bits);
        w.u8(self.gpio.data);
        w.u8(self.gpio.direction);
        w.bool(self.gpio.readable);
        w.u8(self.gpio.rtc.control);
    }
    fn load_state(&mut self, r: &mut StateReader<'_>) -> StateResult<()> {
        let data = r.bytes()?;
        if data.len() == self.backup.data.len() {
            self.backup.data.copy_from_slice(data);
        }
        self.backup.flash_bank = r.u8()? as usize;
        self.backup.flash_id_mode = r.bool()?;
        self.backup.eeprom_addr_bits = r.u32()?;
        self.backup.flash_state = FlashState::Ready;
        self.backup.eeprom_state = EepromState::Idle;
        self.gpio.data = r.u8()?;
        self.gpio.direction = r.u8()?;
        self.gpio.readable = r.bool()?;
        self.gpio.rtc.control = r.u8()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_backup_strings() {
        let mut rom = vec![0u8; 0x400];
        rom[0x200..0x20A].copy_from_slice(b"FLASH1M_V1");
        assert_eq!(detect_backup(&rom), BackupKind::Flash128);
        rom[0x200..0x20A].copy_from_slice(b"EEPROM_V12");
        assert_eq!(detect_backup(&rom), BackupKind::Eeprom);
        assert_eq!(detect_backup(&[0; 64]), BackupKind::None);
    }

    #[test]
    fn flash_program_erase_and_id() {
        let mut b = Backup::new(BackupKind::Flash128);
        let cmd = |b: &mut Backup, c: u8| {
            b.write8(0x5555, 0xAA);
            b.write8(0x2AAA, 0x55);
            b.write8(0x5555, c);
        };
        cmd(&mut b, 0x90);
        assert_eq!((b.read8(0), b.read8(1)), (0x62, 0x13));
        cmd(&mut b, 0xF0);
        cmd(&mut b, 0xA0);
        b.write8(0x1234, 0x5A);
        assert_eq!(b.read8(0x1234), 0x5A);
        // bank 1
        cmd(&mut b, 0xB0);
        b.write8(0, 1);
        cmd(&mut b, 0xA0);
        b.write8(0x10, 0x77);
        assert_eq!(b.data[0x10010], 0x77);
        // sector erase in bank 0
        cmd(&mut b, 0xB0);
        b.write8(0, 0);
        cmd(&mut b, 0x80);
        b.write8(0x5555, 0xAA);
        b.write8(0x2AAA, 0x55);
        b.write8(0x1000, 0x30);
        assert_eq!(b.read8(0x1234), 0xFF);
        assert_eq!(b.data[0x10010], 0x77);
    }

    fn send_bits(b: &mut Backup, bits: &[u16], len: u32) {
        for &bit in bits {
            b.eeprom_write_bit(bit, Some(len));
        }
    }

    #[test]
    fn eeprom_write_then_read_512b() {
        let mut b = Backup::new(BackupKind::Eeprom);
        let value: u64 = 0x0123_4567_89AB_CDEF;
        let mut bits = vec![1, 0];
        bits.extend((0..6).rev().map(|i| (5 >> i) & 1)); // address 5
        bits.extend((0..64).rev().map(|i| ((value >> i) & 1) as u16));
        bits.push(0);
        send_bits(&mut b, &bits, 73);
        assert_eq!(b.eeprom_addr_bits, 6);
        assert_eq!(&b.data[40..48], &value.to_be_bytes());

        let mut req = vec![1, 1];
        req.extend((0..6).rev().map(|i| (5 >> i) & 1));
        req.push(0);
        send_bits(&mut b, &req, 9);
        let mut out = 0u64;
        for i in 0..68 {
            let bit = b.eeprom_read_bit();
            if i >= 4 {
                out = (out << 1) | bit as u64;
            }
        }
        assert_eq!(out, value);
    }

    #[test]
    fn header_parsing() {
        let mut rom = vec![0u8; 0x200];
        rom[0xA0..0xA8].copy_from_slice(b"KDTEST\0\0");
        rom[0xAC..0xB0].copy_from_slice(b"AKDE");
        rom[0xB0..0xB2].copy_from_slice(b"01");
        rom[0xB2] = 0x96;
        rom[0xBD] = Header::complement(&rom);
        let h = Header::parse(&rom);
        assert_eq!(h.title, "KDTEST");
        assert_eq!(&h.game_code, b"AKDE");
        assert!(h.checksum_ok);
    }

    #[test]
    fn rtc_reads_guest_clock() {
        use kd_core::clock::{DateTime, EPOCH_2000_UNIX};
        let mut clock = GuestClock::fixed(EPOCH_2000_UNIX);
        clock.travel_to(DateTime { year: 2004, month: 7, day: 9, hour: 13, minute: 45, second: 30, weekday: 5 });
        let mut g = Gpio::new(true, false);
        g.write(0xC8, 1, &clock);
        g.write(0xC6, 0b0111, &clock); // all outputs
        g.write(0xC4, 0b001, &clock); // SCK high, CS low
        g.write(0xC4, 0b101, &clock); // CS high
        let cmd: u8 = 0x06 | (6 << 4) | 0x80; // read time, LSB first
        for i in 0..8 {
            let bit = ((cmd >> i) & 1) as u16;
            g.write(0xC4, 0b100 | bit << 1, &clock);
            g.write(0xC4, 0b101 | bit << 1, &clock);
        }
        g.write(0xC6, 0b0101, &clock); // SIO becomes input
        let mut bytes = [0u8; 3];
        for byte in &mut bytes {
            for i in 0..8 {
                g.write(0xC4, 0b100, &clock);
                g.write(0xC4, 0b101, &clock);
                *byte |= (((g.read(0xC4).unwrap() >> 1) & 1) as u8) << i;
            }
        }
        // 24-hour mode is the default control value used here.
        assert_eq!(bytes, [0x13, 0x45, 0x30]);
    }

    #[test]
    fn tilt_sensor_latches() {
        let mut t = Tilt { enabled: true, input: (0.0, 0.5), ..Default::default() };
        t.write(0x0E00_8000, 0x55);
        t.write(0x0E00_8100, 0xAA);
        let x = t.read(0x8200) as u16 | ((t.read(0x8300) as u16 & 0xF) << 8);
        let y = t.read(0x8400) as u16 | ((t.read(0x8500) as u16) << 8);
        assert_eq!(x, 0x3A0);
        assert_eq!(y, 0x3A0 + 128);
        assert_ne!(t.read(0x8300) & 0x80, 0);
    }
}
