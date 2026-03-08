use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::sync::Arc;

use anyhow::{Context, anyhow, bail};
use ash::vk;
use libloading::Library;
use wgpu::hal::api::Vulkan as VkApi;

use self::vulkan::{destroy_imports, load_external_memory_fd_fns};
use super::libva::PrimeDmabufFrame;

mod importer;
mod vulkan;

const GPU_INIT_POLL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);
const GPU_FENCE_TIMEOUT_NS: u64 = 1_000_000_000;

pub struct ImportedPlaneFrame {
    pub timestamp: u64,
    pub width: u32,
    pub height: u32,
    pub y_texture: wgpu::Texture,
    pub uv_texture: wgpu::Texture,
}

struct ImportedImage {
    image: vk::Image,
    memory: vk::DeviceMemory,
}

struct PendingRelease {
    fence: vk::Fence,
    command_buffer: vk::CommandBuffer,
    imports: Vec<ImportedImage>,
}

pub struct VaapiVulkanFrameImporter {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    command_pool: vk::CommandPool,
    external_memory_fd_fns: ash::khr::external_memory_fd::DeviceFn,
    _vulkan_loader: Library,
    pending_releases: Vec<PendingRelease>,
}

impl VaapiVulkanFrameImporter {
    pub fn is_supported(device: &wgpu::Device) -> bool {
        unsafe { device.as_hal::<VkApi>() }.is_some()
    }

    pub fn new(device: Arc<wgpu::Device>, queue: Arc<wgpu::Queue>) -> anyhow::Result<Self> {
        let vulkan_loader =
            unsafe { Library::new("libvulkan.so.1") }.context("Failed to load libvulkan.so.1")?;
        let (command_pool, external_memory_fd_fns) = {
            let hal_device = unsafe { device.as_hal::<VkApi>() }.ok_or_else(|| {
                anyhow!("The supplied wgpu device is not using the Vulkan backend")
            })?;
            let raw_device = hal_device.raw_device();
            let external_memory_fd_fns = load_external_memory_fd_fns(raw_device, &vulkan_loader)?;
            let command_pool_info = vk::CommandPoolCreateInfo::default()
                .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
                .queue_family_index(hal_device.queue_family_index());
            let command_pool = unsafe { raw_device.create_command_pool(&command_pool_info, None) }
                .context("Failed to create Vulkan command pool for dmabuf imports")?;
            (command_pool, external_memory_fd_fns)
        };

        Ok(Self {
            device,
            queue,
            command_pool,
            external_memory_fd_fns,
            _vulkan_loader: vulkan_loader,
            pending_releases: Vec::new(),
        })
    }
}

impl Drop for VaapiVulkanFrameImporter {
    fn drop(&mut self) {
        let Some(hal_device) = (unsafe { self.device.as_hal::<VkApi>() }) else {
            return;
        };
        let raw_device = hal_device.raw_device();
        unsafe {
            let _ = raw_device.device_wait_idle();
            for pending in self.pending_releases.drain(..) {
                destroy_imports(raw_device, pending.imports);
                raw_device.free_command_buffers(self.command_pool, &[pending.command_buffer]);
                raw_device.destroy_fence(pending.fence, None);
            }
            raw_device.destroy_command_pool(self.command_pool, None);
        }
    }
}
