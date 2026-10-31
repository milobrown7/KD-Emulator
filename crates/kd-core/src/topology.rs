//! Google Tensor CPU topology detection and thread pinning.
//!
//! Cores are classified purely from `cpuinfo_max_freq`, so the same code
//! handles every Tensor generation:
//!
//! | SoC | Layout | Prime | Mid | Little |
//! |-----|--------|-------|-----|--------|
//! | G1 / G2 | 2+2+4 | Cortex-X1 | A76 / A78 | A55 |
//! | G3 | 1+4+4 | Cortex-X3 | A715 | A510 |
//! | G4 | 1+3+4 | Cortex-X4 | A720 | A520 |
//! | G5 | 1+5+2 | Cortex-X4 @ 3.78 GHz (cpu7) | A725 @ 3.05 GHz (cpu2..6) | A520 @ 2.25 GHz (cpu0..1) |
//!
//! The primary CPU/JIT thread is pinned to Prime|Mid, the Vulkan worker and
//! the audio/DSP/ARM7 thread to Mid, and Little cores are always excluded.

use std::fmt;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ClusterKind {
    Little,
    Mid,
    Prime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cluster {
    pub kind: ClusterKind,
    pub max_freq_khz: u64,
    pub cpus: Vec<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TensorGeneration {
    G1G2,
    G3,
    G4,
    G5,
    Unknown,
}

impl fmt::Display for TensorGeneration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            TensorGeneration::G1G2 => "Tensor G1/G2",
            TensorGeneration::G3 => "Tensor G3",
            TensorGeneration::G4 => "Tensor G4",
            TensorGeneration::G5 => "Tensor G5",
            TensorGeneration::Unknown => "Unknown SoC",
        })
    }
}

/// Which emulator thread is being pinned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadRole {
    /// Main guest CPU interpreter / JIT thread.
    EmuCpu,
    /// Vulkan command recording / submission worker.
    GpuWorker,
    /// Audio mixing, GameCube/Wii DSP, NDS ARM7.
    AudioDsp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuTopology {
    /// Clusters sorted Little → Prime. Missing kinds are simply absent.
    pub clusters: Vec<Cluster>,
}

impl CpuTopology {
    /// Read `/sys/devices/system/cpu`.
    pub fn detect() -> Self {
        Self::from_sysfs(Path::new("/sys/devices/system/cpu"))
    }

    /// Parse a sysfs-like tree (`cpuN/cpufreq/cpuinfo_max_freq`). Unreadable
    /// CPUs (offline, or hidden by SELinux) are skipped.
    pub fn from_sysfs(root: &Path) -> Self {
        let mut cores = Vec::new();
        if let Ok(entries) = fs::read_dir(root) {
            for e in entries.flatten() {
                let name = e.file_name();
                let name = name.to_string_lossy();
                let Some(idx) = name.strip_prefix("cpu").and_then(|n| n.parse::<usize>().ok()) else {
                    continue;
                };
                let f = e.path().join("cpufreq/cpuinfo_max_freq");
                if let Some(khz) = fs::read_to_string(f).ok().and_then(|s| s.trim().parse::<u64>().ok()) {
                    cores.push((idx, khz));
                }
            }
        }
        Self::from_frequencies(&cores)
    }

    /// Build from `(cpu index, max kHz)` pairs.
    pub fn from_frequencies(cores: &[(usize, u64)]) -> Self {
        let mut freqs: Vec<u64> = cores.iter().map(|&(_, f)| f).collect();
        freqs.sort_unstable();
        freqs.dedup();
        let kinds: Vec<ClusterKind> = match freqs.len() {
            0 => vec![],
            1 => vec![ClusterKind::Mid],
            2 => vec![ClusterKind::Little, ClusterKind::Mid],
            n => {
                let mut v = vec![ClusterKind::Little];
                v.extend(std::iter::repeat_n(ClusterKind::Mid, n - 2));
                v.push(ClusterKind::Prime);
                v
            }
        };
        let mut clusters: Vec<Cluster> = Vec::new();
        for (freq, kind) in freqs.iter().zip(kinds) {
            let mut cpus: Vec<usize> = cores.iter().filter(|&&(_, f)| f == *freq).map(|&(i, _)| i).collect();
            cpus.sort_unstable();
            // Extra middle tiers (rare) merge into one Mid cluster.
            if let Some(c) = clusters.iter_mut().find(|c| c.kind == kind) {
                c.cpus.extend(cpus);
                c.cpus.sort_unstable();
                c.max_freq_khz = c.max_freq_khz.max(*freq);
            } else {
                clusters.push(Cluster { kind, max_freq_khz: *freq, cpus });
            }
        }
        Self { clusters }
    }

    pub fn cluster(&self, kind: ClusterKind) -> Option<&Cluster> {
        self.clusters.iter().find(|c| c.kind == kind)
    }

    fn count(&self, kind: ClusterKind) -> usize {
        self.cluster(kind).map_or(0, |c| c.cpus.len())
    }

    /// Layout string such as `"1+5+2"` (Prime+Mid+Little).
    pub fn layout(&self) -> String {
        format!(
            "{}+{}+{}",
            self.count(ClusterKind::Prime),
            self.count(ClusterKind::Mid),
            self.count(ClusterKind::Little)
        )
    }

    pub fn generation(&self) -> TensorGeneration {
        let prime_mhz = self.cluster(ClusterKind::Prime).map_or(0, |c| c.max_freq_khz / 1000);
        match self.layout().as_str() {
            "1+5+2" => TensorGeneration::G5,
            "1+3+4" => TensorGeneration::G4,
            "1+4+4" => TensorGeneration::G3,
            "2+2+4" if prime_mhz < 3200 => TensorGeneration::G1G2,
            _ => TensorGeneration::Unknown,
        }
    }

    /// CPUs a thread with `role` may run on. Never empty: if the preferred
    /// clusters are missing (emulator on a homogeneous host) all CPUs that
    /// are not Little are used, and failing that every CPU.
    pub fn cpus_for(&self, role: ThreadRole) -> Vec<usize> {
        let wanted: &[ClusterKind] = match role {
            ThreadRole::EmuCpu => &[ClusterKind::Prime, ClusterKind::Mid],
            ThreadRole::GpuWorker | ThreadRole::AudioDsp => &[ClusterKind::Mid],
        };
        let mut cpus: Vec<usize> = self
            .clusters
            .iter()
            .filter(|c| wanted.contains(&c.kind))
            .flat_map(|c| c.cpus.iter().copied())
            .collect();
        if cpus.is_empty() {
            cpus = self
                .clusters
                .iter()
                .filter(|c| c.kind != ClusterKind::Little)
                .flat_map(|c| c.cpus.iter().copied())
                .collect();
        }
        if cpus.is_empty() {
            cpus = self.clusters.iter().flat_map(|c| c.cpus.iter().copied()).collect();
        }
        cpus.sort_unstable();
        cpus
    }

    /// Pin the calling thread according to `role`.
    pub fn pin_current_thread(&self, role: ThreadRole) -> std::io::Result<Vec<usize>> {
        let cpus = self.cpus_for(role);
        if cpus.is_empty() {
            return Ok(cpus);
        }
        // SAFETY: cpu_set_t is plain data; sched_setaffinity(0, ..) targets
        // the calling thread.
        unsafe {
            let mut set: libc::cpu_set_t = std::mem::zeroed();
            for &c in &cpus {
                libc::CPU_SET(c, &mut set);
            }
            if libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set) != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(cpus)
    }
}

/// AArch64 ISA extensions relevant to the JIT and NEON fast paths, read with
/// `getauxval(AT_HWCAP/AT_HWCAP2)`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CpuFeatures {
    pub asimd: bool,
    pub dotprod: bool,
    pub fp16: bool,
    pub i8mm: bool,
    pub sve2: bool,
    pub bf16: bool,
}

/// Linux arm64 `AT_HWCAP` / `AT_HWCAP2` bits (uapi/asm/hwcap.h).
pub mod hwcap {
    pub const HWCAP_ASIMD: u64 = 1 << 1;
    pub const HWCAP_FPHP: u64 = 1 << 9;
    pub const HWCAP_ASIMDHP: u64 = 1 << 10;
    pub const HWCAP_ASIMDDP: u64 = 1 << 20;
    pub const HWCAP2_SVE2: u64 = 1 << 1;
    pub const HWCAP2_I8MM: u64 = 1 << 13;
    pub const HWCAP2_BF16: u64 = 1 << 14;
}

impl CpuFeatures {
    pub fn from_hwcaps(hwcap: u64, hwcap2: u64) -> Self {
        use hwcap::*;
        Self {
            asimd: hwcap & HWCAP_ASIMD != 0,
            dotprod: hwcap & HWCAP_ASIMDDP != 0,
            fp16: hwcap & (HWCAP_FPHP | HWCAP_ASIMDHP) == (HWCAP_FPHP | HWCAP_ASIMDHP),
            i8mm: hwcap2 & HWCAP2_I8MM != 0,
            sve2: hwcap2 & HWCAP2_SVE2 != 0,
            bf16: hwcap2 & HWCAP2_BF16 != 0,
        }
    }

    pub fn detect() -> Self {
        #[cfg(all(target_arch = "aarch64", any(target_os = "linux", target_os = "android")))]
        {
            // SAFETY: getauxval has no preconditions.
            let (h1, h2) = unsafe { (libc::getauxval(libc::AT_HWCAP), libc::getauxval(libc::AT_HWCAP2)) };
            Self::from_hwcaps(h1 as u64, h2 as u64)
        }
        #[cfg(not(all(target_arch = "aarch64", any(target_os = "linux", target_os = "android"))))]
        {
            Self::default()
        }
    }

    /// ARMv9.2-A class core (Tensor G5): SVE2 + i8mm + dotprod.
    pub fn is_armv9_class(&self) -> bool {
        self.sve2 && self.i8mm && self.dotprod
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g5() -> CpuTopology {
        let mut v = vec![(0, 2_253_000), (1, 2_253_000)];
        v.extend((2..=6).map(|i| (i, 3_053_000)));
        v.push((7, 3_782_000));
        CpuTopology::from_frequencies(&v)
    }

    #[test]
    fn classifies_tensor_g5() {
        let t = g5();
        assert_eq!(t.layout(), "1+5+2");
        assert_eq!(t.generation(), TensorGeneration::G5);
        assert_eq!(t.cluster(ClusterKind::Prime).unwrap().cpus, vec![7]);
        assert_eq!(t.cluster(ClusterKind::Mid).unwrap().cpus, vec![2, 3, 4, 5, 6]);
        assert_eq!(t.cluster(ClusterKind::Little).unwrap().cpus, vec![0, 1]);
        assert_eq!(t.cpus_for(ThreadRole::EmuCpu), vec![2, 3, 4, 5, 6, 7]);
        assert_eq!(t.cpus_for(ThreadRole::GpuWorker), vec![2, 3, 4, 5, 6]);
        assert_eq!(t.cpus_for(ThreadRole::AudioDsp), vec![2, 3, 4, 5, 6]);
    }

    #[test]
    fn classifies_g1_and_g4() {
        let g1: Vec<_> = (0..4)
            .map(|i| (i, 1_803_000))
            .chain((4..6).map(|i| (i, 2_253_000)))
            .chain((6..8).map(|i| (i, 2_802_000)))
            .collect();
        let t = CpuTopology::from_frequencies(&g1);
        assert_eq!(t.layout(), "2+2+4");
        assert_eq!(t.generation(), TensorGeneration::G1G2);
        assert!(!t.cpus_for(ThreadRole::EmuCpu).iter().any(|c| *c < 4));

        let g4: Vec<_> = (0..4)
            .map(|i| (i, 1_950_000))
            .chain((4..7).map(|i| (i, 2_600_000)))
            .chain(std::iter::once((7, 3_105_000)))
            .collect();
        let t = CpuTopology::from_frequencies(&g4);
        assert_eq!(t.layout(), "1+3+4");
        assert_eq!(t.generation(), TensorGeneration::G4);
    }

    #[test]
    fn homogeneous_host_falls_back() {
        let t = CpuTopology::from_frequencies(&[(0, 3_000_000), (1, 3_000_000)]);
        assert_eq!(t.layout(), "0+2+0");
        assert_eq!(t.cpus_for(ThreadRole::EmuCpu), vec![0, 1]);
        assert_eq!(t.cpus_for(ThreadRole::GpuWorker), vec![0, 1]);
        let empty = CpuTopology::from_frequencies(&[]);
        assert!(empty.cpus_for(ThreadRole::EmuCpu).is_empty());
    }

    #[test]
    fn parses_fake_sysfs() {
        let dir = std::env::temp_dir().join(format!("kd_sysfs_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for (i, f) in [(0, 2_253_000), (1, 2_253_000), (2, 3_053_000), (7, 3_782_000)] {
            let p = dir.join(format!("cpu{i}/cpufreq"));
            fs::create_dir_all(&p).unwrap();
            fs::write(p.join("cpuinfo_max_freq"), format!("{f}\n")).unwrap();
        }
        fs::create_dir_all(dir.join("cpufreq")).unwrap(); // not a cpuN dir
        fs::create_dir_all(dir.join("cpu5")).unwrap(); // offline: no cpufreq
        let t = CpuTopology::from_sysfs(&dir);
        assert_eq!(t.layout(), "1+1+2");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn pinning_current_thread_works_on_host() {
        let t = CpuTopology::detect();
        if t.clusters.is_empty() {
            return; // container without cpufreq
        }
        let pinned = t.pin_current_thread(ThreadRole::EmuCpu).unwrap();
        assert!(!pinned.is_empty());
    }

    #[test]
    fn hwcap_decoding() {
        use hwcap::*;
        let f = CpuFeatures::from_hwcaps(
            HWCAP_ASIMD | HWCAP_ASIMDDP | HWCAP_FPHP | HWCAP_ASIMDHP,
            HWCAP2_SVE2 | HWCAP2_I8MM,
        );
        assert!(f.asimd && f.dotprod && f.fp16 && f.i8mm && f.sve2 && !f.bf16);
        assert!(f.is_armv9_class());
        assert!(!CpuFeatures::from_hwcaps(HWCAP_ASIMD, 0).is_armv9_class());
    }
}
