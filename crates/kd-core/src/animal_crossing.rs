//! Animal Crossing profiles — one for every system KD-Emulator runs.
//!
//! Each console core consults this table when a game boots. A match turns on
//! the per-system Animal Crossing setup: the time-travel RTC, the right save
//! media, link features and the controller layout the game plays best with.
//!
//! | System | Title(s) | What the profile enables |
//! |--------|----------|--------------------------|
//! | GBA | Link-cable companion (island, NES games, patterns) | JoyBus link mode + multiboot loader |
//! | NDS | Wild World / Oideyo Dōbutsu no Mori | RTC time travel, stylus-first layout |

use crate::System;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    Japan,
    NorthAmerica,
    Europe,
    Australia,
    Korea,
}

impl Region {
    /// Nintendo game-code region letter (4th character).
    pub fn from_code(c: u8) -> Option<Self> {
        Some(match c {
            b'J' => Self::Japan,
            b'E' => Self::NorthAmerica,
            b'P' => Self::Europe,
            b'U' => Self::Australia,
            b'K' => Self::Korea,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcTitle {
    pub system: System,
    /// 4-character product code from the cartridge header.
    pub code: &'static str,
    pub title: &'static str,
    pub region: Region,
}

/// Every Animal Crossing release KD-Emulator recognises.
pub const TITLES: &[AcTitle] = &[
    AcTitle { system: System::Nds, code: "ADMJ", title: "Oideyo Dōbutsu no Mori", region: Region::Japan },
    AcTitle { system: System::Nds, code: "ADME", title: "Animal Crossing: Wild World", region: Region::NorthAmerica },
    AcTitle { system: System::Nds, code: "ADMP", title: "Animal Crossing: Wild World", region: Region::Europe },
    AcTitle { system: System::Nds, code: "ADMK", title: "Animal Crossing: Wild World", region: Region::Korea },
];

/// Look up a game by its product code (`ADME`, ...).
pub fn lookup(system: System, code: &[u8]) -> Option<&'static AcTitle> {
    let code = code.get(..4)?;
    TITLES.iter().find(|t| t.system == system && t.code.as_bytes() == code)
}

/// Preferred on-screen control scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlLayout {
    /// D-pad + face buttons + shoulders.
    Classic,
    /// Bottom screen is the main input; buttons shrink to the sides.
    StylusFirst,
}

/// What the Animal Crossing profile switches on for a system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcSetup {
    /// Guest RTC follows [`crate::clock::GuestClock`] with a per-game offset
    /// the player can change from the overlay ("time travel").
    pub rtc_time_travel: bool,
    pub layout: ControlLayout,
    /// GBA: boots multiboot images sent over JoyBus into EWRAM.
    pub joybus_client: bool,
    /// Accent colour used by the library and overlay (0xRRGGBB), drawn from
    /// a cosy island palette.
    pub accent_rgb: u32,
    /// One-line summary shown in the ROM library.
    pub blurb: &'static str,
}

pub fn setup_for(system: System) -> AcSetup {
    match system {
        System::Gba => AcSetup {
            rtc_time_travel: true,
            layout: ControlLayout::Classic,
            joybus_client: true,
            accent_rgb: 0x4FB3A9, // lagoon teal
            blurb: "Link-cable mode: multiboot images sent over JoyBus boot straight into EWRAM",
        },
        System::Nds => AcSetup {
            rtc_time_travel: true,
            layout: ControlLayout::StylusFirst,
            joybus_client: false,
            accent_rgb: 0x7EC8E3, // open sky
            blurb: "Stylus-first controls and a time-travel clock for every season",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_system_has_a_setup_with_time_travel() {
        for s in [System::Gba, System::Nds] {
            assert!(setup_for(s).rtc_time_travel, "{s:?}");
        }
    }

    #[test]
    fn lookup_by_code() {
        let t = lookup(System::Nds, b"ADME01").unwrap();
        assert_eq!(t.region, Region::NorthAmerica);
        assert_eq!(t.title, "Animal Crossing: Wild World");
        assert!(lookup(System::Gba, b"ADME").is_none(), "system must match");
        assert!(lookup(System::Nds, b"AD").is_none());
    }

    #[test]
    fn table_codes_are_consistent() {
        for t in TITLES {
            assert_eq!(t.code.len(), 4);
            assert_eq!(Region::from_code(t.code.as_bytes()[3]), Some(t.region), "{}", t.code);
        }
    }
}
