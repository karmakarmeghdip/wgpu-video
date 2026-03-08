use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context};
use ash::vk;
use libloading::Library;
use wgpu::hal::api::Vulkan as VkApi;

use super::libva::PrimeDmabufFrame;

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

    pub fn import_prime_frame(
        &mut self,
        frame: PrimeDmabufFrame,
    ) -> anyhow::Result<ImportedPlaneFrame> {
        self.cleanup_completed()?;

        let mut descriptor = frame.descriptor;
        if descriptor.layers.len() != 1 {
            bail!("Only single-layer PRIME descriptors are currently supported")
        }
        let layer = descriptor.layers.remove(0);
        if layer.num_planes != 2 {
            bail!("Only 2-plane NV12 PRIME descriptors are currently supported")
        }

        let width = frame.metadata.display_resolution.0.max(1);
        let height = frame.metadata.display_resolution.1.max(1);
        let uv_width = width.div_ceil(2);
        let uv_height = height.div_ceil(2);
        let coded_width = frame.metadata.coded_resolution.0.max(width);
        let coded_height = frame.metadata.coded_resolution.1.max(height);

        let hal_device = unsafe { self.device.as_hal::<VkApi>() }
            .ok_or_else(|| anyhow!("The supplied wgpu device is not using the Vulkan backend"))?;
        let raw_device = hal_device.raw_device();

        if descriptor.objects.len() != 1 {
            bail!("Only single-object NV12 PRIME descriptors are currently supported")
        }

        let object = descriptor.objects.into_iter().next().ok_or_else(|| {
            anyhow!("PRIME descriptor did not contain an exported dma-buf object")
        })?;

        let imported_image = self.import_nv12_dmabuf_as_image(
            raw_device,
            hal_device.queue_family_index(),
            &object.fd,
            object.size,
            layer.drm_format,
            object.drm_format_modifier,
            &layer,
            coded_width,
            coded_height,
        )?;

        let (y_image, y_memory) = create_image(raw_device, width, height, vk::Format::R8_UNORM)?;
        let (uv_image, uv_memory) =
            create_image(raw_device, uv_width, uv_height, vk::Format::R8G8_UNORM)?;

        let y_texture = wrap_image_as_texture(
            &self.device,
            &hal_device,
            y_image,
            y_memory,
            width,
            height,
            wgpu::TextureFormat::R8Unorm,
            "vaapi-y-plane",
        );
        let uv_texture = wrap_image_as_texture(
            &self.device,
            &hal_device,
            uv_image,
            uv_memory,
            uv_width,
            uv_height,
            wgpu::TextureFormat::Rg8Unorm,
            "vaapi-uv-plane",
        );

        initialize_imported_texture(&self.queue, &y_texture, width, height, 1);
        initialize_imported_texture(&self.queue, &uv_texture, uv_width, uv_height, 2);
        let init_submission = self.queue.submit([]);
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(init_submission),
                timeout: Some(GPU_INIT_POLL_TIMEOUT),
            })
            .context("Timed out waiting for initialized import textures")?;

        let command_buffer = self.allocate_command_buffer(raw_device)?;
        let fence = unsafe { raw_device.create_fence(&vk::FenceCreateInfo::default(), None) }
            .context("Failed to create Vulkan fence for dmabuf copy")?;

        let submit_result = (|| -> anyhow::Result<()> {
            unsafe {
                raw_device.begin_command_buffer(
                    command_buffer,
                    &vk::CommandBufferBeginInfo::default()
                        .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                )
            }
            .context("Failed to begin Vulkan command buffer for dmabuf copy")?;

            transition_image(
                raw_device,
                command_buffer,
                imported_image.image,
                vk::ImageAspectFlags::PLANE_0,
                vk::ImageLayout::GENERAL,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                vk::AccessFlags::MEMORY_WRITE,
                vk::AccessFlags::TRANSFER_READ,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::TRANSFER,
                Some(vk::QUEUE_FAMILY_EXTERNAL),
                Some(hal_device.queue_family_index()),
            );
            transition_image(
                raw_device,
                command_buffer,
                imported_image.image,
                vk::ImageAspectFlags::PLANE_1,
                vk::ImageLayout::GENERAL,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                vk::AccessFlags::MEMORY_WRITE,
                vk::AccessFlags::TRANSFER_READ,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::TRANSFER,
                Some(vk::QUEUE_FAMILY_EXTERNAL),
                Some(hal_device.queue_family_index()),
            );
            transition_image(
                raw_device,
                command_buffer,
                y_image,
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::AccessFlags::empty(),
                vk::AccessFlags::TRANSFER_WRITE,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::TRANSFER,
                None,
                None,
            );
            transition_image(
                raw_device,
                command_buffer,
                uv_image,
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::AccessFlags::empty(),
                vk::AccessFlags::TRANSFER_WRITE,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::TRANSFER,
                None,
                None,
            );

            let y_copy = vk::ImageCopy::default()
                .src_subresource(
                    vk::ImageSubresourceLayers::default()
                        .aspect_mask(vk::ImageAspectFlags::PLANE_0)
                        .mip_level(0)
                        .base_array_layer(0)
                        .layer_count(1),
                )
                .dst_subresource(
                    vk::ImageSubresourceLayers::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .mip_level(0)
                        .base_array_layer(0)
                        .layer_count(1),
                )
                .extent(vk::Extent3D {
                    width,
                    height,
                    depth: 1,
                });
            unsafe {
                raw_device.cmd_copy_image(
                    command_buffer,
                    imported_image.image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    y_image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[y_copy],
                );
            }

            let uv_copy = vk::ImageCopy::default()
                .src_subresource(
                    vk::ImageSubresourceLayers::default()
                        .aspect_mask(vk::ImageAspectFlags::PLANE_1)
                        .mip_level(0)
                        .base_array_layer(0)
                        .layer_count(1),
                )
                .dst_subresource(
                    vk::ImageSubresourceLayers::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .mip_level(0)
                        .base_array_layer(0)
                        .layer_count(1),
                )
                .extent(vk::Extent3D {
                    width: uv_width,
                    height: uv_height,
                    depth: 1,
                });
            unsafe {
                raw_device.cmd_copy_image(
                    command_buffer,
                    imported_image.image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    uv_image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[uv_copy],
                );
            }

            transition_image(
                raw_device,
                command_buffer,
                y_image,
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                vk::AccessFlags::TRANSFER_WRITE,
                vk::AccessFlags::SHADER_READ,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::FRAGMENT_SHADER,
                None,
                None,
            );
            transition_image(
                raw_device,
                command_buffer,
                uv_image,
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                vk::AccessFlags::TRANSFER_WRITE,
                vk::AccessFlags::SHADER_READ,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::FRAGMENT_SHADER,
                None,
                None,
            );

            unsafe { raw_device.end_command_buffer(command_buffer) }
                .context("Failed to end Vulkan command buffer for dmabuf copy")?;

            let submit_info =
                vk::SubmitInfo::default().command_buffers(std::slice::from_ref(&command_buffer));
            unsafe { raw_device.queue_submit(hal_device.raw_queue(), &[submit_info], fence) }
                .context("Failed to submit Vulkan dmabuf copy work")?;

            Ok(())
        })();

        if let Err(err) = submit_result {
            unsafe {
                raw_device.destroy_fence(fence, None);
                raw_device.free_command_buffers(self.command_pool, &[command_buffer]);
                destroy_imports(raw_device, vec![imported_image]);
            }
            return Err(err);
        }

        let wait_result =
            unsafe { raw_device.wait_for_fences(&[fence], true, GPU_FENCE_TIMEOUT_NS) };
        unsafe {
            destroy_imports(raw_device, vec![imported_image]);
            raw_device.free_command_buffers(self.command_pool, &[command_buffer]);
            raw_device.destroy_fence(fence, None);
        }
        wait_result.context("Timed out waiting for Vulkan dmabuf copy completion")?;

        Ok(ImportedPlaneFrame {
            timestamp: frame.metadata.timestamp,
            width,
            height,
            y_texture,
            uv_texture,
        })
    }

    fn allocate_command_buffer(
        &self,
        raw_device: &ash::Device,
    ) -> anyhow::Result<vk::CommandBuffer> {
        let alloc_info = vk::CommandBufferAllocateInfo::default()
            .command_pool(self.command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        let buffers = unsafe { raw_device.allocate_command_buffers(&alloc_info) }
            .context("Failed to allocate Vulkan command buffer")?;
        buffers
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("Vulkan returned no command buffers"))
    }

    fn import_nv12_dmabuf_as_image(
        &self,
        raw_device: &ash::Device,
        queue_family_index: u32,
        fd: &OwnedFd,
        size: u32,
        drm_format: u32,
        drm_format_modifier: u64,
        layer: &libva::DrmPrimeSurfaceDescriptorLayer,
        coded_width: u32,
        coded_height: u32,
    ) -> anyhow::Result<ImportedImage> {
        const DRM_FORMAT_NV12: u32 = 0x3231_564e;

        if drm_format != DRM_FORMAT_NV12 {
            bail!("Only NV12 DRM formats are currently supported for Vulkan import")
        }
        if layer.object_index[0] != 0 || layer.object_index[1] != 0 {
            bail!("Only single-object NV12 PRIME descriptors are currently supported")
        }

        let plane_layouts = [
            vk::SubresourceLayout {
                offset: layer.offset[0] as u64,
                size: 0,
                row_pitch: layer.pitch[0] as u64,
                array_pitch: 0,
                depth_pitch: 0,
            },
            vk::SubresourceLayout {
                offset: layer.offset[1] as u64,
                size: 0,
                row_pitch: layer.pitch[1] as u64,
                array_pitch: 0,
                depth_pitch: 0,
            },
        ];

        let mut modifier_info = vk::ImageDrmFormatModifierExplicitCreateInfoEXT::default()
            .drm_format_modifier(drm_format_modifier)
            .plane_layouts(&plane_layouts);
        let mut external_memory_info = vk::ExternalMemoryImageCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
        let image_info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk::Format::G8_B8R8_2PLANE_420_UNORM)
            .extent(vk::Extent3D {
                width: coded_width,
                height: coded_height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
            .usage(vk::ImageUsageFlags::TRANSFER_SRC)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .queue_family_indices(std::slice::from_ref(&queue_family_index))
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .push_next(&mut modifier_info)
            .push_next(&mut external_memory_info);
        let image = unsafe { raw_device.create_image(&image_info, None) }
            .context("Failed to create Vulkan image for dma-buf NV12 import")?;
        let requirements = unsafe { raw_device.get_image_memory_requirements(image) };

        let mut fd_properties = vk::MemoryFdPropertiesKHR::default();
        let result = unsafe {
            (self.external_memory_fd_fns.get_memory_fd_properties_khr)(
                raw_device.handle(),
                vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT,
                fd.as_raw_fd(),
                &mut fd_properties,
            )
        };
        if result != vk::Result::SUCCESS {
            unsafe { raw_device.destroy_image(image, None) };
            return Err(anyhow!("vkGetMemoryFdPropertiesKHR failed with {result:?}"));
        }

        let memory_type_bits = requirements.memory_type_bits & fd_properties.memory_type_bits;
        let memory_type_index = lowest_set_bit(memory_type_bits)
            .ok_or_else(|| anyhow!("No compatible Vulkan memory type for imported dmabuf"))?;

        let import_fd = fd
            .try_clone()
            .context("Failed to clone dmabuf fd for Vulkan import")?;
        let import_fd_raw = import_fd.into_raw_fd();
        let mut import_info = vk::ImportMemoryFdInfoKHR::default()
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
            .fd(import_fd_raw);
        let alloc_info = vk::MemoryAllocateInfo::default()
            .allocation_size(requirements.size.max(size as u64))
            .memory_type_index(memory_type_index)
            .push_next(&mut import_info);
        let memory = match unsafe { raw_device.allocate_memory(&alloc_info, None) } {
            Ok(memory) => memory,
            Err(err) => {
                let _owned = unsafe { OwnedFd::from_raw_fd(import_fd_raw) };
                unsafe { raw_device.destroy_image(image, None) };
                return Err(anyhow!(err))
                    .context("Failed to allocate Vulkan memory for dmabuf image import");
            }
        };
        if let Err(err) = unsafe { raw_device.bind_image_memory(image, memory, 0) } {
            unsafe {
                raw_device.free_memory(memory, None);
                raw_device.destroy_image(image, None);
            }
            return Err(anyhow!(err))
                .context("Failed to bind imported dmabuf memory to Vulkan image");
        }

        Ok(ImportedImage { image, memory })
    }

    fn cleanup_completed(&mut self) -> anyhow::Result<()> {
        if self.pending_releases.is_empty() {
            return Ok(());
        }
        let hal_device = unsafe { self.device.as_hal::<VkApi>() }
            .ok_or_else(|| anyhow!("The supplied wgpu device is not using the Vulkan backend"))?;
        let raw_device = hal_device.raw_device();
        let mut still_pending = Vec::with_capacity(self.pending_releases.len());

        for pending in self.pending_releases.drain(..) {
            let ready = unsafe { raw_device.get_fence_status(pending.fence) }
                .context("Failed to query Vulkan fence status for dmabuf import")?;
            if ready {
                unsafe {
                    destroy_imports(raw_device, pending.imports);
                    raw_device.free_command_buffers(self.command_pool, &[pending.command_buffer]);
                    raw_device.destroy_fence(pending.fence, None);
                }
            } else {
                still_pending.push(pending);
            }
        }

        self.pending_releases = still_pending;
        Ok(())
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

fn create_image(
    raw_device: &ash::Device,
    width: u32,
    height: u32,
    format: vk::Format,
) -> anyhow::Result<(vk::Image, vk::DeviceMemory)> {
    let info = vk::ImageCreateInfo::default()
        .image_type(vk::ImageType::TYPE_2D)
        .format(format)
        .extent(vk::Extent3D {
            width,
            height,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::OPTIMAL)
        .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED);
    let image = unsafe { raw_device.create_image(&info, None) }
        .context("Failed to create Vulkan image for copied video plane")?;
    let requirements = unsafe { raw_device.get_image_memory_requirements(image) };
    let memory_type_index = lowest_set_bit(requirements.memory_type_bits)
        .ok_or_else(|| anyhow!("No compatible Vulkan memory type for copied video plane"))?;
    let alloc_info = vk::MemoryAllocateInfo::default()
        .allocation_size(requirements.size)
        .memory_type_index(memory_type_index);
    let memory = match unsafe { raw_device.allocate_memory(&alloc_info, None) } {
        Ok(memory) => memory,
        Err(err) => {
            unsafe { raw_device.destroy_image(image, None) };
            return Err(anyhow!(err))
                .context("Failed to allocate Vulkan image memory for copied video plane");
        }
    };
    if let Err(err) = unsafe { raw_device.bind_image_memory(image, memory, 0) } {
        unsafe {
            raw_device.free_memory(memory, None);
            raw_device.destroy_image(image, None);
        }
        return Err(anyhow!(err))
            .context("Failed to bind Vulkan image memory for copied video plane");
    }

    Ok((image, memory))
}

fn wrap_image_as_texture(
    device: &wgpu::Device,
    hal_device: &wgpu::hal::vulkan::Device,
    image: vk::Image,
    memory: vk::DeviceMemory,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
    label: &'static str,
) -> wgpu::Texture {
    let hal_texture = unsafe {
        hal_device.texture_from_raw(
            image,
            &wgpu::hal::TextureDescriptor {
                label: Some(label),
                usage: wgpu::TextureUses::RESOURCE
                    | wgpu::TextureUses::COPY_SRC
                    | wgpu::TextureUses::COPY_DST,
                memory_flags: wgpu::hal::MemoryFlags::empty(),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                dimension: wgpu::TextureDimension::D2,
                sample_count: 1,
                view_formats: Vec::new(),
                format,
                mip_level_count: 1,
            },
            None,
            wgpu::hal::vulkan::TextureMemory::Dedicated(memory),
        )
    };

    unsafe {
        device.create_texture_from_hal::<VkApi>(
            hal_texture,
            &wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
        )
    }
}

fn initialize_imported_texture(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
    bytes_per_pixel: u32,
) {
    let data = vec![0u8; (width * height * bytes_per_pixel) as usize];
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width * bytes_per_pixel),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
}

fn transition_image(
    raw_device: &ash::Device,
    command_buffer: vk::CommandBuffer,
    image: vk::Image,
    aspect_mask: vk::ImageAspectFlags,
    old_layout: vk::ImageLayout,
    new_layout: vk::ImageLayout,
    src_access_mask: vk::AccessFlags,
    dst_access_mask: vk::AccessFlags,
    src_stage_mask: vk::PipelineStageFlags,
    dst_stage_mask: vk::PipelineStageFlags,
    src_queue_family_index: Option<u32>,
    dst_queue_family_index: Option<u32>,
) {
    let barrier = vk::ImageMemoryBarrier::default()
        .old_layout(old_layout)
        .new_layout(new_layout)
        .src_access_mask(src_access_mask)
        .dst_access_mask(dst_access_mask)
        .src_queue_family_index(src_queue_family_index.unwrap_or(vk::QUEUE_FAMILY_IGNORED))
        .dst_queue_family_index(dst_queue_family_index.unwrap_or(vk::QUEUE_FAMILY_IGNORED))
        .image(image)
        .subresource_range(
            vk::ImageSubresourceRange::default()
                .aspect_mask(aspect_mask)
                .base_mip_level(0)
                .level_count(1)
                .base_array_layer(0)
                .layer_count(1),
        );
    unsafe {
        raw_device.cmd_pipeline_barrier(
            command_buffer,
            src_stage_mask,
            dst_stage_mask,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[barrier],
        );
    }
}

fn destroy_imports(raw_device: &ash::Device, imports: Vec<ImportedImage>) {
    for import in imports {
        unsafe {
            raw_device.destroy_image(import.image, None);
            raw_device.free_memory(import.memory, None);
        }
    }
}

fn lowest_set_bit(bits: u32) -> Option<u32> {
    if bits == 0 {
        None
    } else {
        Some(bits.trailing_zeros())
    }
}

fn load_external_memory_fd_fns(
    raw_device: &ash::Device,
    vulkan_loader: &Library,
) -> anyhow::Result<ash::khr::external_memory_fd::DeviceFn> {
    let get_device_proc_addr =
        unsafe { vulkan_loader.get::<vk::PFN_vkGetDeviceProcAddr>(b"vkGetDeviceProcAddr") }
            .context("Failed to resolve vkGetDeviceProcAddr")?;
    let get_device_proc_addr = *get_device_proc_addr;

    Ok(ash::khr::external_memory_fd::DeviceFn::load(
        |name| unsafe {
            get_device_proc_addr(raw_device.handle(), name.as_ptr())
                .map_or(std::ptr::null(), |proc| proc as *const _)
        },
    ))
}
