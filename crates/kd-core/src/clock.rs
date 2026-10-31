//! Guest real-time clocks with "time travel".
//!
//! Every system KD-Emulator targets has a battery-backed clock that games read
//! for their calendar — most famously Animal Crossing on GameCube, DS and Wii,
//! where seasons, events, visitors and turnip prices follow the console date.
//! A [`GuestClock`] is the host wall clock plus a user-controlled offset, so
//! players can visit a different season or undo an accidental skip without
//! touching the phone's system time. The offset is stored per game.

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds between 1970-01-01 and 2000-01-01 (GameCube/Wii RTC epoch).
pub const EPOCH_2000_UNIX: i64 = 946_684_800;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DateTime {
    pub year: i32,
    /// 1..=12
    pub month: u8,
    /// 1..=31
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    /// 0 = Sunday .. 6 = Saturday
    pub weekday: u8,
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`).
pub const fn days_from_civil(y: i32, m: u8, d: u8) -> i64 {
    let y = if m <= 2 { y as i64 - 1 } else { y as i64 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Inverse of [`days_from_civil`].
pub const fn civil_from_days(z: i64) -> (i32, u8, u8) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    ((if m <= 2 { y + 1 } else { y }) as i32, m, d)
}

impl DateTime {
    pub fn from_unix(secs: i64) -> Self {
        let days = secs.div_euclid(86_400);
        let rem = secs.rem_euclid(86_400);
        let (year, month, day) = civil_from_days(days);
        Self {
            year,
            month,
            day,
            hour: (rem / 3600) as u8,
            minute: (rem / 60 % 60) as u8,
            second: (rem % 60) as u8,
            weekday: (days + 4).rem_euclid(7) as u8, // 1970-01-01 was a Thursday
        }
    }

    pub fn to_unix(&self) -> i64 {
        days_from_civil(self.year, self.month, self.day) * 86_400
            + self.hour as i64 * 3600
            + self.minute as i64 * 60
            + self.second as i64
    }
}

/// Encode 0..=99 as packed BCD (GBA/NDS cartridge and console RTC chips).
#[inline]
pub const fn to_bcd(v: u8) -> u8 {
    ((v / 10) << 4) | (v % 10)
}

#[inline]
pub const fn from_bcd(v: u8) -> u8 {
    (v >> 4) * 10 + (v & 0xF)
}

/// Where the clock's base time comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockSource {
    /// Host wall clock (normal play).
    Host,
    /// Frozen at a fixed Unix time (tests, deterministic replays, netplay).
    Fixed(i64),
}

/// A guest RTC = base time + per-game offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuestClock {
    pub source: ClockSource,
    /// Seconds added to the base time ("time travel").
    pub offset_secs: i64,
}

impl Default for GuestClock {
    fn default() -> Self {
        Self { source: ClockSource::Host, offset_secs: 0 }
    }
}

impl GuestClock {
    pub fn host() -> Self {
        Self::default()
    }

    pub fn fixed(unix: i64) -> Self {
        Self { source: ClockSource::Fixed(unix), offset_secs: 0 }
    }

    fn base(&self) -> i64 {
        match self.source {
            ClockSource::Host => SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0),
            ClockSource::Fixed(t) => t,
        }
    }

    /// Current guest time as Unix seconds.
    pub fn now_unix(&self) -> i64 {
        self.base() + self.offset_secs
    }

    pub fn now(&self) -> DateTime {
        DateTime::from_unix(self.now_unix())
    }

    /// Seconds since 2000-01-01 00:00:00 (GameCube/Wii RTC counter value
    /// once the IPL's counter bias has been applied).
    pub fn seconds_since_2000(&self) -> i64 {
        self.now_unix() - EPOCH_2000_UNIX
    }

    /// Move the guest clock so that it currently reads `target`.
    pub fn travel_to(&mut self, target: DateTime) {
        self.offset_secs = target.to_unix() - self.base();
    }

    /// Shift by a relative amount (e.g. `+86_400` = tomorrow).
    pub fn travel_by(&mut self, secs: i64) {
        self.offset_secs += secs;
    }

    /// Return to real time.
    pub fn reset(&mut self) {
        self.offset_secs = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_round_trip() {
        for days in (-800_000..800_000).step_by(997) {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 1, 1) * 86_400, EPOCH_2000_UNIX);
        assert_eq!(civil_from_days(days_from_civil(2024, 2, 29)), (2024, 2, 29));
    }

    #[test]
    fn known_dates() {
        // 2001-12-14 (a Friday)
        let t = DateTime { year: 2001, month: 12, day: 14, hour: 9, minute: 30, second: 5, weekday: 5 };
        let back = DateTime::from_unix(t.to_unix());
        assert_eq!(back, t);
        assert_eq!(DateTime::from_unix(0).weekday, 4);
        assert_eq!(DateTime::from_unix(-1).year, 1969);
    }

    #[test]
    fn bcd() {
        for v in 0..100 {
            assert_eq!(from_bcd(to_bcd(v)), v);
        }
        assert_eq!(to_bcd(59), 0x59);
    }

    #[test]
    fn time_travel() {
        let mut c = GuestClock::fixed(EPOCH_2000_UNIX);
        assert_eq!(c.seconds_since_2000(), 0);
        let xmas = DateTime { year: 2005, month: 12, day: 24, hour: 18, minute: 0, second: 0, weekday: 6 };
        c.travel_to(xmas);
        assert_eq!(c.now(), xmas);
        c.travel_by(86_400);
        assert_eq!(c.now().day, 25);
        assert_eq!(c.now().weekday, 0);
        c.reset();
        assert_eq!(c.now_unix(), EPOCH_2000_UNIX);
    }
}
