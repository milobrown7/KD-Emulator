//! Vulkan device bring-up for the GX renderer (ash).
//!
//! Picks the physical device, derives the [`RendererProfile`] from
//! `VkPhysicalDeviceProperties::vendorID` (0x1010 PowerVR DXT, 0x13B5 Mali),
//! restores the persistent `VkPipelineCache`, and builds the EFB render
//! pass and images with the vendor rules:
//!
//! * PowerVR DXT: `rasterizationSamples = 1` (SSAA via resolution scaling
//!   and `vkCmdBlitImage` downscale instead of MSAA), optimal tiling,
//!   HSR-friendly shaders, explicit mapped-range flushes.
//! * Mali: transient, lazily allocated EFB attachments so depth/colour
//!   stay in tile memory.
//!
//! Ray-tracing extensions are never requested (Tensor G5's PowerVR omits
//! the RT units).

use crate::profile::{pack_pipeline_cache, unpack_pipeline_cache, CacheKey, RendererProfile};
use ash::vk;
use std::ffi::CStr;

#[derive(Debug)]
pub enum VkError {
    Loader(String),
    NoDevice,
    Vk(vk::Result),
}

impl std::fmt::Display for VkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VkError::Loader(e) => write!(f, "cannot load Vulkan: {e}"),
            VkError::NoDevice => write!(f, "no Vulkan device with a graphics queue"),
            VkError::Vk(r) => write!(f, "Vulkan error {r:?}"),
        }
    }
}

impl std::error::Error for VkError {}

impl From<vk::Result> for VkError {
    fn from(r: vk::Result) -> Self {
        VkError::Vk(r)
    }
}

pub struct VulkanContext {
    _entry: ash::Entry,
    pub instance: ash::Instance,
    pub physical: vk::PhysicalDevice,
    pub device: ash::Device,
    pub queue: vk::Queue,
    pub queue_family: u32,
    pub properties: vk::PhysicalDeviceProperties,
    pub profile: RendererProfile,
    pub pipeline_cache: vk::PipelineCache,
}

/// An EFB colour + depth target pair.
pub struct Efb {
    pub width: u32,
    pub height: u32,
    pub color: vk::Image,
    pub depth: vk::Image,
    pub memory: Vec<vk::DeviceMemory>,
    pub render_pass: vk::RenderPass,
}

impl VulkanContext {
    /// Create an instance and device. `instance_exts` / `device_exts` are
    /// supplied by the platform layer (e.g. `VK_KHR_android_surface`,
    /// `VK_KHR_swapchain`).
    pub fn new(
        instance_exts: &[&CStr],
        device_exts: &[&CStr],
        cache_file: Option<&[u8]>,
        requested_samples: u32,
        efb_scale: u8,
    ) -> Result<Self, VkError> {
        // SAFETY: loading the system Vulkan loader.
        let entry = unsafe { ash::Entry::load() }.map_err(|e| VkError::Loader(e.to_string()))?;
        let app = vk::ApplicationInfo::default()
            .application_name(c"KD-Emulator")
            .engine_name(c"KD-Emulator")
            .api_version(vk::make_api_version(0, 1, 1, 0));
        let ext_ptrs: Vec<*const std::ffi::c_char> = instance_exts.iter().map(|e| e.as_ptr()).collect();
        let info = vk::InstanceCreateInfo::default().application_info(&app).enabled_extension_names(&ext_ptrs);
        // SAFETY: valid create info.
        let instance = unsafe { entry.create_instance(&info, None)? };
        let picked = unsafe { instance.enumerate_physical_devices()? }.into_iter().find_map(|pd| {
            let families = unsafe { instance.get_physical_device_queue_family_properties(pd) };
            families.iter().position(|f| f.queue_flags.contains(vk::QueueFlags::GRAPHICS)).map(|i| (pd, i as u32))
        });
        let Some((physical, queue_family)) = picked else {
            unsafe { instance.destroy_instance(None) };
            return Err(VkError::NoDevice);
        };
        let properties = unsafe { instance.get_physical_device_properties(physical) };
        let profile = RendererProfile::new(
            properties.vendor_id,
            properties.limits.min_uniform_buffer_offset_alignment,
            requested_samples,
            efb_scale,
        );
        let priorities = [1.0f32];
        let queue_info = vk::DeviceQueueCreateInfo::default().queue_family_index(queue_family).queue_priorities(&priorities);
        let dev_ext_ptrs: Vec<*const std::ffi::c_char> = device_exts.iter().map(|e| e.as_ptr()).collect();
        let dev_info = vk::DeviceCreateInfo::default()
            .queue_create_infos(std::slice::from_ref(&queue_info))
            .enabled_extension_names(&dev_ext_ptrs);
        let device = unsafe { instance.create_device(physical, &dev_info, None)? };
        let queue = unsafe { device.get_device_queue(queue_family, 0) };

        let key = Self::cache_key(&properties);
        let initial = cache_file.and_then(|f| unpack_pipeline_cache(&key, f)).unwrap_or(&[]);
        let cache_info = vk::PipelineCacheCreateInfo::default().initial_data(initial);
        let pipeline_cache = unsafe { device.create_pipeline_cache(&cache_info, None)? };
        Ok(Self { _entry: entry, instance, physical, device, queue, queue_family, properties, profile, pipeline_cache })
    }

    fn cache_key(p: &vk::PhysicalDeviceProperties) -> CacheKey {
        CacheKey { vendor_id: p.vendor_id, device_id: p.device_id, driver_version: p.driver_version, uuid: p.pipeline_cache_uuid }
    }

    pub fn device_name(&self) -> String {
        self.properties.device_name_as_c_str().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// Serialise the pipeline cache for `context.cacheDir`.
    pub fn save_pipeline_cache(&self) -> Result<Vec<u8>, VkError> {
        let blob = unsafe { self.device.get_pipeline_cache_data(self.pipeline_cache)? };
        Ok(pack_pipeline_cache(&Self::cache_key(&self.properties), &blob))
    }

    fn memory_type(&self, bits: u32, want: vk::MemoryPropertyFlags) -> Option<u32> {
        let mem = unsafe { self.instance.get_physical_device_memory_properties(self.physical) };
        (0..mem.memory_type_count)
            .find(|&i| bits & (1 << i) != 0 && mem.memory_types[i as usize].property_flags.contains(want))
    }

    /// Create the EFB images and render pass following the profile.
    pub fn create_efb(&self) -> Result<Efb, VkError> {
        let (width, height) = self.profile.efb_size();
        let samples = vk::SampleCountFlags::from_raw(self.profile.samples);
        let transient = self.profile.transient_efb;
        let make_image = |format: vk::Format, usage: vk::ImageUsageFlags| -> Result<(vk::Image, vk::DeviceMemory), VkError> {
            let info = vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(format)
                .extent(vk::Extent3D { width, height, depth: 1 })
                .mip_levels(1)
                .array_layers(1)
                .samples(samples)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(usage)
                .initial_layout(vk::ImageLayout::UNDEFINED);
            let image = unsafe { self.device.create_image(&info, None)? };
            let req = unsafe { self.device.get_image_memory_requirements(image) };
            let lazy = usage.contains(vk::ImageUsageFlags::TRANSIENT_ATTACHMENT);
            let ty = (if lazy { self.memory_type(req.memory_type_bits, vk::MemoryPropertyFlags::LAZILY_ALLOCATED) } else { None })
                .or_else(|| self.memory_type(req.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL))
                .ok_or(VkError::NoDevice)?;
            let alloc = vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(ty);
            let memory = unsafe { self.device.allocate_memory(&alloc, None)? };
            unsafe { self.device.bind_image_memory(image, memory, 0)? };
            Ok((image, memory))
        };
        // Colour must be copyable for EFB copies; depth can stay in tile
        // memory on Mali.
        let color_usage = vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::SAMPLED;
        let mut depth_usage = vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT;
        if transient {
            depth_usage |= vk::ImageUsageFlags::TRANSIENT_ATTACHMENT | vk::ImageUsageFlags::INPUT_ATTACHMENT;
        }
        let (color, cmem) = make_image(vk::Format::R8G8B8A8_UNORM, color_usage)?;
        let (depth, dmem) = make_image(vk::Format::D32_SFLOAT, depth_usage)?;
        let attachments = [
            vk::AttachmentDescription::default()
                .format(vk::Format::R8G8B8A8_UNORM)
                .samples(samples)
                .load_op(vk::AttachmentLoadOp::LOAD)
                .store_op(vk::AttachmentStoreOp::STORE)
                .initial_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .final_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL),
            vk::AttachmentDescription::default()
                .format(vk::Format::D32_SFLOAT)
                .samples(samples)
                .load_op(vk::AttachmentLoadOp::LOAD)
                .store_op(if transient { vk::AttachmentStoreOp::DONT_CARE } else { vk::AttachmentStoreOp::STORE })
                .initial_layout(vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL)
                .final_layout(vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL),
        ];
        let color_ref = [vk::AttachmentReference { attachment: 0, layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL }];
        let depth_ref = vk::AttachmentReference { attachment: 1, layout: vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL };
        let depth_input = [vk::AttachmentReference { attachment: 1, layout: vk::ImageLayout::DEPTH_STENCIL_READ_ONLY_OPTIMAL }];
        let draw = vk::SubpassDescription::default()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(&color_ref)
            .depth_stencil_attachment(&depth_ref);
        // Mali: a second subpass reads depth as an input attachment for EFB
        // peeks, so the render pass is never split mid-frame.
        let peek = vk::SubpassDescription::default()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(&color_ref)
            .input_attachments(&depth_input);
        let subpasses = if transient { vec![draw, peek] } else { vec![draw] };
        let deps = [vk::SubpassDependency::default()
            .src_subpass(0)
            .dst_subpass(1)
            .src_stage_mask(vk::PipelineStageFlags::LATE_FRAGMENT_TESTS)
            .dst_stage_mask(vk::PipelineStageFlags::FRAGMENT_SHADER)
            .src_access_mask(vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE)
            .dst_access_mask(vk::AccessFlags::INPUT_ATTACHMENT_READ)
            .dependency_flags(vk::DependencyFlags::BY_REGION)];
        let mut rp_info = vk::RenderPassCreateInfo::default().attachments(&attachments).subpasses(&subpasses);
        if transient {
            rp_info = rp_info.dependencies(&deps);
        }
        let render_pass = unsafe { self.device.create_render_pass(&rp_info, None)? };
        Ok(Efb { width, height, color, depth, memory: vec![cmem, dmem], render_pass })
    }

    /// Record the SSAA resolve: blit the scaled EFB down to `dst` with
    /// linear filtering (used instead of MSAA on PowerVR DXT).
    pub fn record_downscale(&self, cmd: vk::CommandBuffer, efb: &Efb, dst: vk::Image, dst_w: u32, dst_h: u32) {
        let sub = vk::ImageSubresourceLayers { aspect_mask: vk::ImageAspectFlags::COLOR, mip_level: 0, base_array_layer: 0, layer_count: 1 };
        let region = vk::ImageBlit::default()
            .src_subresource(sub)
            .src_offsets([vk::Offset3D::default(), vk::Offset3D { x: efb.width as i32, y: efb.height as i32, z: 1 }])
            .dst_subresource(sub)
            .dst_offsets([vk::Offset3D::default(), vk::Offset3D { x: dst_w as i32, y: dst_h as i32, z: 1 }]);
        unsafe {
            self.device.cmd_blit_image(
                cmd,
                efb.color,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                dst,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[region],
                vk::Filter::LINEAR,
            );
        }
    }

    pub fn destroy_efb(&self, efb: Efb) {
        unsafe {
            self.device.destroy_render_pass(efb.render_pass, None);
            self.device.destroy_image(efb.color, None);
            self.device.destroy_image(efb.depth, None);
            for m in efb.memory {
                self.device.free_memory(m, None);
            }
        }
    }
}

impl Drop for VulkanContext {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();
            self.device.destroy_pipeline_cache(self.pipeline_cache, None);
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs only where a Vulkan driver is installed (skips in CI containers).
    #[test]
    fn bring_up_when_available() {
        let ctx = match VulkanContext::new(&[], &[], None, 4, 1) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("skipping: {e}");
                return;
            }
        };
        let efb = ctx.create_efb().expect("EFB");
        assert_eq!((efb.width, efb.height), (640, 528));
        let blob = ctx.save_pipeline_cache().unwrap();
        assert_eq!(&blob[..4], b"KDPC");
        ctx.destroy_efb(efb);
    }
}
