//! Host memory facts the runtime reports (Android 15+ Pixels can run with
//! 16 KB pages; never assume 4 KB).

use std::sync::OnceLock;

/// Host MMU page size in bytes (4096 or 16384 on Android).
pub fn host_page_size() -> usize {
    static PAGE: OnceLock<usize> = OnceLock::new();
    *PAGE.get_or_init(|| {
        // SAFETY: sysconf has no preconditions.
        let v = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if v <= 0 {
            tracing::warn!("sysconf(_SC_PAGESIZE) failed, assuming 16 KiB");
            16384
        } else {
            v as usize
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_size_is_sane() {
        let p = host_page_size();
        assert!(p.is_power_of_two() && (4096..=65536).contains(&p), "{p}");
    }
}
