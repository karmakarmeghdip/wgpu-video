use super::*;

impl VaapiBackend {
    pub(super) fn export_frame<H>(&self, handle: &H) -> anyhow::Result<ExportedDmabufFrame>
    where
        H: DecodedHandle<Frame = DecoderFrame>,
    {
        let frame = handle.video_frame();
        let plane_pitches = frame.get_plane_pitch();
        let plane_sizes = frame.get_plane_size();
        let y_plane_preview = frame
            .map()
            .ok()
            .and_then(|mapping| {
                mapping
                    .get()
                    .first()
                    .map(|plane| plane.iter().copied().take(16).collect())
            })
            .unwrap_or_default();
        let va_surface = frame
            .to_native_handle(&self.display)
            .map_err(|err| anyhow!(err))?;
        let descriptor = va_surface
            .export_prime()
            .context("Failed to export VA surface as PRIME")?;

        Ok(ExportedDmabufFrame {
            timestamp: handle.timestamp(),
            coded_resolution: (
                handle.coded_resolution().width,
                handle.coded_resolution().height,
            ),
            display_resolution: (
                handle.display_resolution().width,
                handle.display_resolution().height,
            ),
            drm_fourcc: descriptor.fourcc,
            width: descriptor.width,
            height: descriptor.height,
            plane_pitches,
            plane_sizes,
            y_plane_preview,
            objects: descriptor
                .objects
                .iter()
                .map(|object| ExportedDmabufObject {
                    fd: object.fd.as_raw_fd(),
                    size: object.size,
                    drm_format_modifier: object.drm_format_modifier,
                })
                .collect(),
            layers: descriptor
                .layers
                .iter()
                .map(|layer| ExportedDmabufLayer {
                    drm_format: layer.drm_format,
                    num_planes: layer.num_planes,
                    object_index: layer.object_index,
                    offset: layer.offset,
                    pitch: layer.pitch,
                })
                .collect(),
        })
    }

    pub(super) fn export_prime_frame(
        &self,
        handle: &Rc<RefCell<VaapiDecodedHandle<DecoderFrame>>>,
    ) -> anyhow::Result<PrimeDmabufFrame> {
        let handle_ref = handle.borrow();
        let va_surface = handle_ref.surface();
        let descriptor = va_surface
            .export_prime()
            .context("Failed to export VA surface as PRIME")?;

        Ok(PrimeDmabufFrame {
            metadata: PrimeFrameMetadata {
                timestamp: handle.timestamp(),
                coded_resolution: (
                    handle.coded_resolution().width,
                    handle.coded_resolution().height,
                ),
                display_resolution: (
                    handle.display_resolution().width,
                    handle.display_resolution().height,
                ),
            },
            descriptor,
        })
    }

    pub(super) fn export_cpu_frame(
        &self,
        handle: &Rc<RefCell<VaapiDecodedHandle<DecoderFrame>>>,
    ) -> anyhow::Result<CpuNv12Frame> {
        fn copy_nv12_image(
            image: &libva::Image<'_>,
            metadata: PrimeFrameMetadata,
        ) -> anyhow::Result<CpuNv12Frame> {
            let va_image = *image.image();
            let data = image.as_ref();
            let width = metadata.display_resolution.0 as usize;
            let height = metadata.display_resolution.1 as usize;
            let y_stride = va_image.pitches[0] as usize;
            let uv_stride = va_image.pitches[1] as usize;
            let y_offset = va_image.offsets[0] as usize;
            let uv_offset = va_image.offsets[1] as usize;

            let mut y_plane = Vec::with_capacity(width * height);
            let mut uv_plane = Vec::with_capacity(width * (height / 2));

            for row in 0..height {
                let start = y_offset + row * y_stride;
                let end = start + width;
                y_plane.extend_from_slice(&data[start..end]);
            }

            for row in 0..(height / 2) {
                let start = uv_offset + row * uv_stride;
                let end = start + width;
                uv_plane.extend_from_slice(&data[start..end]);
            }

            Ok(CpuNv12Frame {
                metadata,
                width: metadata.display_resolution.0,
                height: metadata.display_resolution.1,
                y_stride: metadata.display_resolution.0,
                uv_stride: metadata.display_resolution.0,
                y_plane,
                uv_plane,
            })
        }

        let handle_ref = handle.borrow();
        let va_surface = handle_ref.surface();
        let display_resolution = (
            handle.display_resolution().width,
            handle.display_resolution().height,
        );
        let metadata = PrimeFrameMetadata {
            timestamp: handle.timestamp(),
            coded_resolution: (
                handle.coded_resolution().width,
                handle.coded_resolution().height,
            ),
            display_resolution,
        };

        if let Ok(image) = libva::Image::derive_from(&va_surface, display_resolution) {
            if image.image().format.fourcc == libva::VA_FOURCC_NV12 {
                return copy_nv12_image(&image, metadata);
            }
        }

        let image_format = self
            .display
            .query_image_formats()
            .context("Failed to query VA image formats")?
            .into_iter()
            .find(|format| format.fourcc == libva::VA_FOURCC_NV12)
            .ok_or_else(|| anyhow!("VA driver does not expose an NV12 VAImage format"))?;
        let image = libva::Image::create_from(
            &va_surface,
            image_format,
            metadata.coded_resolution,
            metadata.display_resolution,
        )
        .context("Failed to create a readable NV12 VAImage from the decoded surface")?;

        copy_nv12_image(&image, metadata)
    }

    pub(super) fn export_rgba_frame(
        &mut self,
        handle: &Rc<RefCell<VaapiDecodedHandle<DecoderFrame>>>,
    ) -> anyhow::Result<CpuRgbaFrame> {
        let handle_ref = handle.borrow();
        let va_surface = handle_ref.surface();
        let metadata = PrimeFrameMetadata {
            timestamp: handle.timestamp(),
            coded_resolution: (
                handle.coded_resolution().width,
                handle.coded_resolution().height,
            ),
            display_resolution: (
                handle.display_resolution().width,
                handle.display_resolution().height,
            ),
        };

        let (image_format, format) = self
            .display
            .query_image_formats()
            .context("Failed to query VA image formats")?
            .into_iter()
            .find_map(|format| {
                if format.bits_per_pixel != 32 {
                    return None;
                }
                if format.red_mask == 0x00ff0000
                    && format.green_mask == 0x0000ff00
                    && format.blue_mask == 0x000000ff
                    && format.alpha_mask == 0xff000000
                {
                    Some((format, CpuRgbaFormat::Bgra))
                } else if format.red_mask == 0x000000ff
                    && format.green_mask == 0x0000ff00
                    && format.blue_mask == 0x00ff0000
                    && format.alpha_mask == 0xff000000
                {
                    Some((format, CpuRgbaFormat::Rgba))
                } else {
                    None
                }
            })
            .ok_or_else(|| {
                anyhow!("VA driver does not expose a supported 32-bit RGBA/BGRA image format")
            })?;

        let rgba = {
            let image = libva::Image::create_from(
                &va_surface,
                image_format,
                metadata.coded_resolution,
                metadata.display_resolution,
            )
            .context("Failed to create a readable RGBA VAImage from the decoded surface")?;

            copy_packed_rgba_image(&image, metadata.display_resolution)?
        };

        if !LOGGED_RGBA_EXPORT.swap(true, Ordering::Relaxed) {
            eprintln!(
                "rgba export direct: fourcc=0x{fourcc:08x} bpp={} depth={} byte_order={} masks=(r=0x{r:08x}, g=0x{g:08x}, b=0x{b:08x}, a=0x{a:08x}) first_pixel={:?}",
                image_format.bits_per_pixel,
                image_format.depth,
                image_format.byte_order,
                &rgba.get(0..4).unwrap_or(&[]),
                fourcc = image_format.fourcc,
                r = image_format.red_mask,
                g = image_format.green_mask,
                b = image_format.blue_mask,
                a = image_format.alpha_mask,
            );

            let dump_path = std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join("target/first_frame_rgba.ppm");
            if let Err(err) = dump_rgba_frame_ppm(
                &dump_path,
                metadata.display_resolution.0 as usize,
                metadata.display_resolution.1 as usize,
                format,
                &rgba,
            ) {
                eprintln!(
                    "failed to dump first RGBA frame to {}: {err:#}",
                    dump_path.display()
                );
            } else {
                eprintln!("wrote first RGBA frame dump to {}", dump_path.display());
            }
        }

        Ok(CpuRgbaFrame {
            metadata,
            width: metadata.display_resolution.0,
            height: metadata.display_resolution.1,
            stride: metadata.display_resolution.0 * 4,
            format,
            data: rgba,
        })
    }
}

fn copy_packed_rgba_image(
    image: &libva::Image<'_>,
    display_resolution: (u32, u32),
) -> anyhow::Result<Vec<u8>> {
    let va_image = *image.image();
    let width = display_resolution.0 as usize;
    let height = display_resolution.1 as usize;
    let stride = va_image.pitches[0] as usize;
    let offset = va_image.offsets[0] as usize;
    let data = image.as_ref();
    let mut rgba = Vec::with_capacity(width * height * 4);

    for row in 0..height {
        let start = offset + row * stride;
        let end = start + width * 4;
        rgba.extend_from_slice(&data[start..end]);
    }

    Ok(rgba)
}

fn dump_rgba_frame_ppm(
    path: &Path,
    width: usize,
    height: usize,
    format: CpuRgbaFormat,
    data: &[u8],
) -> anyhow::Result<()> {
    use std::fs::File;
    use std::io::Write;

    let mut file = File::create(path)
        .with_context(|| format!("Failed to create frame dump {}", path.display()))?;
    write!(file, "P6\n{} {}\n255\n", width, height)?;

    for pixel in data.chunks_exact(4) {
        let (r, g, b) = match format {
            CpuRgbaFormat::Rgba => (pixel[0], pixel[1], pixel[2]),
            CpuRgbaFormat::Bgra => (pixel[2], pixel[1], pixel[0]),
        };
        file.write_all(&[r, g, b])?;
    }

    Ok(())
}
