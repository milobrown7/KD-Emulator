//! GPU vendor profiles and ADPF thermal policy.
//!
//! The Kotlin `TensorThermalHintManager` forwards
//! `PowerManager.OnThermalStatusChangedListener` callbacks over JNI; this
//! module turns the status into concrete emulation knobs. At
//! `THERMAL_STATUS_MODERATE` and above we switch to asynchronous uber-shaders,
//! drop the GameCube/Wii DSP to HLE and cap internal resolution so the SoC
//! settles instead of collapsing into heavy throttling.

/// Vulkan `VkPhysicalDeviceProperties::vendorID` values we special-case.
pub const VENDOR_IMAGINATION: u32 = 0x1010;
pub const VENDOR_ARM: u32 = 0x13B5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuVendorProfile {
    /// Imagination PowerVR DXT-48-1536 (Tensor G5).
    PowerVrDxt,
    /// ARM Mali-G78 / G710 / G715 (Tensor G1–G4).
    ArmMali,
    /// Anything else (desktop hosts, emulators, other SoCs).
    Generic,
}

impl GpuVendorProfile {
    pub fn from_vendor_id(vendor_id: u32) -> Self {
        match vendor_id {
            VENDOR_IMAGINATION => Self::PowerVrDxt,
            VENDOR_ARM => Self::ArmMali,
            _ => Self::Generic,
        }
    }

    /// Compute workgroup width for texture decoders / EFB copies:
    /// PowerVR executes 32-wide USSE task clusters, Mali Valhall 64-wide warps
    /// (expressed as 64 invocations to match its 16-wide x4 quad engines).
    pub fn compute_local_size_x(self) -> u32 {
        match self {
            Self::PowerVrDxt => 32,
            Self::ArmMali | Self::Generic => 64,
        }
    }

    /// PowerVR DXT drivers mis-handle MSAA: we use SSAA via resolution
    /// scaling instead.
    pub fn allows_msaa(self) -> bool {
        !matches!(self, Self::PowerVrDxt)
    }

    /// Whether `OpKill`/`discard` must be avoided whenever alpha test cannot
    /// fail (preserves PowerVR Hidden Surface Removal).
    pub fn must_preserve_hsr(self) -> bool {
        matches!(self, Self::PowerVrDxt)
    }

    /// Mali benefits from keeping EFB depth/colour reads in tile memory via
    /// transient attachments and subpass inputs.
    pub fn prefers_subpass_efb(self) -> bool {
        matches!(self, Self::ArmMali)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::PowerVrDxt => "PowerVR DXT (Tensor G5)",
            Self::ArmMali => "ARM Mali (Tensor G1–G4)",
            Self::Generic => "Generic Vulkan",
        }
    }
}

/// Mirrors `android.os.PowerManager.THERMAL_STATUS_*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum ThermalStatus {
    #[default]
    None = 0,
    Light = 1,
    Moderate = 2,
    Severe = 3,
    Critical = 4,
    Emergency = 5,
    Shutdown = 6,
}

impl ThermalStatus {
    pub fn from_android(v: i32) -> Self {
        match v {
            i32::MIN..=0 => Self::None,
            1 => Self::Light,
            2 => Self::Moderate,
            3 => Self::Severe,
            4 => Self::Critical,
            5 => Self::Emergency,
            _ => Self::Shutdown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DspMode {
    /// Low-level: interpret the Macronix DSP microcode.
    Lle,
    /// High-level: emulate the AX/Zelda ucode behaviour directly.
    Hle,
}

/// Knobs the cores and renderer read every frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PerformanceKnobs {
    pub async_ubershaders: bool,
    pub dsp_mode: DspMode,
    /// Maximum internal EFB scale (1 = native 640x528).
    pub max_internal_scale: u8,
    /// Frame skip allowed (only in emergencies).
    pub frameskip: u8,
}

/// User preferences that the thermal policy may only ever reduce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserGraphicsPrefs {
    pub internal_scale: u8,
    pub dsp_lle: bool,
    pub async_ubershaders: bool,
}

impl Default for UserGraphicsPrefs {
    fn default() -> Self {
        Self { internal_scale: 2, dsp_lle: false, async_ubershaders: false }
    }
}

#[derive(Debug, Clone)]
pub struct ThermalPolicy {
    status: ThermalStatus,
    prefs: UserGraphicsPrefs,
}

impl ThermalPolicy {
    pub fn new(prefs: UserGraphicsPrefs) -> Self {
        Self { status: ThermalStatus::None, prefs }
    }

    pub fn status(&self) -> ThermalStatus {
        self.status
    }

    /// Returns `true` if the knobs changed.
    pub fn on_status_changed(&mut self, status: ThermalStatus) -> bool {
        let before = self.knobs();
        self.status = status;
        before != self.knobs()
    }

    pub fn set_prefs(&mut self, prefs: UserGraphicsPrefs) {
        self.prefs = prefs;
    }

    pub fn knobs(&self) -> PerformanceKnobs {
        let p = self.prefs;
        let scale_cap = match self.status {
            ThermalStatus::None | ThermalStatus::Light => 4,
            ThermalStatus::Moderate => 2,
            ThermalStatus::Severe => 1,
            _ => 1,
        };
        let hot = self.status >= ThermalStatus::Moderate;
        PerformanceKnobs {
            async_ubershaders: p.async_ubershaders || hot,
            dsp_mode: if p.dsp_lle && !hot { DspMode::Lle } else { DspMode::Hle },
            max_internal_scale: p.internal_scale.clamp(1, 4).min(scale_cap),
            frameskip: match self.status {
                ThermalStatus::Critical => 1,
                s if s >= ThermalStatus::Emergency => 2,
                _ => 0,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendor_profiles() {
        let pvr = GpuVendorProfile::from_vendor_id(0x1010);
        assert_eq!(pvr, GpuVendorProfile::PowerVrDxt);
        assert_eq!(pvr.compute_local_size_x(), 32);
        assert!(!pvr.allows_msaa());
        assert!(pvr.must_preserve_hsr());
        let mali = GpuVendorProfile::from_vendor_id(0x13B5);
        assert_eq!(mali.compute_local_size_x(), 64);
        assert!(mali.prefers_subpass_efb());
        assert_eq!(GpuVendorProfile::from_vendor_id(0x10DE), GpuVendorProfile::Generic);
    }

    #[test]
    fn moderate_enables_mitigations() {
        let mut p =
            ThermalPolicy::new(UserGraphicsPrefs { internal_scale: 3, dsp_lle: true, async_ubershaders: false });
        let cool = p.knobs();
        assert_eq!(cool.dsp_mode, DspMode::Lle);
        assert!(!cool.async_ubershaders);
        assert_eq!(cool.max_internal_scale, 3);
        assert!(!p.on_status_changed(ThermalStatus::Light));
        assert!(p.on_status_changed(ThermalStatus::Moderate));
        let warm = p.knobs();
        assert_eq!(warm.dsp_mode, DspMode::Hle);
        assert!(warm.async_ubershaders);
        assert_eq!(warm.max_internal_scale, 2);
        p.on_status_changed(ThermalStatus::Severe);
        assert_eq!(p.knobs().max_internal_scale, 1);
        p.on_status_changed(ThermalStatus::None);
        assert_eq!(p.knobs(), cool);
    }

    #[test]
    fn android_status_mapping() {
        assert_eq!(ThermalStatus::from_android(2), ThermalStatus::Moderate);
        assert_eq!(ThermalStatus::from_android(-1), ThermalStatus::None);
        assert_eq!(ThermalStatus::from_android(42), ThermalStatus::Shutdown);
    }
}
