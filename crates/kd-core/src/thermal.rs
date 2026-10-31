//! ADPF thermal policy.
//!
//! The Kotlin `TensorThermalHintManager` forwards
//! `PowerManager.OnThermalStatusChangedListener` callbacks over JNI. GBA and
//! DS emulation is light, so the only mitigation is frame skipping once the
//! phone reports CRITICAL or worse.

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

#[derive(Debug, Clone, Default)]
pub struct ThermalPolicy {
    status: ThermalStatus,
}

impl ThermalPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn status(&self) -> ThermalStatus {
        self.status
    }

    /// Returns `true` if the frame-skip setting changed.
    pub fn on_status_changed(&mut self, status: ThermalStatus) -> bool {
        let before = self.frameskip();
        self.status = status;
        before != self.frameskip()
    }

    /// Frames to skip rendering between drawn frames (emergencies only).
    pub fn frameskip(&self) -> u8 {
        match self.status {
            ThermalStatus::Critical => 1,
            s if s >= ThermalStatus::Emergency => 2,
            _ => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn critical_enables_frameskip() {
        let mut p = ThermalPolicy::new();
        assert!(!p.on_status_changed(ThermalStatus::Moderate));
        assert!(p.on_status_changed(ThermalStatus::Critical));
        assert_eq!(p.frameskip(), 1);
        p.on_status_changed(ThermalStatus::Emergency);
        assert_eq!(p.frameskip(), 2);
        p.on_status_changed(ThermalStatus::None);
        assert_eq!(p.frameskip(), 0);
    }

    #[test]
    fn android_status_mapping() {
        assert_eq!(ThermalStatus::from_android(2), ThermalStatus::Moderate);
        assert_eq!(ThermalStatus::from_android(-1), ThermalStatus::None);
        assert_eq!(ThermalStatus::from_android(42), ThermalStatus::Shutdown);
    }
}
