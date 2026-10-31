//! Platform-independent emulation session: system detection, core
//! creation, the frame loop body (fast-forward, rewind, cheats), audio
//! resampling, thermal policy, game metadata for the library and the
//! Animal Crossing time-travel controls. Everything here runs (and is
//! tested) on the host; the Android glue lives in `android.rs`.

use core_gcn::disc::{Disc, DiscHeader, IsoDisc, GC_MAGIC, WII_MAGIC};
use core_wii::containers::WbfsDisc;
use kd_core::animal_crossing::{self, ControlLayout};
use kd_core::audio::{audio_ring, AudioConsumer, AudioProducer, Resampler};
use kd_core::cheats::{Cheat, CheatFormat};
use kd_core::rewind::RewindBuffer;
use kd_core::thermal::{GpuVendorProfile, ThermalPolicy, ThermalStatus, UserGraphicsPrefs};
use kd_core::topology::{CpuFeatures, CpuTopology};
use kd_core::{CoreMemory, EmulatorCore, InputState, StereoFrame, System};
use std::time::Duration;

pub const OUTPUT_RATE: u32 = 48_000;
/// Rewind history budget.
pub const REWIND_BUDGET: usize = 96 << 20;
/// While rewinding, step back one snapshot every this many displayed frames.
const REWIND_PACE: u32 = 3;
/// Separates cheats in [`Session::set_cheats`] input.
pub const CHEAT_RECORD_SEP: char = '\u{1E}';
/// Separates a cheat's format id from its code.
pub const CHEAT_FIELD_SEP: char = '\u{1F}';

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

/// Read a disc/ROM header without booting.
pub fn peek_title(name: &str, data: &[u8]) -> Option<(System, String, String)> {
    let mut read = |off: u64, buf: &mut [u8]| {
        let o = off as usize;
        match data.get(o..o + buf.len()) {
            Some(s) => {
                buf.copy_from_slice(s);
                true
            }
            None => false,
        }
    };
    game_info(name, &mut read).map(|g| (g.system, g.code, g.title))
}

/// What the library shows for a game.
#[derive(Debug, Clone, PartialEq)]
pub struct GameInfo {
    pub system: System,
    pub code: String,
    pub title: String,
    /// Artwork from the game itself (DS icon, GameCube banner) in RGBA8888.
    pub icon: Option<(u32, u32, Vec<u32>)>,
    pub animal_crossing: bool,
}

/// Read metadata with random access (`read` fills a buffer from a file
/// offset and returns false past the end), so only the header and banner
/// are touched even for multi-gigabyte discs.
pub fn game_info(name: &str, read: &mut dyn FnMut(u64, &mut [u8]) -> bool) -> Option<GameInfo> {
    let mut head = vec![0u8; 0x440];
    if !read(0, &mut head) {
        // Small files (GBA homebrew, multiboot images): read what exists.
        let mut n = 0x440;
        while n > 0xC0 {
            n /= 2;
            head.truncate(n);
            if read(0, &mut head) {
                break;
            }
        }
    }
    let lower = name.to_ascii_lowercase();
    let wbfs = head.starts_with(b"WBFS");
    let system = if wbfs { System::Wii } else { detect_system(name, &head)? };
    let mut icon = None;
    let (code, mut title) = match system {
        System::Gba => {
            let h = core_gba::cart::Header::parse(&head);
            (String::from_utf8_lossy(&h.game_code).into_owned(), h.title)
        }
        System::Nds => {
            let h = core_nds::cart::Header::parse(&head);
            let mut title = h.title;
            let off = core_nds::cart::banner_offset(&head);
            if off != 0 {
                let mut b = vec![0u8; core_nds::cart::BANNER_SIZE];
                if read(off as u64, &mut b) {
                    if let Some(banner) = core_nds::cart::parse_banner(&b) {
                        if let Some(t) = banner.display_title() {
                            title = t;
                        }
                        icon = Some((32, 32, banner.icon));
                    }
                }
            }
            (String::from_utf8_lossy(&h.game_code).into_owned(), title)
        }
        _ if lower.ends_with(".dol") || lower.ends_with(".elf") => {
            let stem = name.rsplit('/').next().unwrap_or(name);
            ("DOL".to_string(), stem.rsplit_once('.').map_or(stem, |(s, _)| s).to_string())
        }
        _ => {
            // WBFS keeps a copy of the disc header at the start of its
            // second hard-disk sector.
            let disc_head = if wbfs {
                let shift = head.get(8).copied().unwrap_or(9).clamp(9, 20);
                let mut d = vec![0u8; 0x100];
                if !read(1u64 << shift, &mut d) {
                    return None;
                }
                d
            } else {
                head.clone()
            };
            let mut pad = disc_head.clone();
            pad.resize(0x440, 0);
            let mut d = IsoDisc::new(pad);
            let h = DiscHeader::read(&mut d).ok()?;
            let mut title = h.title;
            if system == System::GameCube {
                if let Some(b) = core_gcn::disc::read_banner(read) {
                    if !b.long_name.is_empty() && !h.game_id.get(3).is_some_and(|&r| r == b'J') {
                        title = b.long_name;
                    }
                    icon = Some((96, 32, b.image));
                }
            }
            (String::from_utf8_lossy(&h.game_id).into_owned(), title)
        }
    };
    let ac = animal_crossing::lookup(system, code.as_bytes());
    if let Some(t) = ac {
        title = t.title.to_string();
    }
    Some(GameInfo { system, code, title: title.trim().to_string(), icon, animal_crossing: ac.is_some() })
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
    core_rate: u32,
    scratch: Vec<StereoFrame>,
    resampled: Vec<StereoFrame>,
    pub stats: FrameStats,
    pub topology: CpuTopology,
    pub features: CpuFeatures,
    /// 1 = normal speed; 2–8 run that many frames per displayed frame.
    speed: u32,
    /// Output volume, 0–256 (256 = unity).
    pub volume: u32,
    rewind: RewindBuffer,
    /// Frames between rewind snapshots (0 disables rewind).
    rewind_interval: u32,
    since_snapshot: u32,
    rewinding: bool,
    rewind_tick: u32,
    rewind_cursor: Option<Vec<u8>>,
    cheats: Vec<Cheat>,
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
            core_rate: rate,
            scratch: Vec::with_capacity(4096),
            resampled: Vec::with_capacity(4096),
            stats: FrameStats::default(),
            topology: CpuTopology::detect(),
            features: CpuFeatures::detect(),
            speed: 1,
            volume: 256,
            rewind: RewindBuffer::new(REWIND_BUDGET),
            rewind_interval: 0,
            since_snapshot: 0,
            rewinding: false,
            rewind_tick: 0,
            rewind_cursor: None,
            cheats: Vec::new(),
        }
    }

    /// Hand the audio consumer to the output stream (AAudio callback).
    pub fn take_audio_consumer(&mut self) -> Option<AudioConsumer> {
        self.audio_rx.take()
    }

    /// Run one displayed frame: `speed` guest frames normally, or one
    /// rewound frame while rewind is held.
    pub fn step(&mut self) {
        let start = std::time::Instant::now();
        self.scratch.clear();
        if self.rewinding {
            self.rewind_tick += 1;
            if self.rewind_cursor.is_none() || self.rewind_tick % REWIND_PACE == 0 {
                if let Some(state) = self.rewind.pop() {
                    self.rewind_cursor = Some(state);
                }
            }
            if let Some(state) = &self.rewind_cursor {
                let _ = self.core.load_state(state);
            }
            // Render the restored moment; rewinding is silent.
            self.core.run_frame(&InputState::default());
            self.core.drain_audio(&mut self.scratch);
            self.scratch.clear();
        } else {
            for _ in 0..self.speed {
                self.core.run_frame(&self.input);
                self.apply_cheats();
                self.core.drain_audio(&mut self.scratch);
                if self.rewind_interval > 0 {
                    self.since_snapshot += 1;
                    if self.since_snapshot >= self.rewind_interval {
                        self.since_snapshot = 0;
                        self.rewind.push(self.core.save_state());
                    }
                }
            }
        }
        self.resampler.steer(self.audio_tx.len(), self.audio_tx.capacity());
        self.resampled.clear();
        self.resampler.process(&self.scratch, &mut self.resampled);
        if self.volume != 256 {
            let v = self.volume as i32;
            for f in &mut self.resampled {
                f.left = ((f.left as i32 * v) >> 8) as i16;
                f.right = ((f.right as i32 * v) >> 8) as i16;
            }
        }
        self.audio_tx.push(&self.resampled);
        let took = start.elapsed();
        self.stats.frames += 1;
        self.stats.last_frame = took;
        let us = took.as_secs_f64() * 1e6;
        self.stats.avg_frame_us = if self.stats.frames == 1 { us } else { self.stats.avg_frame_us * 0.95 + us * 0.05 };
    }

    fn apply_cheats(&mut self) {
        if self.cheats.is_empty() {
            return;
        }
        let mut mem = CoreMemory(self.core.as_mut());
        for c in &self.cheats {
            c.apply(&mut mem);
        }
    }

    /// Fast-forward factor (1 = off). Audio is resampled so it keeps pace.
    pub fn set_speed(&mut self, speed: u32) {
        self.speed = speed.clamp(1, 8);
        self.resampler.set_input_rate(self.core_rate * self.speed);
    }

    pub fn speed(&self) -> u32 {
        self.speed
    }

    /// Take a rewind snapshot every `frames` frames (0 turns rewind off and
    /// frees the history).
    pub fn set_rewind_interval(&mut self, frames: u32) {
        self.rewind_interval = frames;
        if frames == 0 {
            self.rewind.clear();
        }
    }

    pub fn rewind_available(&self) -> usize {
        self.rewind.len()
    }

    /// Start or stop holding the rewind button.
    pub fn set_rewinding(&mut self, on: bool) {
        if on == self.rewinding {
            return;
        }
        self.rewinding = on;
        self.rewind_tick = 0;
        if !on {
            // Resume exactly at the snapshot that is on screen, and keep it
            // in the history so the player can rewind again from here.
            if let Some(state) = self.rewind_cursor.take() {
                let _ = self.core.load_state(&state);
                self.rewind.push(state);
            }
            self.since_snapshot = 0;
        }
    }

    pub fn is_rewinding(&self) -> bool {
        self.rewinding
    }

    /// Replace the active cheats. `spec` holds records separated by
    /// [`CHEAT_RECORD_SEP`], each `format-id` [`CHEAT_FIELD_SEP`] `code`.
    /// Invalid cheats are skipped; their errors are returned.
    pub fn set_cheats(&mut self, spec: &str) -> Vec<String> {
        let mut errors = Vec::new();
        self.cheats = spec
            .split(CHEAT_RECORD_SEP)
            .filter(|r| !r.trim().is_empty())
            .filter_map(|r| {
                let (fmt, code) = r.split_once(CHEAT_FIELD_SEP)?;
                let Some(format) = CheatFormat::from_id(fmt.trim()) else {
                    errors.push(format!("unknown cheat format '{fmt}'"));
                    return None;
                };
                Cheat::parse(format, code).map_err(|e| errors.push(e)).ok()
            })
            .collect();
        errors
    }

    pub fn active_cheats(&self) -> usize {
        self.cheats.len()
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
            "system={}\ngpu={}\nhost_page_size={}\nsoc={} ({})\nclusters={}\narmv9={}\nthermal={:?}\nubershaders={}\ndsp={:?}\nmax_scale={}\nframe_ms={:.2}\nframes={}\nspeed={}x\nrewind_states={}\ncheats={}\n",
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
            self.stats.frames,
            self.speed,
            self.rewind.len(),
            self.cheats.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal valid GBA header; the entry point increments a counter in
    /// EWRAM every loop iteration (so rewind/cheat effects are visible).
    pub(crate) fn gba_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x200];
        let code: [u32; 6] = [
            0xE3A0_0402, // mov r0, #0x02000000
            0xE590_1000, // ldr r1, [r0]
            0xE281_1001, // add r1, r1, #1
            0xE580_1000, // str r1, [r0]
            0xEAFF_FFFB, // b (to ldr)
            0,
        ];
        // Header branch to 0xC0 (past the header).
        rom[0..4].copy_from_slice(&0xEA00_002Eu32.to_le_bytes());
        for (i, w) in code.iter().enumerate() {
            rom[0xC0 + i * 4..][..4].copy_from_slice(&w.to_le_bytes());
        }
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
    fn fast_forward_runs_more_guest_frames_with_steady_audio() {
        let mut s = Session::new(create_core("test.gba", gba_rom()).unwrap());
        let mut rx = s.take_audio_consumer().unwrap();
        s.step();
        let one = s.core.peek(0x0200_0000, 4);
        let mut buf = vec![StereoFrame::SILENCE; 1 << 16];
        rx.pop_into(&mut buf);
        s.set_speed(4);
        s.step();
        let four = s.core.peek(0x0200_0000, 4) - one;
        assert!(four > one * 3, "4x speed ran ~4 frames ({one} → {four} loop iterations)");
        let queued = rx.len();
        assert!((600..1200).contains(&queued), "audio stays ~1 frame per tick at 4x: {queued}");
        s.set_speed(99);
        assert_eq!(s.speed(), 8);
    }

    #[test]
    fn rewind_goes_back_and_resumes_from_the_shown_snapshot() {
        let mut s = Session::new(create_core("test.gba", gba_rom()).unwrap());
        s.set_rewind_interval(5);
        for _ in 0..40 {
            s.step();
        }
        assert_eq!(s.rewind_available(), 8);
        let before = s.core.peek(0x0200_0000, 4);
        s.set_rewinding(true);
        for _ in 0..7 {
            s.step();
        }
        let during = s.core.peek(0x0200_0000, 4);
        assert!(during < before, "counter went back: {before} → {during}");
        s.set_rewinding(false);
        let resumed = s.core.peek(0x0200_0000, 4);
        assert!(resumed <= during, "resumes at the snapshot, not one frame after it");
        s.step();
        assert!(s.core.peek(0x0200_0000, 4) > resumed, "runs forward again");
        s.set_rewind_interval(0);
        assert_eq!(s.rewind_available(), 0);
    }

    #[test]
    fn cheats_apply_every_frame_and_report_errors() {
        let mut s = Session::new(create_core("test.gba", gba_rom()).unwrap());
        let spec = format!(
            "raw{CHEAT_FIELD_SEP}03000100:BEEF{CHEAT_RECORD_SEP}bogus{CHEAT_FIELD_SEP}1{CHEAT_RECORD_SEP}raw{CHEAT_FIELD_SEP}nothex"
        );
        let errors = s.set_cheats(&spec);
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert_eq!(s.active_cheats(), 1);
        s.step();
        assert_eq!(s.core.peek(0x0300_0100, 2), 0xBEEF);
        s.core.poke(0x0300_0100, 0, 2);
        s.step();
        assert_eq!(s.core.peek(0x0300_0100, 2), 0xBEEF, "re-applied each frame");
        assert!(s.set_cheats("").is_empty());
        assert_eq!(s.active_cheats(), 0);
    }

    #[test]
    fn volume_scales_output() {
        let mut s = Session::new(create_core("test.gba", gba_rom()).unwrap());
        s.volume = 0;
        let mut rx = s.take_audio_consumer().unwrap();
        for _ in 0..3 {
            s.step();
        }
        let mut buf = vec![StereoFrame::new(1, 1); 2000];
        let n = rx.pop_into(&mut buf);
        assert!(buf[..n].iter().all(|f| *f == StereoFrame::SILENCE));
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

    #[test]
    fn game_info_reads_ds_banner_and_wbfs_header() {
        let mut rom = vec![0u8; 0x2000];
        rom[0..12].copy_from_slice(b"HOMEBREW\0\0\0\0");
        rom[0x0C..0x10].copy_from_slice(b"KDHB");
        rom[0x68..0x6C].copy_from_slice(&0x1000u32.to_le_bytes());
        rom[0x1000] = 1;
        rom[0x1222..0x1224].copy_from_slice(&0x7C00u16.to_le_bytes()); // blue
        rom[0x1020] = 0x11;
        let t: Vec<u16> = "Island Days\nKD".encode_utf16().collect();
        for (i, u) in t.iter().enumerate() {
            rom[0x1340 + i * 2..][..2].copy_from_slice(&u.to_le_bytes());
        }
        let mut read = |off: u64, buf: &mut [u8]| {
            let o = off as usize;
            rom.get(o..o + buf.len()).map(|s| buf.copy_from_slice(s)).is_some()
        };
        let info = game_info("island.nds", &mut read).unwrap();
        assert_eq!((info.system, info.title.as_str(), info.code.as_str()), (System::Nds, "Island Days", "KDHB"));
        let (w, h, px) = info.icon.unwrap();
        assert_eq!((w, h), (32, 32));
        assert_eq!(px[0], 0xFFFF_0000, "blue in RGBA byte order");
        assert!(!info.animal_crossing);

        // WBFS: header sector shift 9 → disc header copy at 512.
        let mut wbfs = vec![0u8; 0x800];
        wbfs[..4].copy_from_slice(b"WBFS");
        wbfs[8] = 9;
        wbfs[0x200..0x206].copy_from_slice(b"RUUE01");
        wbfs[0x218..0x21C].copy_from_slice(&WII_MAGIC.to_be_bytes());
        wbfs[0x220..0x22A].copy_from_slice(b"City Folk!");
        let mut read = |off: u64, buf: &mut [u8]| {
            let o = off as usize;
            wbfs.get(o..o + buf.len()).map(|s| buf.copy_from_slice(s)).is_some()
        };
        let info = game_info("cf.wbfs", &mut read).unwrap();
        assert_eq!(info.system, System::Wii);
        assert_eq!(info.code, "RUUE01");
        assert!(info.animal_crossing, "City Folk recognised");

        let tiny = gba_rom();
        let mut read = |off: u64, buf: &mut [u8]| {
            let o = off as usize;
            tiny.get(o..o + buf.len()).map(|s| buf.copy_from_slice(s)).is_some()
        };
        let info = game_info("tiny.gba", &mut read).unwrap();
        assert_eq!(info.title, "KDTEST");
    }
}
