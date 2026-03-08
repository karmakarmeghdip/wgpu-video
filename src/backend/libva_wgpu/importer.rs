use super::vulkan::{
    create_image, destroy_imports, initialize_imported_texture, lowest_set_bit, transition_image,
    wrap_image_as_texture,
};
use super::*;

impl VaapiVulkanFrameImporter {
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
