//! External Interface: three channels with chip-select, immediate and DMA
//! transfers. Devices: the IPL chip (RTC + SRAM; the IPL ROM itself is not
//! bundled) and memory cards — Animal Crossing ships with a Memory Card 251.

use kd_core::animal_crossing::MemoryCardModel;
use kd_core::GuestClock;

/// An EXI device on a chip-select line.
pub trait ExiDevice: Send {
    fn select(&mut self) {}
    fn deselect(&mut self) {}
    /// Exchange one byte.
    fn transfer(&mut self, out: u8, clock: &GuestClock) -> u8;
}

/// The IPL chip on channel 0 / device 1: RTC, SRAM and UART.
pub struct IplDevice {
    pub sram: [u8; 64],
    cmd: u32,
    cmd_len: usize,
    pos: usize,
    rtc_latch: [u8; 4],
}

impl Default for IplDevice {
    fn default() -> Self {
        Self::new()
    }
}

impl IplDevice {
    pub fn new() -> Self {
        let mut sram = [0u8; 64];
        sram[0x13] = 0x2C; // flags: sound mode stereo, progressive off
        Self::fix_checksum(&mut sram);
        Self { sram, cmd: 0, cmd_len: 0, pos: 0, rtc_latch: [0; 4] }
    }

    /// SRAM checksum over words 6..10 (counter bias, display offset, NTD,
    /// language, flags).
    pub fn fix_checksum(sram: &mut [u8; 64]) {
        let mut sum = 0u16;
        let mut inv = 0u16;
        for i in 6..10 {
            let w = u16::from_be_bytes([sram[i * 2], sram[i * 2 + 1]]);
            sum = sum.wrapping_add(w);
            inv = inv.wrapping_add(w ^ 0xFFFF);
        }
        sram[0..2].copy_from_slice(&sum.to_be_bytes());
        sram[2..4].copy_from_slice(&inv.to_be_bytes());
    }
}

impl ExiDevice for IplDevice {
    fn select(&mut self) {
        self.cmd = 0;
        self.cmd_len = 0;
        self.pos = 0;
    }

    fn transfer(&mut self, out: u8, clock: &GuestClock) -> u8 {
        if self.cmd_len < 4 {
            self.cmd = self.cmd << 8 | out as u32;
            self.cmd_len += 1;
            if self.cmd_len == 4 && self.cmd & 0x7FFF_FF00 == 0x2000_0000 {
                // RTC: seconds since 2000-01-01 (the OS adds the SRAM counter
                // bias, which we keep at 0) — this is the time-travel clock.
                self.rtc_latch = (clock.seconds_since_2000() as u32).to_be_bytes();
            }
            return 0;
        }
        let write = self.cmd & 0x8000_0000 != 0;
        let region = self.cmd & 0x7FFF_FF00;
        let p = self.pos;
        self.pos += 1;
        match region {
            0x2000_0000 => self.rtc_latch.get(p).copied().unwrap_or(0),
            0x2000_0100 => {
                let i = (p + (self.cmd as usize & 0x3F)) % 64;
                if write {
                    self.sram[i] = out;
                    0
                } else {
                    self.sram[i]
                }
            }
            0x2001_0000 => 0, // UART
            _ => 0,           // IPL ROM (fonts) not available
        }
    }
}

/// A GameCube memory card (GCI/raw image in `data`).
pub struct MemoryCard {
    pub model: MemoryCardModel,
    pub data: Vec<u8>,
    pub dirty: bool,
    cmd: Option<u8>,
    args: Vec<u8>,
    status: u8,
}

const STATUS_READY: u8 = 0x01;
const STATUS_UNLOCKED: u8 = 0x40;

impl MemoryCard {
    pub fn new(model: MemoryCardModel) -> Self {
        Self { model, data: vec![0xFF; model.bytes()], dirty: false, cmd: None, args: Vec::new(), status: STATUS_READY | STATUS_UNLOCKED }
    }

    fn address(&self) -> usize {
        let a = &self.args;
        ((a[0] as usize & 0x7F) << 17) | (a[1] as usize) << 9 | (a[2] as usize & 3) << 7 | (a[3] as usize & 0x7F)
    }
}

impl ExiDevice for MemoryCard {
    fn select(&mut self) {
        self.cmd = None;
        self.args.clear();
    }

    fn deselect(&mut self) {
        // Erase commands take effect when the chip is released.
        match self.cmd {
            Some(0xF1) if self.args.len() >= 2 => {
                let sector = ((self.args[0] as usize & 0x7F) << 17 | (self.args[1] as usize) << 9) & !0x1FFF;
                let end = (sector + 0x2000).min(self.data.len());
                self.data[sector.min(end)..end].fill(0xFF);
                self.dirty = true;
            }
            Some(0xF4) => {
                self.data.fill(0xFF);
                self.dirty = true;
            }
            _ => {}
        }
        self.cmd = None;
    }

    fn transfer(&mut self, out: u8, _clock: &GuestClock) -> u8 {
        let Some(cmd) = self.cmd else {
            self.cmd = Some(out);
            if out == 0x89 {
                self.status = STATUS_READY | STATUS_UNLOCKED;
            }
            return 0;
        };
        self.args.push(out);
        let n = self.args.len();
        match cmd {
            // Get ID: one dummy byte, then the 32-bit ID (card size in Mbit).
            0x00 => {
                let id = self.model.megabits().to_be_bytes();
                if (2..=5).contains(&n) {
                    id[n - 2]
                } else {
                    0
                }
            }
            0x83 => self.status,
            // Page read: 4 address bytes, 4 latency bytes, then data.
            0x52 => {
                if n <= 8 {
                    0xFF
                } else {
                    let i = (self.address() + n - 9) % self.data.len();
                    self.data[i]
                }
            }
            // Page program: 4 address bytes then up to 128 data bytes.
            0xF2 => {
                if n > 4 {
                    let page = self.address() & !0x7F;
                    let i = (page + ((self.address() & 0x7F) + n - 5) % 0x80) % self.data.len();
                    self.data[i] = out;
                    self.dirty = true;
                }
                0
            }
            _ => 0,
        }
    }
}

/// One EXI channel's registers and selected devices.
#[derive(Default)]
pub struct ExiChannel {
    pub csr: u32,
    pub mar: u32,
    pub length: u32,
    pub cr: u32,
    pub data: u32,
    pub devices: [Option<Box<dyn ExiDevice>>; 3],
    selected: Option<usize>,
}

impl ExiChannel {
    /// Interrupt line: (EXI interrupt | transfer complete) & masks.
    pub fn interrupt(&self) -> bool {
        (self.csr & 0x3 == 0x3) || (self.csr & 0xC == 0xC) || (self.csr & 0xC00 == 0xC00)
    }

    pub fn write_csr(&mut self, v: u32) {
        // Interrupt status bits are write-1-to-clear.
        let status = self.csr & 0x80A & !(v & 0x80A);
        self.csr = (v & !0x80A) | status;
        let cs = (v >> 7) & 7;
        let new = match cs {
            1 => Some(0),
            2 => Some(1),
            4 => Some(2),
            _ => None,
        };
        if new != self.selected {
            if let Some(d) = self.selected.and_then(|i| self.devices[i].as_mut()) {
                d.deselect();
            }
            if let Some(d) = new.and_then(|i| self.devices[i].as_mut()) {
                d.select();
            }
            self.selected = new;
        }
    }

    pub fn read_csr(&self) -> u32 {
        let connected = self.devices[0].is_some() as u32;
        self.csr | connected << 12
    }

    /// CR write: run the transfer immediately. `ram` is main memory for DMA.
    pub fn write_cr(&mut self, v: u32, ram: &mut [u8], clock: &GuestClock) {
        self.cr = v & 0x3F;
        if v & 1 == 0 {
            return;
        }
        let dev = self.selected.and_then(|i| self.devices[i].as_mut());
        let rw = (v >> 2) & 3;
        if v & 2 != 0 {
            let start = (self.mar & 0x03FF_FFE0) as usize;
            let len = (self.length & 0x03FF_FFE0) as usize;
            if let Some(d) = dev {
                for i in 0..len {
                    let Some(slot) = ram.get_mut(start + i) else { break };
                    if rw == 0 {
                        *slot = d.transfer(0, clock);
                    } else {
                        d.transfer(*slot, clock);
                    }
                }
            }
        } else {
            let n = ((v >> 4) & 3) as usize + 1;
            let mut result = 0u32;
            let mut dev = dev;
            for i in 0..n {
                let out = (self.data >> (24 - i * 8)) as u8;
                let inb = dev.as_mut().map_or(0xFF, |d| d.transfer(out, clock));
                result |= (inb as u32) << (24 - i * 8);
            }
            if rw != 1 {
                self.data = result;
            }
        }
        self.cr &= !1;
        self.csr |= 0x8; // TCINT
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kd_core::clock::{DateTime, EPOCH_2000_UNIX};

    fn imm(ch: &mut ExiChannel, word: u32, len: u32, write: bool, clock: &GuestClock) -> u32 {
        ch.data = word;
        ch.write_cr(1 | (if write { 1 } else { 0 }) << 2 | (len - 1) << 4, &mut [], clock);
        ch.data
    }

    #[test]
    fn rtc_reads_time_travel_clock() {
        let mut clock = GuestClock::fixed(EPOCH_2000_UNIX);
        clock.travel_to(DateTime { year: 2002, month: 9, day: 16, hour: 10, minute: 0, second: 0, weekday: 1 });
        let mut ch = ExiChannel::default();
        ch.devices[1] = Some(Box::new(IplDevice::new()));
        ch.write_csr(2 << 7); // select device 1
        imm(&mut ch, 0x2000_0000, 4, true, &clock);
        let secs = imm(&mut ch, 0, 4, false, &clock);
        assert_eq!(secs as i64, clock.seconds_since_2000());
        assert_ne!(ch.csr & 0x8, 0, "transfer complete");
    }

    #[test]
    fn sram_checksum() {
        let ipl = IplDevice::new();
        let sum = u16::from_be_bytes([ipl.sram[0], ipl.sram[1]]);
        let inv = u16::from_be_bytes([ipl.sram[2], ipl.sram[3]]);
        assert_eq!(sum.wrapping_add(inv), 0xFFFC, "sum + inverse = 4 * 0xFFFF (mod 2^16)");
    }

    #[test]
    fn memory_card_251_id_program_and_read() {
        let clock = GuestClock::fixed(0);
        let mut ch = ExiChannel::default();
        ch.devices[0] = Some(Box::new(MemoryCard::new(MemoryCardModel::Card251)));
        let sel = |ch: &mut ExiChannel| {
            ch.write_csr(0);
            ch.write_csr(1 << 7);
        };
        sel(&mut ch);
        imm(&mut ch, 0x0000_0000, 2, true, &clock);
        assert_eq!(imm(&mut ch, 0, 4, false, &clock), 16, "251-block card = 16 Mbit");
        // program "KD" at 0x4000
        sel(&mut ch);
        imm(&mut ch, 0xF200_2000, 4, true, &clock); // cmd + addr bytes 0x00, 0x20, 0x00
        imm(&mut ch, 0x0000_0000, 1, true, &clock); // last addr byte
        imm(&mut ch, 0x4B44_0000, 2, true, &clock);
        // read back
        sel(&mut ch);
        imm(&mut ch, 0x5200_2000, 4, true, &clock);
        imm(&mut ch, 0x0000_0000, 4, true, &clock); // addr byte + 3 latency
        imm(&mut ch, 0, 1, true, &clock); // final latency byte
        let v = imm(&mut ch, 0, 2, false, &clock);
        assert_eq!(v >> 16, 0x4B44);
    }
}
