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
//! | GameCube | Animal Crossing / Dōbutsu no Mori+ / e+ | RTC time travel, Memory Card 251, GBA link port |
//! | Wii | City Folk / Let's Go to the City / Machi e Ikō yo | RTC time travel, IR pointer layout, SD card |

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
    /// Product code: 4 chars for NDS, 4-char prefix of the 6-char disc id
    /// for GameCube/Wii (maker code ignored).
    pub code: &'static str,
    pub title: &'static str,
    pub region: Region,
}

/// Every Animal Crossing release KD-Emulator recognises.
pub const TITLES: &[AcTitle] = &[
    AcTitle { system: System::GameCube, code: "GAFJ", title: "Dōbutsu no Mori+", region: Region::Japan },
    AcTitle { system: System::GameCube, code: "GAEJ", title: "Dōbutsu no Mori e+", region: Region::Japan },
    AcTitle { system: System::GameCube, code: "GAFE", title: "Animal Crossing", region: Region::NorthAmerica },
    AcTitle { system: System::GameCube, code: "GAFP", title: "Animal Crossing", region: Region::Europe },
    AcTitle { system: System::GameCube, code: "GAFU", title: "Animal Crossing", region: Region::Australia },
    AcTitle { system: System::Nds, code: "ADMJ", title: "Oideyo Dōbutsu no Mori", region: Region::Japan },
    AcTitle { system: System::Nds, code: "ADME", title: "Animal Crossing: Wild World", region: Region::NorthAmerica },
    AcTitle { system: System::Nds, code: "ADMP", title: "Animal Crossing: Wild World", region: Region::Europe },
    AcTitle { system: System::Nds, code: "ADMK", title: "Animal Crossing: Wild World", region: Region::Korea },
    AcTitle { system: System::Wii, code: "RUUJ", title: "Machi e Ikō yo: Dōbutsu no Mori", region: Region::Japan },
    AcTitle { system: System::Wii, code: "RUUE", title: "Animal Crossing: City Folk", region: Region::NorthAmerica },
    AcTitle {
        system: System::Wii,
        code: "RUUP",
        title: "Animal Crossing: Let's Go to the City",
        region: Region::Europe,
    },
    AcTitle { system: System::Wii, code: "RUUK", title: "Animal Crossing: City Folk", region: Region::Korea },
];

/// Look up a game by its product code (`ADME`, `GAFE01`, `RUUE01`, ...).
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
    /// Analog stick + C-stick + triggers.
    DualStick,
    /// Wii Remote IR pointer from touch drag / gyroscope, plus Nunchuk stick.
    Pointer,
}

/// GameCube memory card sizes (blocks as shown in the IPL).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryCardModel {
    Card59,
    Card251,
    Card1019,
}

impl MemoryCardModel {
    /// Capacity in megabits.
    pub fn megabits(self) -> u32 {
        match self {
            Self::Card59 => 4,
            Self::Card251 => 16,
            Self::Card1019 => 64,
        }
    }
    pub fn bytes(self) -> usize {
        self.megabits() as usize * 1024 * 1024 / 8
    }
}

/// What the Animal Crossing profile switches on for a system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcSetup {
    /// Guest RTC follows [`crate::clock::GuestClock`] with a per-game offset
    /// the player can change from the overlay ("time travel").
    pub rtc_time_travel: bool,
    pub layout: ControlLayout,
    /// GameCube: memory card inserted in slot A.
    pub memory_card: Option<MemoryCardModel>,
    /// GameCube: a GBA is attached to controller port 2 over JoyBus
    /// (island visits, NES games, pattern editing).
    pub gba_link_host: bool,
    /// GBA: boots multiboot images sent over JoyBus into EWRAM.
    pub joybus_client: bool,
    /// Wii: a virtual SD card is inserted.
    pub sd_card: bool,
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
            memory_card: None,
            gba_link_host: false,
            joybus_client: true,
            sd_card: false,
            accent_rgb: 0x4FB3A9, // lagoon teal
            blurb: "Link-cable companion: island trips, NES games and pattern editing over JoyBus",
        },
        System::Nds => AcSetup {
            rtc_time_travel: true,
            layout: ControlLayout::StylusFirst,
            memory_card: None,
            gba_link_host: false,
            joybus_client: false,
            sd_card: false,
            accent_rgb: 0x7EC8E3, // open sky
            blurb: "Stylus-first controls and a time-travel clock for every season",
        },
        System::GameCube => AcSetup {
            rtc_time_travel: true,
            layout: ControlLayout::DualStick,
            memory_card: Some(MemoryCardModel::Card251),
            gba_link_host: true,
            joybus_client: false,
            sd_card: false,
            accent_rgb: 0x78C25A, // leaf green
            blurb: "Memory Card 251, GBA link port and a time-travel clock",
        },
        System::Wii => AcSetup {
            rtc_time_travel: true,
            layout: ControlLayout::Pointer,
            memory_card: None,
            gba_link_host: false,
            joybus_client: false,
            sd_card: true,
            accent_rgb: 0xF2A65A, // city-lights orange
            blurb: "Pointer controls, SD card and a time-travel clock",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_system_has_a_setup_with_time_travel() {
        for s in [System::Gba, System::Nds, System::GameCube, System::Wii] {
            assert!(setup_for(s).rtc_time_travel, "{s:?}");
        }
    }

    #[test]
    fn lookup_by_code() {
        let t = lookup(System::GameCube, b"GAFE01").unwrap();
        assert_eq!(t.region, Region::NorthAmerica);
        assert_eq!(lookup(System::Nds, b"ADME").unwrap().title, "Animal Crossing: Wild World");
        assert_eq!(lookup(System::Wii, b"RUUP01").unwrap().region, Region::Europe);
        assert!(lookup(System::Wii, b"GAFE01").is_none(), "system must match");
        assert!(lookup(System::Nds, b"AD").is_none());
    }

    #[test]
    fn table_codes_are_consistent() {
        for t in TITLES {
            assert_eq!(t.code.len(), 4);
            assert_eq!(Region::from_code(t.code.as_bytes()[3]), Some(t.region), "{}", t.code);
        }
    }

    #[test]
    fn memory_card_251_is_2_mib() {
        assert_eq!(MemoryCardModel::Card251.bytes(), 2 * 1024 * 1024);
        assert_eq!(setup_for(System::GameCube).memory_card, Some(MemoryCardModel::Card251));
    }
}
