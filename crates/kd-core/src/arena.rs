//! 16 KB-safe guest memory and JIT code arenas.
//!
//! Tensor G5 (Pixel 10, Android 15+) runs a 16 KB page-size kernel while the
//! guests (GameCube/Wii PowerPC, NDS ARM9) think in 4 KB pages. We therefore
//! never hardcode `4096`: the host page size is queried once with
//! `sysconf(_SC_PAGESIZE)` and every `mmap`/`mprotect`/`madvise`/`memfd`
//! size is rounded to it. Fine-grained 4 KB guest protections are handled
//! by the software TLB in `cpu-ppc`, not by host `mprotect`.
//!
//! [`JitArena`] maps one `memfd` twice: a `PROT_READ|PROT_WRITE` view used by
//! the code emitter and a `PROT_READ|PROT_EXEC` view that is executed. No
//! page is ever writable and executable at once, which keeps Android's
//! SELinux `execmem` (W^X) policy happy.

use std::ffi::CStr;
use std::io;
use std::ptr::NonNull;
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

/// Round `n` up to a multiple of `align` (a power of two).
#[inline]
pub const fn align_up(n: usize, align: usize) -> usize {
    (n + align - 1) & !(align - 1)
}

/// Round `n` down to a multiple of `align` (a power of two).
#[inline]
pub const fn align_down(n: usize, align: usize) -> usize {
    n & !(align - 1)
}

/// Round `n` up to the host page size.
#[inline]
pub fn page_round_up(n: usize) -> usize {
    align_up(n, host_page_size())
}

fn memfd(name: &CStr, len: usize) -> io::Result<libc::c_int> {
    // SAFETY: valid NUL-terminated name, flags are a constant.
    let fd = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd is a fresh memfd we own.
    if unsafe { libc::ftruncate(fd, len as libc::off_t) } != 0 {
        let e = io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(e);
    }
    Ok(fd)
}

fn map(fd: libc::c_int, len: usize, prot: libc::c_int) -> io::Result<NonNull<u8>> {
    // SAFETY: mapping a memfd we own with a page-multiple length.
    let p = unsafe { libc::mmap(std::ptr::null_mut(), len, prot, libc::MAP_SHARED, fd, 0) };
    if p == libc::MAP_FAILED {
        return Err(io::Error::last_os_error());
    }
    Ok(NonNull::new(p as *mut u8).expect("mmap returned null"))
}

/// A zero-initialised, host-page-aligned block of guest RAM backed by a
/// `memfd`, e.g. GameCube MEM1 (24 MiB) or Wii MEM2 (64 MiB).
pub struct PageAwareArena {
    fd: libc::c_int,
    base: NonNull<u8>,
    len: usize,
    requested: usize,
}

// SAFETY: the arena owns its mapping; access is through &self/&mut self.
unsafe impl Send for PageAwareArena {}
unsafe impl Sync for PageAwareArena {}

impl PageAwareArena {
    /// Allocate at least `size` bytes, rounded up to the host page size.
    pub fn new(name: &CStr, size: usize) -> io::Result<Self> {
        let len = page_round_up(size.max(1));
        let fd = memfd(name, len)?;
        let base = match map(fd, len, libc::PROT_READ | libc::PROT_WRITE) {
            Ok(b) => b,
            Err(e) => {
                unsafe { libc::close(fd) };
                return Err(e);
            }
        };
        Ok(Self { fd, base, len, requested: size })
    }

    /// Mapped length (a multiple of [`host_page_size`]).
    pub fn mapped_len(&self) -> usize {
        self.len
    }

    /// Size the caller asked for.
    pub fn len(&self) -> usize {
        self.requested
    }

    pub fn is_empty(&self) -> bool {
        self.requested == 0
    }

    pub fn as_ptr(&self) -> *const u8 {
        self.base.as_ptr()
    }

    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.base.as_ptr()
    }

    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: mapping is valid for `requested` bytes while self lives.
        unsafe { std::slice::from_raw_parts(self.base.as_ptr(), self.requested) }
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: as above, uniquely borrowed.
        unsafe { std::slice::from_raw_parts_mut(self.base.as_ptr(), self.requested) }
    }

    /// The backing descriptor, for mirroring the same RAM at several guest
    /// addresses (e.g. 0x80000000 cached / 0xC0000000 uncached views).
    pub fn fd(&self) -> libc::c_int {
        self.fd
    }

    /// Zero `[offset, offset+len)`. Whole host pages are returned to the
    /// kernel with `MADV_DONTNEED` (so ZRAM sees clean pages); the unaligned
    /// head and tail are cleared by hand.
    pub fn discard(&mut self, offset: usize, len: usize) {
        let end = (offset + len).min(self.requested);
        if offset >= end {
            return;
        }
        let page = host_page_size();
        let first_full = align_up(offset, page);
        let last_full = align_down(end, page);
        let slice = self.as_mut_slice();
        if first_full >= last_full {
            slice[offset..end].fill(0);
            return;
        }
        slice[offset..first_full].fill(0);
        slice[last_full..end].fill(0);
        // SAFETY: page-aligned sub-range of our own shared mapping. For
        // MAP_SHARED memfd mappings MADV_DONTNEED alone does not zero the
        // backing file, so punch a hole as well.
        unsafe {
            let p = self.base.as_ptr().add(first_full);
            libc::fallocate(
                self.fd,
                libc::FALLOC_FL_PUNCH_HOLE | libc::FALLOC_FL_KEEP_SIZE,
                first_full as libc::off_t,
                (last_full - first_full) as libc::off_t,
            );
            libc::madvise(p as *mut libc::c_void, last_full - first_full, libc::MADV_DONTNEED);
        }
    }

    /// Map another view of the same memory (used for mirrors).
    pub fn mirror(&self) -> io::Result<MirrorView> {
        let base = map(self.fd, self.len, libc::PROT_READ | libc::PROT_WRITE)?;
        Ok(MirrorView { base, len: self.len })
    }
}

impl Drop for PageAwareArena {
    fn drop(&mut self) {
        // SAFETY: unmapping our own mapping and closing our own fd.
        unsafe {
            libc::munmap(self.base.as_ptr() as *mut libc::c_void, self.len);
            libc::close(self.fd);
        }
    }
}

/// An additional mapping of a [`PageAwareArena`].
pub struct MirrorView {
    base: NonNull<u8>,
    len: usize,
}

unsafe impl Send for MirrorView {}

impl MirrorView {
    pub fn as_ptr(&self) -> *mut u8 {
        self.base.as_ptr()
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl Drop for MirrorView {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.base.as_ptr() as *mut libc::c_void, self.len) };
    }
}

/// Dual-mapped W^X code cache for the AArch64 JIT.
pub struct JitArena {
    fd: libc::c_int,
    rw: NonNull<u8>,
    rx: NonNull<u8>,
    len: usize,
    used: usize,
}

unsafe impl Send for JitArena {}

impl JitArena {
    pub fn new(size: usize) -> io::Result<Self> {
        let len = page_round_up(size.max(1));
        let fd = memfd(c"kd_emulator_jit", len)?;
        let rw = map(fd, len, libc::PROT_READ | libc::PROT_WRITE);
        let rx = map(fd, len, libc::PROT_READ | libc::PROT_EXEC);
        match (rw, rx) {
            (Ok(rw), Ok(rx)) => Ok(Self { fd, rw, rx, len, used: 0 }),
            (rw, rx) => {
                unsafe {
                    if let Ok(p) = rw {
                        libc::munmap(p.as_ptr() as *mut libc::c_void, len);
                    }
                    if let Ok(p) = rx {
                        libc::munmap(p.as_ptr() as *mut libc::c_void, len);
                    }
                    libc::close(fd);
                }
                Err(io::Error::last_os_error())
            }
        }
    }

    pub fn capacity(&self) -> usize {
        self.len
    }

    pub fn used(&self) -> usize {
        self.used
    }

    pub fn rw_base(&self) -> *mut u8 {
        self.rw.as_ptr()
    }

    pub fn rx_base(&self) -> *const u8 {
        self.rx.as_ptr()
    }

    /// Copy `code` into the cache (aligned to `align`), flush the
    /// instruction cache for it and return its executable address.
    pub fn emit(&mut self, code: &[u8], align: usize) -> Option<*const u8> {
        let start = align_up(self.used, align.max(1));
        let end = start.checked_add(code.len())?;
        if end > self.len {
            return None;
        }
        // SAFETY: [start, end) is inside the RW mapping.
        unsafe {
            std::ptr::copy_nonoverlapping(code.as_ptr(), self.rw.as_ptr().add(start), code.len());
            let rx = self.rx.as_ptr().add(start);
            flush_icache(rx, code.len());
            self.used = end;
            Some(rx)
        }
    }

    /// Throw away all compiled code (e.g. on GameCube `icbi` storms) and
    /// return the pages to the kernel.
    pub fn reset(&mut self) {
        unsafe {
            libc::fallocate(
                self.fd,
                libc::FALLOC_FL_PUNCH_HOLE | libc::FALLOC_FL_KEEP_SIZE,
                0,
                self.len as libc::off_t,
            );
            libc::madvise(self.rw.as_ptr() as *mut libc::c_void, self.len, libc::MADV_DONTNEED);
        }
        self.used = 0;
    }
}

impl Drop for JitArena {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.rw.as_ptr() as *mut libc::c_void, self.len);
            libc::munmap(self.rx.as_ptr() as *mut libc::c_void, self.len);
            libc::close(self.fd);
        }
    }
}

/// Make freshly written code at `ptr..ptr+len` visible to instruction fetch.
///
/// On AArch64 this is the architectural sequence: `dc cvau` each D-cache line,
/// `dsb ish`, `ic ivau` each I-cache line, `dsb ish`, `isb`. Line sizes come
/// from `CTR_EL0` (readable from EL0 on Linux/Android).
///
/// # Safety
/// `ptr..ptr+len` must be mapped.
#[allow(unused_variables)]
pub unsafe fn flush_icache(ptr: *const u8, len: usize) {
    #[cfg(target_arch = "aarch64")]
    {
        let ctr: u64;
        core::arch::asm!("mrs {}, ctr_el0", out(reg) ctr, options(nomem, nostack));
        let dline = 4usize << ((ctr >> 16) & 0xF);
        let iline = 4usize << (ctr & 0xF);
        let start = ptr as usize;
        let end = start + len;
        let mut a = start & !(dline - 1);
        while a < end {
            core::arch::asm!("dc cvau, {}", in(reg) a, options(nostack));
            a += dline;
        }
        core::arch::asm!("dsb ish", options(nostack));
        let mut a = start & !(iline - 1);
        while a < end {
            core::arch::asm!("ic ivau, {}", in(reg) a, options(nostack));
            a += iline;
        }
        core::arch::asm!("dsb ish", "isb", options(nostack));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_size_is_sane() {
        let p = host_page_size();
        assert!(p.is_power_of_two());
        assert!(p == 4096 || p == 16384 || p == 65536, "unexpected page size {p}");
        assert_eq!(page_round_up(1), p);
        assert_eq!(page_round_up(p), p);
        assert_eq!(page_round_up(p + 1), 2 * p);
    }

    #[test]
    fn arena_is_zeroed_and_rounded() {
        let mut a = PageAwareArena::new(c"kd_test_mem1", 24 * 1024 * 1024 + 3).unwrap();
        assert_eq!(a.len(), 24 * 1024 * 1024 + 3);
        assert_eq!(a.mapped_len() % host_page_size(), 0);
        assert!(a.as_slice().iter().step_by(4093).all(|&b| b == 0));
        a.as_mut_slice()[100] = 0xAB;
        assert_eq!(a.as_slice()[100], 0xAB);
    }

    #[test]
    fn mirror_views_alias() {
        let mut a = PageAwareArena::new(c"kd_test_mirror", 64 * 1024).unwrap();
        let m = a.mirror().unwrap();
        a.as_mut_slice()[0x1234] = 0x5A;
        assert_eq!(unsafe { *m.as_ptr().add(0x1234) }, 0x5A);
        unsafe { *m.as_ptr().add(0x10) = 0x77 };
        assert_eq!(a.as_slice()[0x10], 0x77);
    }

    #[test]
    fn discard_zeroes_partial_and_full_pages() {
        let page = host_page_size();
        let mut a = PageAwareArena::new(c"kd_test_discard", page * 4).unwrap();
        a.as_mut_slice().fill(0xFF);
        a.discard(10, page * 3);
        let s = a.as_slice();
        assert!(s[..10].iter().all(|&b| b == 0xFF));
        assert!(s[10..10 + page * 3].iter().all(|&b| b == 0));
        assert!(s[10 + page * 3..].iter().all(|&b| b == 0xFF));
    }

    #[test]
    fn jit_arena_is_wx_and_executes() {
        let mut jit = JitArena::new(1).unwrap();
        assert_eq!(jit.capacity(), host_page_size());
        assert_ne!(jit.rw_base() as usize, jit.rx_base() as usize);

        #[cfg(target_arch = "x86_64")]
        let code: &[u8] = &[0xB8, 42, 0, 0, 0, 0xC3]; // mov eax, 42 ; ret
        #[cfg(target_arch = "aarch64")]
        let code: &[u8] = &[0x40, 0x05, 0x80, 0x52, 0xC0, 0x03, 0x5F, 0xD6]; // mov w0,#42 ; ret

        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        {
            let entry = jit.emit(code, 16).unwrap();
            let f: extern "C" fn() -> u32 = unsafe { std::mem::transmute(entry) };
            assert_eq!(f(), 42);
            jit.reset();
            assert_eq!(jit.used(), 0);
        }
    }

    #[test]
    fn jit_arena_rejects_overflow() {
        let mut jit = JitArena::new(1).unwrap();
        let big = vec![0u8; jit.capacity() + 1];
        assert!(jit.emit(&big, 4).is_none());
    }
}
