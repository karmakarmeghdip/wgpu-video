use super::*;

pub(super) fn create_image(
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

pub(super) fn wrap_image_as_texture(
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

pub(super) fn initialize_imported_texture(
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

pub(super) fn transition_image(
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

pub(super) fn destroy_imports(raw_device: &ash::Device, imports: Vec<ImportedImage>) {
    for import in imports {
        unsafe {
            raw_device.destroy_image(import.image, None);
            raw_device.free_memory(import.memory, None);
        }
    }
}

pub(super) fn lowest_set_bit(bits: u32) -> Option<u32> {
    if bits == 0 {
        None
    } else {
        Some(bits.trailing_zeros())
    }
}

pub(super) fn load_external_memory_fd_fns(
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
