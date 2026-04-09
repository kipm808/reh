use aubio_rs::{OnsetMode, Tempo};

fn detect_silence(pcm: &[f32], channels: usize, threshold: f32) -> usize {
    for (i, frame) in pcm.chunks_exact(channels).enumerate() {
        let rms = (frame.iter().map(|&s| s * s).sum::<f32>() / channels as f32).sqrt();
        if rms > threshold {
            return i;
        }
    }
    0
}

fn detect_end(pcm: &[f32], channels: usize, threshold: f32) -> usize {
    let total_frames = pcm.len() / channels;
    for i in (0..total_frames).rev() {
        let start = i * channels;
        let frame = &pcm[start..start + channels];
        let rms = (frame.iter().map(|&s| s * s).sum::<f32>() / channels as f32).sqrt();
        if rms > threshold {
            return i;
        }
    }
    total_frames
}

pub fn detect_beats(pcm: &[f32], sample_rate: u32, channels: usize) -> (Vec<usize>, f32) {
    if pcm.is_empty() || channels == 0 {
        return (Vec::new(), 0.0);
    }

    let start_frame = detect_silence(pcm, channels, 0.1);
    let end_frame = detect_end(pcm, channels, 0.1);

    if start_frame >= end_frame {
        return (Vec::new(), 0.0);
    }

    let win_size = 1024;
    let hop_size = 512;

    let mut tempo_detector = Tempo::new(OnsetMode::SpecFlux, win_size, hop_size, sample_rate)
        .expect("Failed to initialize aubio tempo detector");

    let mut detected_onsets = Vec::new();

    let active_pcm = &pcm[start_frame * channels..end_frame * channels];

    for (i, chunk) in active_pcm.chunks_exact(hop_size * channels).enumerate() {
        let mut mono_window = vec![0.0f32; hop_size];
        for (j, sample_chunk) in chunk.chunks_exact(channels).enumerate() {
            mono_window[j] = sample_chunk.iter().sum::<f32>() / channels as f32;
        }

        if tempo_detector.do_result(&mono_window).unwrap_or(0.0) > 0.0 {
            detected_onsets.push(start_frame + (i * hop_size));
        }
    }

    if detected_onsets.len() < 2 {
        return (Vec::new(), 0.0);
    }

    let mut deltas: Vec<usize> = detected_onsets.windows(2).map(|w| w[1] - w[0]).collect();
    deltas.sort_unstable();
    let median_delta = deltas[deltas.len() / 2] as f32;
    let bpm = (60.0 * sample_rate as f32) / median_delta;

    let samples_per_bar = (median_delta * 4.0) as usize;
    let mut bar_indices = Vec::new();

    let snap_threshold = (sample_rate as f32 * 0.1) as usize;

    let mut ideal_cursor = start_frame;

    while ideal_cursor < end_frame {
        let closest_onset = detected_onsets
            .iter()
            .min_by_key(|&&onset| onset.abs_diff(ideal_cursor));

        let final_pos = match closest_onset {
            Some(&onset) if onset.abs_diff(ideal_cursor) < snap_threshold => onset,
            _ => ideal_cursor,
        };

        bar_indices.push(final_pos);

        ideal_cursor = final_pos + samples_per_bar;

        if samples_per_bar == 0 {
            break;
        }
    }

    (bar_indices, bpm)
}
