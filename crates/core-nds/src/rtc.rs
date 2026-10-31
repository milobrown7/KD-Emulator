//! Seiko S-35180 real-time clock on the ARM7 (0x04000138), driven by the
//! time-travel [`GuestClock`] — this is the clock Animal Crossing: Wild
//! World reads for seasons, holidays, visitors and shop hours.

use kd_core::clock::{to_bcd, GuestClock};
use kd_core::{SaveState, StateReader, StateResult, StateWriter};

#[derive(Debug, Clone, Default)]
pub struct Rtc {
    reg: u16,
    selected: bool,
    sck: bool,
    shift: u8,
    bits: u8,
    command: Option<u8>,
    out: Vec<u8>,
    out_pos: usize,
    out_bit: u8,
    written: Vec<u8>,
    pub status1: u8,
    pub status2: u8,
}

const PIN_DATA: u16 = 1;
const PIN_SCK: u16 = 2;
const PIN_CS: u16 = 4;

impl Rtc {
    pub fn new() -> Self {
        Self { status1: 0x02, ..Default::default() } // 24-hour mode
    }

    pub fn read(&self) -> u16 {
        let mut v = self.reg & 0xFFF6;
        if self.reg & 0x10 == 0 {
            v |= self.out_bit as u16 & PIN_DATA;
        } else {
            v |= self.reg & PIN_DATA;
        }
        v
    }

    pub fn write(&mut self, v: u16, clock: &GuestClock) {
        self.reg = v;
        let cs = v & PIN_CS != 0;
        let sck = v & PIN_SCK != 0;
        if !cs {
            self.selected = false;
            self.command = None;
            self.bits = 0;
            self.shift = 0;
            self.out.clear();
            self.written.clear();
            self.sck = sck;
            return;
        }
        if !self.selected {
            self.selected = true;
            self.sck = sck;
            return;
        }
        // The DS clocks data on the rising edge of SCK (pin is active low
        // during the low phase of the cycle).
        let rising = sck && !self.sck;
        self.sck = sck;
        if !rising {
            return;
        }
        if self.out_pos < self.out.len() {
            self.out_bit = (self.out[self.out_pos] >> self.bits) & 1;
            self.bits += 1;
            if self.bits == 8 {
                self.bits = 0;
                self.out_pos += 1;
            }
            return;
        }
        self.shift |= ((v & PIN_DATA) as u8) << self.bits;
        self.bits += 1;
        if self.bits < 8 {
            return;
        }
        let byte = self.shift;
        self.shift = 0;
        self.bits = 0;
        match self.command {
            None => self.start_command(byte, clock),
            Some(reg) => {
                self.written.push(byte);
                match reg {
                    0 => self.status1 = byte & 0x0E | (self.status1 & 0x01),
                    1 => self.status2 = byte,
                    _ => {} // date/time writes: the GuestClock is the source of truth
                }
            }
        }
    }

    fn start_command(&mut self, byte: u8, clock: &GuestClock) {
        // Normalise to the 0110-RRR-W layout (games send either bit order).
        let cmd = if byte & 0xF0 == 0x60 {
            byte
        } else if byte & 0x0F == 0x06 {
            byte.reverse_bits()
        } else {
            return;
        };
        let reg = (cmd >> 1) & 7;
        let reading = cmd & 1 != 0;
        self.command = Some(reg);
        if !reading {
            return;
        }
        let now = clock.now();
        let h24 = self.status1 & 0x02 != 0;
        let hour = if h24 { to_bcd(now.hour) } else { to_bcd(now.hour % 12) };
        let pm = if now.hour >= 12 { 0x40 } else { 0 };
        let time = [hour | pm, to_bcd(now.minute), to_bcd(now.second)];
        self.out = match reg {
            0 => vec![self.status1],
            1 => vec![self.status2],
            2 => vec![
                to_bcd(now.year.rem_euclid(100) as u8),
                to_bcd(now.month),
                to_bcd(now.day),
                now.weekday,
                time[0],
                time[1],
                time[2],
            ],
            3 => time.to_vec(),
            _ => vec![0; 3],
        };
        self.out_pos = 0;
        self.bits = 0;
    }
}

impl SaveState for Rtc {
    fn save_state(&self, w: &mut StateWriter) {
        w.u16(self.reg);
        w.u8(self.status1);
        w.u8(self.status2);
    }
    fn load_state(&mut self, r: &mut StateReader<'_>) -> StateResult<()> {
        self.reg = r.u16()?;
        self.status1 = r.u8()?;
        self.status2 = r.u8()?;
        self.selected = false;
        self.command = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kd_core::clock::{DateTime, EPOCH_2000_UNIX};

    fn send_byte(rtc: &mut Rtc, b: u8, clock: &GuestClock) {
        for i in 0..8 {
            let bit = ((b >> i) & 1) as u16;
            rtc.write(0x10 | 0x20 | 0x40 | PIN_CS | bit, clock);
            rtc.write(0x10 | 0x20 | 0x40 | PIN_CS | PIN_SCK | bit, clock);
        }
    }

    fn recv_byte(rtc: &mut Rtc, clock: &GuestClock) -> u8 {
        let mut b = 0;
        for i in 0..8 {
            rtc.write(0x20 | 0x40 | PIN_CS, clock);
            rtc.write(0x20 | 0x40 | PIN_CS | PIN_SCK, clock);
            b |= ((rtc.read() & PIN_DATA) as u8) << i;
        }
        b
    }

    #[test]
    fn wild_world_style_date_read_with_time_travel() {
        let mut clock = GuestClock::fixed(EPOCH_2000_UNIX);
        // Visit the Harvest Festival: fourth Thursday of November 2005.
        clock.travel_to(DateTime { year: 2005, month: 11, day: 24, hour: 18, minute: 30, second: 0, weekday: 4 });
        let mut rtc = Rtc::new();
        rtc.write(0x70, &clock); // CS low
        rtc.write(0x70 | PIN_CS | PIN_SCK, &clock);
        send_byte(&mut rtc, 0x65, &clock); // 0110-010-1: read date/time
        let bytes: Vec<u8> = (0..7).map(|_| recv_byte(&mut rtc, &clock)).collect();
        assert_eq!(bytes, vec![0x05, 0x11, 0x24, 4, 0x18 | 0x40, 0x30, 0x00]);
    }
}
