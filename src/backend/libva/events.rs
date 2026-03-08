use super::*;

impl VaapiBackend {
    fn process_decoder_events<R>(
        &mut self,
        report: &mut DecodeReport,
        mut on_frame_ready: R,
    ) -> anyhow::Result<usize>
    where
        R: FnMut(&mut Self, &mut DecodeReport, DecoderHandle) -> anyhow::Result<()>,
    {
        let mut handled = 0;
        while let Some(event) = self.decoder.next_event() {
            match event {
                DecoderEvent::FormatChanged => {
                    let stream_info = self.decoder.stream_info().cloned().ok_or(anyhow!(
                        "Decoder reported a format change without stream info"
                    ))?;
                    self.framepool.resize(&stream_info);
                }
                DecoderEvent::FrameReady(handle) => {
                    handle
                        .sync()
                        .context("Failed to synchronize decoded frame")?;
                    report.frames_decoded += 1;
                    on_frame_ready(self, report, handle)?;
                    handled += 1;
                }
            }
        }
        Ok(handled)
    }

    pub(super) fn handle_decoder_events(
        &mut self,
        export_frame_limit: usize,
        report: &mut DecodeReport,
    ) -> anyhow::Result<usize> {
        self.process_decoder_events(report, |backend, report, handle| {
            if report.exported_frames.len() < export_frame_limit {
                let summary = backend.export_frame(&handle)?;
                report.exported_frames.push(summary);
            }
            Ok(())
        })
    }

    pub(super) fn handle_decoder_events_with_callback<F>(
        &mut self,
        report: &mut DecodeReport,
        on_frame: &mut F,
    ) -> anyhow::Result<usize>
    where
        F: FnMut(PrimeDmabufFrame) -> anyhow::Result<()> + ?Sized,
    {
        self.process_decoder_events(report, |backend, _, handle| {
            let prime_frame = backend.export_prime_frame(&handle)?;
            on_frame(prime_frame)
        })
    }

    pub(super) fn handle_decoder_events_with_cpu_callback<F>(
        &mut self,
        report: &mut DecodeReport,
        on_frame: &mut F,
    ) -> anyhow::Result<usize>
    where
        F: FnMut(CpuNv12Frame) -> anyhow::Result<()> + ?Sized,
    {
        self.process_decoder_events(report, |backend, _, handle| {
            let cpu_frame = backend.export_cpu_frame(&handle)?;
            on_frame(cpu_frame)
        })
    }

    pub(super) fn handle_decoder_events_with_rgba_callback<F>(
        &mut self,
        report: &mut DecodeReport,
        on_frame: &mut F,
    ) -> anyhow::Result<usize>
    where
        F: FnMut(CpuRgbaFrame) -> anyhow::Result<()> + ?Sized,
    {
        self.process_decoder_events(report, |backend, _, handle| {
            let rgba_frame = backend.export_rgba_frame(&handle)?;
            on_frame(rgba_frame)
        })
    }
}
