//! ARM7 SPI bus: power management chip, firmware flash and the TSC2046
//! touchscreen controller.
//!
//! No Nintendo firmware dump is needed: [`build_firmware`] synthesises a
//! 256 KiB image with a valid header and user-settings block (nickname,
//! language, birthday and touchscreen calibration) so games that read it
//! behave as on a configured console.

use kd_core::{SaveState, StateReader, StateResult, StateWriter};

pub const FIRMWARE_SIZE: usize = 0x40000;
const USER_SETTINGS: usize = 0x3FE00;

/// CRC-16 used by the firmware user-settings block.
pub fn crc16(mut crc: u16, data: &[u8]) -> u16 {
    for &b in data {
        crc ^= b as u16;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xA001 } else { crc >> 1 };
        }
    }
    crc
}

/// Touchscreen calibration: ADC = screen * 16 (12-bit ADC over 256x192).
pub const fn screen_to_adc(v: u16) -> u16 {
    v * 16
}

/// User settings block as copied by the firmware to 0x027FFC80 (0x70 bytes).
pub fn user_settings(nickname: &str) -> [u8; 0x70] {
    let mut u = [0u8; 0x70];
    u[0x00] = 5; // version
    u[0x02] = 11; // favourite colour (green — Animal Crossing leaf)
    u[0x03] = 3; // birthday month
    u[0x04] = 14; // birthday day
    let name: Vec<u16> = nickname.encode_utf16().take(10).collect();
    for (i, c) in name.iter().enumerate() {
        u[0x06 + i * 2..0x08 + i * 2].copy_from_slice(&c.to_le_bytes());
    }
    u[0x1A] = name.len() as u8;
    // Touch calibration points (1,1) and (255,191).
    let (x1, y1, x2, y2) = (1u16, 1u16, 255u16, 191u16);
    u[0x58..0x5A].copy_from_slice(&screen_to_adc(x1).to_le_bytes());
    u[0x5A..0x5C].copy_from_slice(&screen_to_adc(y1).to_le_bytes());
    u[0x5C] = x1 as u8;
    u[0x5D] = y1 as u8;
    u[0x5E..0x60].copy_from_slice(&screen_to_adc(x2).to_le_bytes());
    u[0x60..0x62].copy_from_slice(&screen_to_adc(y2).to_le_bytes());
    u[0x62] = x2 as u8;
    u[0x63] = y2 as u8;
    u[0x64] = 0x01; // language: English
    u[0x65] = 0xFC; // flags: GBA screen, backlight
    u
}

pub fn build_firmware(nickname: &str) -> Vec<u8> {
    let mut fw = vec![0xFFu8; FIRMWARE_SIZE];
    fw[..0x200].fill(0);
    fw[0x08..0x0C].copy_from_slice(b"MACP"); // firmware identifier
    fw[0x20..0x22].copy_from_slice(&((USER_SETTINGS >> 3) as u16).to_le_bytes());
    fw[0x1D] = 0xFF; // console type: DS
    let us = user_settings(nickname);
    for copy in 0..2 {
        let base = USER_SETTINGS + copy * 0x100;
        fw[base..base + 0x70].copy_from_slice(&us);
        fw[base + 0x70..base + 0x72].copy_from_slice(&(copy as u16).to_le_bytes()); // update counter
        let crc = crc16(0xFFFF, &fw[base..base + 0x70]);
        fw[base + 0x72..base + 0x74].copy_from_slice(&crc.to_le_bytes());
        fw[base + 0x74..base + 0x100].fill(0);
    }
    fw
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlashCmd {
    None,
    Read { addr: u32, stage: u8 },
    Status,
    Id(u8),
}

pub struct Spi {
    pub cnt: u16,
    data: u8,
    pub firmware: Vec<u8>,
    flash_cmd: FlashCmd,
    write_enable: bool,
    /// Touch position in screen pixels, `None` when the pen is up.
    pub touch: Option<(u16, u16)>,
    tsc_out: u16,
    tsc_bytes: u8,
    pm_index: Option<u8>,
    pm_regs: [u8; 8],
}

impl Default for Spi {
    fn default() -> Self {
        Self::new()
    }
}

impl Spi {
    pub fn new() -> Self {
        Self {
            cnt: 0,
            data: 0,
            firmware: build_firmware("KD"),
            flash_cmd: FlashCmd::None,
            write_enable: false,
            touch: None,
            tsc_out: 0,
            tsc_bytes: 0,
            pm_index: None,
            pm_regs: [0, 0, 0, 0, 0x0F, 0, 0, 0],
        }
    }

    pub fn read_cnt(&self) -> u16 {
        self.cnt & !0x80 // transfers complete instantly
    }

    pub fn write_cnt(&mut self, v: u16) {
        let was_enabled = self.cnt & 0x8000 != 0;
        self.cnt = v;
        if !was_enabled || v & 0x8000 == 0 {
            self.end_transfer();
        }
    }

    fn end_transfer(&mut self) {
        self.flash_cmd = FlashCmd::None;
        self.pm_index = None;
        self.tsc_bytes = 0;
    }

    pub fn read_data(&self) -> u8 {
        self.data
    }

    /// Exchange one byte with the selected device.
    pub fn write_data(&mut self, v: u8) {
        if self.cnt & 0x8000 == 0 {
            return;
        }
        let device = (self.cnt >> 8) & 3;
        self.data = match device {
            0 => self.power(v),
            1 => self.flash(v),
            2 => self.touchscreen(v),
            _ => 0,
        };
        if self.cnt & 0x800 == 0 {
            // chip-select released after this byte
            self.end_transfer();
        }
    }

    fn power(&mut self, v: u8) -> u8 {
        match self.pm_index {
            None => {
                self.pm_index = Some(v);
                0
            }
            Some(cmd) => {
                let reg = (cmd & 7) as usize;
                if cmd & 0x80 != 0 {
                    self.pm_regs[reg]
                } else {
                    self.pm_regs[reg] = v;
                    0
                }
            }
        }
    }

    fn flash(&mut self, v: u8) -> u8 {
        match self.flash_cmd {
            FlashCmd::None => {
                match v {
                    0x03 => self.flash_cmd = FlashCmd::Read { addr: 0, stage: 0 },
                    0x05 => self.flash_cmd = FlashCmd::Status,
                    0x06 => self.write_enable = true,
                    0x04 => self.write_enable = false,
                    0x9F => self.flash_cmd = FlashCmd::Id(0),
                    _ => {}
                }
                0
            }
            FlashCmd::Read { addr, stage } if stage < 3 => {
                self.flash_cmd = FlashCmd::Read { addr: addr << 8 | v as u32, stage: stage + 1 };
                0
            }
            FlashCmd::Read { addr, stage } => {
                let b = self.firmware[addr as usize % FIRMWARE_SIZE];
                self.flash_cmd = FlashCmd::Read { addr: addr + 1, stage };
                b
            }
            FlashCmd::Status => (self.write_enable as u8) << 1,
            FlashCmd::Id(i) => {
                self.flash_cmd = FlashCmd::Id(i + 1);
                [0x20, 0x40, 0x12][(i as usize).min(2)]
            }
        }
    }

    /// TSC2046: a control byte selects the channel; the 12-bit result is
    /// shifted out over the next two bytes.
    fn touchscreen(&mut self, v: u8) -> u8 {
        let out = match self.tsc_bytes {
            1 => (self.tsc_out >> 5) as u8,
            2 => (self.tsc_out << 3) as u8,
            _ => 0,
        };
        if v & 0x80 != 0 {
            let channel = (v >> 4) & 7;
            self.tsc_out = match (channel, self.touch) {
                (5, Some((x, _))) => screen_to_adc(x),
                (1, Some((_, y))) => screen_to_adc(y),
                (5, None) | (1, None) => 0,
                (6, _) => 0x800, // AUX: microphone at rest
                _ => 0,
            };
            self.tsc_bytes = 1;
        } else if self.tsc_bytes > 0 {
            self.tsc_bytes += 1;
        }
        out
    }
}

impl SaveState for Spi {
    fn save_state(&self, w: &mut StateWriter) {
        w.u16(self.cnt);
        w.u8(self.data);
        w.bytes(&self.pm_regs);
    }
    fn load_state(&mut self, r: &mut StateReader<'_>) -> StateResult<()> {
        self.cnt = r.u16()?;
        self.data = r.u8()?;
        r.bytes_into(&mut self.pm_regs)?;
        self.end_transfer();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xfer(spi: &mut Spi, device: u16, bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for (i, &b) in bytes.iter().enumerate() {
            let hold = if i + 1 < bytes.len() { 0x800 } else { 0 };
            spi.write_cnt(0x8000 | device << 8 | hold);
            spi.write_data(b);
            out.push(spi.read_data());
        }
        spi.write_cnt(0);
        out
    }

    #[test]
    fn firmware_read_returns_user_settings() {
        let mut spi = Spi::new();
        let addr = USER_SETTINGS as u32;
        let mut cmd = vec![0x03, (addr >> 16) as u8, (addr >> 8) as u8, addr as u8];
        cmd.extend([0u8; 16]);
        let out = xfer(&mut spi, 1, &cmd);
        assert_eq!(out[4], 5, "user settings version");
        assert_eq!(out[4 + 6], b'K');
        assert_eq!(out[4 + 8], b'D');
    }

    #[test]
    fn touchscreen_reports_position() {
        let mut spi = Spi::new();
        spi.touch = Some((100, 50));
        let out = xfer(&mut spi, 2, &[0xD0, 0x00, 0x00]);
        let adc = ((out[1] as u16) << 5) | (out[2] as u16 >> 3);
        assert_eq!(adc, screen_to_adc(100));
        let out = xfer(&mut spi, 2, &[0x90, 0x00, 0x00]);
        let adc = ((out[1] as u16) << 5) | (out[2] as u16 >> 3);
        assert_eq!(adc, screen_to_adc(50));
    }

    #[test]
    fn user_settings_crc_matches() {
        let fw = build_firmware("Islander");
        let base = USER_SETTINGS;
        let crc = u16::from_le_bytes([fw[base + 0x72], fw[base + 0x73]]);
        assert_eq!(crc, crc16(0xFFFF, &fw[base..base + 0x70]));
        assert_eq!(fw[base + 0x1A], 8);
    }
}
