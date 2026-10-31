//! Renderer profile derived from the Vulkan vendor ID and device limits,
//! and the on-disk `VkPipelineCache` container.

use kd_core::savestate::crc32;
use kd_core::thermal::GpuVendorProfile;

/// Rendering decisions that differ between PowerVR DXT (Tensor G5) and
/// ARM Mali Valhall (Tensor G1–G4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RendererProfile {
    pub vendor: GpuVendorProfile,
    /// `VkPipelineMultisampleStateCreateInfo::rasterizationSamples`.
    pub samples: u32,
    pub compute_local_size_x: u32,
    /// EFB depth/colour as `VK_IMAGE_USAGE_TRANSIENT_ATTACHMENT_BIT` with
    /// subpass input reads (Mali tile memory).
    pub transient_efb: bool,
    /// Avoid `OpKill` whenever possible (PowerVR HSR).
    pub preserve_hsr: bool,
    /// Explicit `vkFlushMappedMemoryRanges` on streaming buffers.
    pub flush_mapped_ranges: bool,
    pub ubo_alignment: u64,
    /// Internal EFB scale (1 = 640x528) used for SSAA via blit downscale.
    pub efb_scale: u8,
}

impl RendererProfile {
    pub fn new(vendor_id: u32, min_ubo_alignment: u64, requested_samples: u32, efb_scale: u8) -> Self {
        let vendor = GpuVendorProfile::from_vendor_id(vendor_id);
        Self {
            vendor,
            samples: if vendor.allows_msaa() { requested_samples.clamp(1, 4).next_power_of_two() } else { 1 },
            compute_local_size_x: vendor.compute_local_size_x(),
            transient_efb: vendor.prefers_subpass_efb(),
            preserve_hsr: vendor.must_preserve_hsr(),
            flush_mapped_ranges: vendor == GpuVendorProfile::PowerVrDxt,
            ubo_alignment: min_ubo_alignment.max(1).next_power_of_two(),
            efb_scale: efb_scale.clamp(1, 4),
        }
    }

    /// Round a uniform-buffer offset up to the device alignment.
    pub fn align_ubo(&self, offset: u64) -> u64 {
        offset.div_ceil(self.ubo_alignment) * self.ubo_alignment
    }

    /// Internal EFB resolution.
    pub fn efb_size(&self) -> (u32, u32) {
        (640 * self.efb_scale as u32, 528 * self.efb_scale as u32)
    }
}

const CACHE_MAGIC: &[u8; 4] = b"KDPC";
const CACHE_VERSION: u32 = 1;

/// Identity of the device a pipeline cache blob belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheKey {
    pub vendor_id: u32,
    pub device_id: u32,
    pub driver_version: u32,
    pub uuid: [u8; 16],
}

/// Wrap a `vkGetPipelineCacheData` blob for storage in `context.cacheDir`.
pub fn pack_pipeline_cache(key: &CacheKey, blob: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(blob.len() + 48);
    out.extend_from_slice(CACHE_MAGIC);
    for v in [CACHE_VERSION, key.vendor_id, key.device_id, key.driver_version] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&key.uuid);
    out.extend_from_slice(&(blob.len() as u32).to_le_bytes());
    out.extend_from_slice(&crc32(blob).to_le_bytes());
    out.extend_from_slice(blob);
    out
}

/// Validate and unwrap a stored cache. Returns `None` (start fresh) if it
/// is corrupt or was produced by a different device/driver — feeding a
/// foreign blob to some drivers crashes them.
pub fn unpack_pipeline_cache<'a>(key: &CacheKey, data: &'a [u8]) -> Option<&'a [u8]> {
    if data.len() < 44 || &data[..4] != CACHE_MAGIC {
        return None;
    }
    let word = |i: usize| u32::from_le_bytes(data[4 + i * 4..8 + i * 4].try_into().unwrap());
    if word(0) != CACHE_VERSION || word(1) != key.vendor_id || word(2) != key.device_id || word(3) != key.driver_version {
        return None;
    }
    if data[20..36] != key.uuid {
        return None;
    }
    let len = u32::from_le_bytes(data[36..40].try_into().unwrap()) as usize;
    let crc = u32::from_le_bytes(data[40..44].try_into().unwrap());
    let blob = data.get(44..44 + len)?;
    (crc32(blob) == crc).then_some(blob)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn powervr_profile() {
        let p = RendererProfile::new(0x1010, 64, 4, 3);
        assert_eq!(p.vendor, GpuVendorProfile::PowerVrDxt);
        assert_eq!(p.samples, 1, "MSAA is forced off on PowerVR DXT");
        assert_eq!(p.compute_local_size_x, 32);
        assert!(p.preserve_hsr && p.flush_mapped_ranges && !p.transient_efb);
        assert_eq!(p.efb_size(), (1920, 1584));
        assert_eq!(p.align_ubo(65), 128);
    }

    #[test]
    fn mali_profile() {
        let p = RendererProfile::new(0x13B5, 16, 4, 1);
        assert_eq!(p.samples, 4);
        assert_eq!(p.compute_local_size_x, 64);
        assert!(p.transient_efb && !p.preserve_hsr);
    }

    #[test]
    fn pipeline_cache_round_trip_and_rejection() {
        let key = CacheKey { vendor_id: 0x1010, device_id: 0x36, driver_version: 7, uuid: [9; 16] };
        let blob = vec![1, 2, 3, 4, 5];
        let packed = pack_pipeline_cache(&key, &blob);
        assert_eq!(unpack_pipeline_cache(&key, &packed), Some(&blob[..]));
        let other = CacheKey { driver_version: 8, ..key };
        assert_eq!(unpack_pipeline_cache(&other, &packed), None, "driver update invalidates");
        let mut corrupt = packed.clone();
        *corrupt.last_mut().unwrap() ^= 1;
        assert_eq!(unpack_pipeline_cache(&key, &corrupt), None);
    }
}
