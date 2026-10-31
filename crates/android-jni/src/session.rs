//! Platform-independent emulation session: system detection, core
//! creation, the frame loop body, audio resampling, thermal policy and the
//! Animal Crossing time-travel controls. Everything here runs (and is
//! tested) on the host; the Android glue lives in `android.rs`.

use core_gcn::disc::{Disc, DiscHeader, IsoDisc, GC_MAGIC, WII_MAGIC};
use core_wii::containers::WbfsDisc;
use kd_core::animal_crossing::{self, ControlLayout};
use kd_core::audio::{audio_ring, AudioConsumer, AudioProducer, Resampler};
use kd_core::thermal::{GpuVendorProfile, ThermalPolicy, ThermalStatus, UserGraphicsPrefs};
use kd_core::topology::{CpuFeatures, CpuTopology};
use kd_core::{EmulatorCore, InputState, StereoFrame, System};
use std::time::Duration;

pub const OUTPUT_RATE: u32 = 48_000;

/// Guess the system from the file name and contents.
pub fn detect_system(name: &str, data: &[u8]) -> Option<System> {
    let lower = name.to_ascii_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or("");
    let be32 = |at: usize| data.get(at..at + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    match ext {
        "gba" | "agb" | "mb" => Some(System::Gba),
        "nds" | "srl" => Some(System::Nds),
        "wbfs" | "wad" => Some(System::Wii),
        "iso" | "gcm" => {
            if be32(0x18) == Some(WII_MAGIC) {
                Some(System::Wii)
            } else if be32(0x1C) == Some(GC_MAGIC) {
                Some(System::GameCube)
            } else {
                None
            }
        }
        "dol" | "elf" => Some(System::GameCube),
        _ => {
            // Header sniffing for files without a useful extension.
            if data.get(0xB2) == Some(&0x96) && data.len() >= 0xC0 {
                Some(System::Gba)
            } else if be32(0x1C) == Some(GC_MAGIC) {
                Some(System::GameCube)
            } else if be32(0x18) == Some(WII_MAGIC) {
                Some(System::Wii)
            } else {
                None
            }
        }
    }
}

/// Instantiate the right core for a loaded file.
pub fn create_core(name: &str, data: Vec<u8>) -> Result<Box<dyn EmulatorCore>, String> {
    let system = detect_system(name, &data).ok_or_else(|| format!("unrecognised file: {name}"))?;
    let lower = name.to_ascii_lowercase();
    Ok(match system {
        System::Gba => {
            if lower.ends_with(".mb") {
                Box::new(core_gba::Gba::new_multiboot(&data))
            } else {
                Box::new(core_gba::Gba::new(data))
            }
        }
        System::Nds => Box::new(core_nds::Nds::new(data).map_err(|e| e.to_string())?),
        System::GameCube => {
            if lower.ends_with(".dol") {
                Box::new(core_gcn::GameCube::from_dol(&data).map_err(|e| e.to_string())?)
            } else {
                Box::new(core_gcn::GameCube::from_disc(data).map_err(|e| e.to_string())?)
            }
        }
        System::Wii => {
            if lower.ends_with(".wad") {
                return Err("WAD titles must be installed to the virtual NAND first (needs your console keys)".into());
            }
            let mut wii = core_wii::Wii::new_machine().map_err(|e| e.to_string())?;
            let disc: Box<dyn Disc> = if lower.ends_with(".wbfs") {
                Box::new(WbfsDisc::new(data).map_err(|e| e.to_string())?)
            } else {
                Box::new(IsoDisc::new(data))
            };
            wii.insert_disc(disc).map_err(|e| e.to_string())?;
            Box::new(wii)
        }
    })
}

/// Read a disc/ROM header without booting (library screen).
pub fn peek_title(name: &str, data: &[u8]) -> Option<(System, String, String)> {
    let system = detect_system(name, data)?;
    let (code, title) = match system {
        System::Gba => {
            let h = core_gba::cart::Header::parse(data);
            (String::from_utf8_lossy(&h.game_code).into_owned(), h.title)
        }
        System::Nds => {
            let h = core_nds::cart::Header::parse(data);
            (String::from_utf8_lossy(&h.game_code).into_owned(), h.title)
        }
        _ => {
            let mut d = IsoDisc::new(data.get(..0x440)?.to_vec());
            let h = DiscHeader::read(&mut d).ok()?;
            (String::from_utf8_lossy(&h.game_id).into_owned(), h.title)
        }
    };
    let title = animal_crossing::lookup(system, code.as_bytes()).map(|t| t.title.to_string()).unwrap_or(title);
    Some((system, code, title))
}

/// Per-frame statistics for the mobile debugger.
#[derive(Debug, Clone, Copy, Default)]
pub struct FrameStats {
    pub frames: u64,
    pub last_frame: Duration,
    pub avg_frame_us: f64,
}

pub struct Session {
    pub core: Box<dyn EmulatorCore>,
    pub input: InputState,
    pub policy: ThermalPolicy,
    pub gpu: GpuVendorProfile,
    audio_tx: AudioProducer,
    audio_rx: Option<AudioConsumer>,
    resampler: Resampler,
    scratch: Vec<StereoFrame>,
    resampled: Vec<StereoFrame>,
    pub stats: FrameStats,
    pub topology: CpuTopology,
    pub features: CpuFeatures,
}

impl Session {
    pub fn new(core: Box<dyn EmulatorCore>) -> Self {
        let (tx, rx) = audio_ring(OUTPUT_RATE as usize / 5);
        let rate = core.audio_sample_rate();
        Self {
            core,
            input: InputState::default(),
            policy: ThermalPolicy::new(UserGraphicsPrefs::default()),
            gpu: GpuVendorProfile::Generic,
            audio_tx: tx,
            audio_rx: Some(rx),
            resampler: Resampler::new(rate, OUTPUT_RATE),
            scratch: Vec::with_capacity(2048),
            resampled: Vec::with_capacity(4096),
            stats: FrameStats::default(),
            topology: CpuTopology::detect(),
            features: CpuFeatures::detect(),
        }
    }

    /// Hand the audio consumer to the output stream (AAudio callback).
    pub fn take_audio_consumer(&mut self) -> Option<AudioConsumer> {
        self.audio_rx.take()
    }

    /// Run one guest frame and push its audio.
    pub fn step(&mut self) {
        let start = std::time::Instant::now();
        self.core.run_frame(&self.input);
        self.scratch.clear();
        self.core.drain_audio(&mut self.scratch);
        self.resampler.steer(self.audio_tx.len(), self.audio_tx.capacity());
        self.resampled.clear();
        self.resampler.process(&self.scratch, &mut self.resampled);
        self.audio_tx.push(&self.resampled);
        let took = start.elapsed();
        self.stats.frames += 1;
        self.stats.last_frame = took;
        let us = took.as_secs_f64() * 1e6;
        self.stats.avg_frame_us = if self.stats.frames == 1 { us } else { self.stats.avg_frame_us * 0.95 + us * 0.05 };
    }

    /// Guest frame rate (for pacing).
    pub fn frame_period(&self) -> Duration {
        let hz = match self.core.system() {
            System::Gba => 59.7275,
            System::Nds => 59.8261,
            System::GameCube | System::Wii => 59.94,
        };
        Duration::from_secs_f64(1.0 / hz)
    }

    pub fn set_thermal_status(&mut self, status: i32) -> bool {
        self.policy.on_status_changed(ThermalStatus::from_android(status))
    }

    /// Animal Crossing time travel: shift the guest RTC by `secs`.
    pub fn time_travel_by(&mut self, secs: i64) {
        self.core.clock().travel_by(secs);
    }

    pub fn set_clock_offset(&mut self, secs: i64) {
        self.core.clock().offset_secs = secs;
    }

    pub fn clock_offset(&mut self) -> i64 {
        self.core.clock().offset_secs
    }

    /// Key/value description of the Animal Crossing profile for the UI
    /// (`key=value` lines; empty when the game is not Animal Crossing).
    pub fn animal_crossing_info(&mut self) -> String {
        let system = self.core.system();
        let Some(title) = self.core.animal_crossing() else {
            return String::new();
        };
        let setup = animal_crossing::setup_for(system);
        let layout = match setup.layout {
            ControlLayout::Classic => "classic",
            ControlLayout::StylusFirst => "stylus",
            ControlLayout::DualStick => "dualstick",
            ControlLayout::Pointer => "pointer",
        };
        let now = self.core.clock().now();
        format!(
            "title={}\nsystem={}\nlayout={layout}\naccent={:06X}\nblurb={}\nclock={:04}-{:02}-{:02} {:02}:{:02}\n",
            title.title,
            system.name(),
            setup.accent_rgb,
            setup.blurb,
            now.year,
            now.month,
            now.day,
            now.hour,
            now.minute
        )
    }

    /// Text for the mobile debugger sheet.
    pub fn debug_info(&self) -> String {
        let knobs = self.policy.knobs();
        let clusters: Vec<String> = self
            .topology
            .clusters
            .iter()
            .map(|c| format!("{:?} {} MHz x{}", c.kind, c.max_freq_khz / 1000, c.cpus.len()))
            .collect();
        format!(
            "system={}\ngpu={}\nhost_page_size={}\nsoc={} ({})\nclusters={}\narmv9={}\nthermal={:?}\nubershaders={}\ndsp={:?}\nmax_scale={}\nframe_ms={:.2}\nframes={}\n",
            self.core.system().name(),
            self.gpu.name(),
            kd_core::arena::host_page_size(),
            self.topology.generation(),
            self.topology.layout(),
            clusters.join(", "),
            self.features.is_armv9_class(),
            self.policy.status(),
            knobs.async_ubershaders,
            knobs.dsp_mode,
            knobs.max_internal_scale,
            self.stats.avg_frame_us / 1000.0,
            self.stats.frames
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gba_rom() -> Vec<u8> {
        // Minimal valid header + infinite loop at the entry point.
        let mut rom = vec![0u8; 0x200];
        rom[0..4].copy_from_slice(&0xEAFF_FFFEu32.to_le_bytes()); // b .
        rom[0xA0..0xA6].copy_from_slice(b"KDTEST");
        rom[0xAC..0xB0].copy_from_slice(b"AKDE");
        rom[0xB2] = 0x96;
        rom[0xBD] = core_gba::cart::Header::complement(&rom);
        rom
    }

    #[test]
    fn detects_systems() {
        assert_eq!(detect_system("Game.GBA", &[]), Some(System::Gba));
        assert_eq!(detect_system("wild_world.nds", &[]), Some(System::Nds));
        let mut gc = vec![0u8; 0x440];
        gc[0x1C..0x20].copy_from_slice(&GC_MAGIC.to_be_bytes());
        assert_eq!(detect_system("ac.iso", &gc), Some(System::GameCube));
        let mut wii = vec![0u8; 0x440];
        wii[0x18..0x1C].copy_from_slice(&WII_MAGIC.to_be_bytes());
        assert_eq!(detect_system("cityfolk.iso", &wii), Some(System::Wii));
        assert_eq!(detect_system("noext", &gba_rom()), Some(System::Gba));
        assert_eq!(detect_system("readme.txt", b"hello"), None);
    }

    #[test]
    fn headless_session_runs_and_produces_audio() {
        let core = create_core("test.gba", gba_rom()).unwrap();
        let mut s = Session::new(core);
        let mut rx = s.take_audio_consumer().unwrap();
        for _ in 0..10 {
            s.step();
        }
        assert_eq!(s.stats.frames, 10);
        // 10 GBA frames ≈ 167 ms of audio at 48 kHz ≈ 8000 frames
        assert!(rx.len() > 6000, "queued {}", rx.len());
        let mut buf = vec![StereoFrame::SILENCE; 256];
        assert_eq!(rx.pop_into(&mut buf), 256);
        assert!(s.debug_info().contains("host_page_size="));
        assert!(s.frame_period() > Duration::from_millis(16));
    }

    #[test]
    fn time_travel_and_thermal_controls() {
        let mut s = Session::new(create_core("test.gba", gba_rom()).unwrap());
        s.time_travel_by(86_400);
        assert_eq!(s.clock_offset(), 86_400);
        s.set_clock_offset(0);
        assert_eq!(s.clock_offset(), 0);
        assert!(s.set_thermal_status(2), "MODERATE changes the knobs");
        assert!(s.debug_info().contains("ubershaders=true"));
        assert_eq!(s.animal_crossing_info(), "", "not an Animal Crossing game");
    }

    #[test]
    fn peek_reports_animal_crossing_titles() {
        let mut gc = vec![0u8; 0x440];
        gc[..6].copy_from_slice(b"GAFE01");
        gc[0x1C..0x20].copy_from_slice(&GC_MAGIC.to_be_bytes());
        let (sys, code, title) = peek_title("ac.gcm", &gc).unwrap();
        assert_eq!((sys, code.as_str(), title.as_str()), (System::GameCube, "GAFE01", "Animal Crossing"));
    }
}
