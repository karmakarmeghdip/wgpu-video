use super::*;

pub(super) fn sample_to_annex_b(
    config: &H264TrackConfig,
    packet: &[u8],
    is_sync: bool,
    parameter_sets_sent: &mut bool,
) -> anyhow::Result<Vec<u8>> {
    let mut annex_b = convert_avcc_packet(packet, config.nal_length_size)?;

    if !*parameter_sets_sent || is_sync {
        let header_bytes = config
            .sequence_parameter_sets
            .iter()
            .chain(config.picture_parameter_sets.iter())
            .map(|nal| nal.len() + ANNEX_B_START_CODE.len())
            .sum::<usize>();
        let mut with_headers = Vec::with_capacity(annex_b.len() + header_bytes);
        for nal in &config.sequence_parameter_sets {
            push_annex_b_nal(&mut with_headers, nal);
        }
        for nal in &config.picture_parameter_sets {
            push_annex_b_nal(&mut with_headers, nal);
        }
        with_headers.append(&mut annex_b);
        annex_b = with_headers;
        *parameter_sets_sent = true;
    }

    Ok(annex_b)
}

pub(super) fn hevc_sample_to_annex_b(
    config: &H265TrackConfig,
    packet: &[u8],
    is_sync: bool,
    parameter_sets_sent: &mut bool,
) -> anyhow::Result<Vec<u8>> {
    let mut annex_b = convert_avcc_packet(packet, config.nal_length_size)?;

    if !*parameter_sets_sent || is_sync {
        let header_bytes = config
            .video_parameter_sets
            .iter()
            .chain(config.sequence_parameter_sets.iter())
            .chain(config.picture_parameter_sets.iter())
            .map(|nal| nal.len() + ANNEX_B_START_CODE.len())
            .sum::<usize>();
        let mut with_headers = Vec::with_capacity(annex_b.len() + header_bytes);
        for nal in &config.video_parameter_sets {
            push_annex_b_nal(&mut with_headers, nal);
        }
        for nal in &config.sequence_parameter_sets {
            push_annex_b_nal(&mut with_headers, nal);
        }
        for nal in &config.picture_parameter_sets {
            push_annex_b_nal(&mut with_headers, nal);
        }
        with_headers.append(&mut annex_b);
        annex_b = with_headers;
        *parameter_sets_sent = true;
    }

    Ok(annex_b)
}

fn convert_avcc_packet(packet: &[u8], length_size: usize) -> anyhow::Result<Vec<u8>> {
    try_convert_avcc_packet(packet, length_size).ok_or_else(|| {
        anyhow!(
            "Unsupported AVC sample layout; failed to parse NAL length prefix size {}",
            length_size
        )
    })
}

fn try_convert_avcc_packet(packet: &[u8], length_size: usize) -> Option<Vec<u8>> {
    let mut cursor = 0usize;
    let mut output = Vec::with_capacity(packet.len() + 64);

    while cursor < packet.len() {
        if cursor + length_size > packet.len() {
            return None;
        }

        let nal_len = match length_size {
            1 => packet[cursor] as usize,
            2 => u16::from_be_bytes([packet[cursor], packet[cursor + 1]]) as usize,
            4 => u32::from_be_bytes([
                packet[cursor],
                packet[cursor + 1],
                packet[cursor + 2],
                packet[cursor + 3],
            ]) as usize,
            _ => return None,
        };
        cursor += length_size;

        if nal_len == 0 || cursor + nal_len > packet.len() {
            return None;
        }

        push_annex_b_nal(&mut output, &packet[cursor..cursor + nal_len]);
        cursor += nal_len;
    }

    if output.is_empty() {
        None
    } else {
        Some(output)
    }
}

fn push_annex_b_nal(output: &mut Vec<u8>, nal: &[u8]) {
    output.extend_from_slice(&ANNEX_B_START_CODE);
    output.extend_from_slice(nal);
}
