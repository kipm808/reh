use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::{Receiver, Sender, unbounded};
use eframe::egui;
use rfd::FileDialog;
use signalsmith_stretch::Stretch;
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::probe::Hint;

mod beats;

struct AppState {
    file_path: String,
    total_samples: usize,
    sample_rate: u32,
    channels: usize,
    waveform: Vec<f32>,
    beat_markers: Vec<usize>,
    bpm: f32,
}

struct AudioControls {
    speed: AtomicU32,
    pitch: AtomicU32,
    volume: AtomicU32,
    cursor: AtomicUsize,
    loop_start: AtomicUsize,
    loop_end: AtomicUsize,
    is_playing: AtomicBool,
    is_loading: AtomicBool,
    is_seeking: AtomicBool,
    pcm_ptr: AtomicPtr<Vec<f32>>,
}

impl Drop for AudioControls {
    fn drop(&mut self) {
        let ptr = self.pcm_ptr.swap(ptr::null_mut(), Ordering::Acquire);
        if !ptr.is_null() {
            unsafe {
                drop(Box::from_raw(ptr));
            }
        }
    }
}

enum ParamUpdate {
    Speed(f32),
    Pitch(f32),
}

struct PlayerApp {
    state: Arc<Mutex<AppState>>,
    controls: Arc<AudioControls>,
    dragging_marker: Option<bool>,
    _stream: Option<cpal::Stream>,
    tx: Sender<ParamUpdate>,
}

impl PlayerApp {
    fn new(_cc: &eframe::CreationContext<'_>, initial_path: Option<PathBuf>) -> Self {
        let (tx, rx) = unbounded();
        let controls = Arc::new(AudioControls {
            speed: AtomicU32::new(1.0f32.to_bits()),
            pitch: AtomicU32::new(1.0f32.to_bits()),
            volume: AtomicU32::new(1.0f32.to_bits()),
            cursor: AtomicUsize::new(0),
            loop_start: AtomicUsize::new(0),
            loop_end: AtomicUsize::new(0),
            is_playing: AtomicBool::new(true),
            is_loading: AtomicBool::new(false),
            is_seeking: AtomicBool::new(false),
            pcm_ptr: AtomicPtr::new(Box::into_raw(Box::new(Vec::new()))),
        });

        let state = Arc::new(Mutex::new(AppState {
            file_path: "No file selected".to_string(),
            total_samples: 0,
            sample_rate: 44100,
            channels: 2,
            waveform: Vec::new(),
            beat_markers: Vec::new(),
            bpm: 0.0,
        }));

        let mut app = Self {
            state,
            controls,
            dragging_marker: None,
            _stream: None,
            tx,
        };

        if let Some(path) = initial_path {
            app.load_audio_file(path);
        }
        app.start_playback(rx);
        app
    }

    fn load_audio_file(&mut self, path: PathBuf) {
        if !path.exists() {
            return;
        }
        let c = self.controls.clone();
        let s_ptr = self.state.clone();

        c.is_loading.store(true, Ordering::SeqCst);
        {
            let mut s = s_ptr.lock().unwrap();
            s.file_path = path.to_string_lossy().into_owned();
        }

        thread::spawn(move || {
            let file = match std::fs::File::open(&path) {
                Ok(f) => f,
                Err(_) => {
                    c.is_loading.store(false, Ordering::SeqCst);
                    return;
                }
            };
            let mss = MediaSourceStream::new(Box::new(file), Default::default());
            let mut hint = Hint::new();
            if let Some(ext) = path.extension() {
                hint.with_extension(&ext.to_string_lossy());
            }

            let probed = symphonia::default::get_probe().format(
                &hint,
                mss,
                &Default::default(),
                &Default::default(),
            );
            let mut format = match probed {
                Ok(p) => p.format,
                Err(_) => {
                    c.is_loading.store(false, Ordering::SeqCst);
                    return;
                }
            };
            let track = match format.default_track() {
                Some(t) => t,
                None => {
                    c.is_loading.store(false, Ordering::SeqCst);
                    return;
                }
            };

            let params = track.codec_params.clone();
            let mut decoder = symphonia::default::get_codecs()
                .make(&params, &Default::default())
                .unwrap();
            let mut pcm = Vec::new();

            while let Ok(packet) = format.next_packet() {
                if let Ok(decoded) = decoder.decode(&packet) {
                    let mut sb =
                        SampleBuffer::<f32>::new(decoded.capacity() as u64, *decoded.spec());
                    sb.copy_interleaved_ref(decoded);
                    pcm.extend_from_slice(sb.samples());
                }
            }

            if !pcm.is_empty() {
                // 1. Calculate RMS (Average Power)
                let square_sum: f32 = pcm.iter().map(|&s| s * s).sum();
                let rms = (square_sum / pcm.len() as f32).sqrt().max(1e-6);

                // 2. Target a standard "healthy" RMS (e.g., -14dB or ~0.2)
                let target_rms = 0.2;
                let gain_factor = target_rms / rms;

                for sample in pcm.iter_mut() {
                    // Apply gain
                    let mut x = *sample * gain_factor;

                    // 3. Light Limiting (Soft Knee Clipping)
                    // This curve is linear until 0.7, then gently curves to 1.0
                    if x.abs() > 0.7 {
                        if x > 0.0 {
                            x = 0.7 + (x - 0.7) / (1.0 + (x - 0.7).powi(2));
                        } else {
                            let x_abs = x.abs();
                            x = -(0.7 + (x_abs - 0.7) / (1.0 + (x_abs - 0.7).powi(2)));
                        }
                    }

                    // Final hard safety clamp
                    *sample = x.clamp(-0.99, 0.99);
                }
            }

            let mut waveform = Vec::new();
            let chunk_size = (pcm.len() / 1000).max(1);
            for chunk in pcm.chunks(chunk_size) {
                waveform.push(chunk.iter().fold(0.0f32, |a, &b| a.max(b.abs())));
            }

            let total_samples = pcm.len();
            let sample_rate = params.sample_rate.unwrap_or(44100);
            let channels = params.channels.map(|c| c.count()).unwrap_or(2);

            let (beat_markers, bpm) = beats::detect_beats(&pcm, sample_rate, channels);

            let new_pcm_ptr = Box::into_raw(Box::new(pcm));
            let old_ptr = c.pcm_ptr.swap(new_pcm_ptr, Ordering::AcqRel);

            if !old_ptr.is_null() {
                unsafe {
                    drop(Box::from_raw(old_ptr));
                }
            }

            c.cursor.store(0, Ordering::SeqCst);
            c.loop_start.store(0, Ordering::SeqCst);
            c.loop_end.store(total_samples, Ordering::SeqCst);

            let mut s = s_ptr.lock().unwrap();
            s.total_samples = total_samples;
            s.sample_rate = sample_rate;
            s.channels = channels;
            s.waveform = waveform;
            s.beat_markers = beat_markers;
            s.bpm = bpm;

            c.is_loading.store(false, Ordering::SeqCst);
        });
    }

    fn start_playback(&mut self, rx: Receiver<ParamUpdate>) {
        let c = self.controls.clone();
        let host = cpal::default_host();
        let device = host.default_output_device().expect("No output device");
        let config = device.default_output_config().unwrap().config();

        let mut stretchers: Vec<Stretch> = (0..config.channels as usize)
            .map(|_| Stretch::preset_default(1, config.sample_rate.0))
            .collect();

        let mut input_scratch = vec![0.0f32; 8192];
        let mut output_scratch = vec![0.0f32; 8192];

        let mut local_speed = 1.0f32;
        let mut local_pitch = 1.0f32;

        let stream = device
            .build_output_stream(
                &config,
                move |data: &mut [f32], _| {
                    while let Ok(update) = rx.try_recv() {
                        match update {
                            ParamUpdate::Speed(s) => local_speed = s,
                            ParamUpdate::Pitch(p) => local_pitch = p,
                        }
                    }

                    if !c.is_playing.load(Ordering::Relaxed)
                        || c.is_loading.load(Ordering::Relaxed)
                        || c.is_seeking.load(Ordering::Relaxed)
                    {
                        data.fill(0.0);
                        return;
                    }

                    let pcm_ptr = c.pcm_ptr.load(Ordering::Acquire);
                    if pcm_ptr.is_null() {
                        data.fill(0.0);
                        return;
                    }

                    let pcm = unsafe { &*pcm_ptr };
                    if pcm.is_empty() {
                        data.fill(0.0);
                        return;
                    }

                    let cursor = c.cursor.load(Ordering::Relaxed);
                    let l_start = c.loop_start.load(Ordering::Relaxed);
                    let l_end = c.loop_end.load(Ordering::Relaxed);
                    let volume = f32::from_bits(c.volume.load(Ordering::Relaxed));
                    let channels = 2;

                    let stretch_ratio = 1.0 / local_speed;
                    let output_frames = data.len() / channels;
                    let input_frames_needed = (output_frames as f32 / stretch_ratio) as usize;

                    if cursor + (input_frames_needed * channels) < pcm.len()
                        && input_frames_needed < 8192
                    {
                        let mut active_cursor = cursor;
                        if active_cursor >= l_end && l_end > l_start {
                            active_cursor = l_start;
                        }

                        for ch in 0..channels {
                            stretchers[ch].set_transpose_factor(local_pitch, None);
                            for i in 0..input_frames_needed {
                                input_scratch[i] = pcm[active_cursor + (i * channels) + ch];
                            }
                            let mut output_view = &mut output_scratch[..output_frames];
                            stretchers[ch]
                                .process(&input_scratch[..input_frames_needed], &mut output_view);
                            for i in 0..output_frames {
                                data[i * channels + ch] = output_scratch[i] * volume;
                            }
                        }
                        c.cursor.store(
                            active_cursor + input_frames_needed * channels,
                            Ordering::Relaxed,
                        );
                    } else {
                        data.fill(0.0);
                    }
                },
                |e| eprintln!("{}", e),
                None,
            )
            .unwrap();

        stream.play().unwrap();
        self._stream = Some(stream);
    }
}

impl eframe::App for PlayerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let (file_path, total_samples, sample_rate, channels, waveform, beat_markers, bpm) = {
            let s = self.state.lock().unwrap();
            (
                s.file_path.clone(),
                s.total_samples,
                s.sample_rate,
                s.channels,
                s.waveform.clone(),
                s.beat_markers.clone(),
                s.bpm,
            )
        };

        if ctx.input(|i| i.key_pressed(egui::Key::Space)) {
            let p = self.controls.is_playing.load(Ordering::Relaxed);
            self.controls.is_playing.store(!p, Ordering::Relaxed);
        }

        let mut trigger_index: Option<usize> = None;

        // --- Nudge Keys: Keypad 1 and 3 (with and without NumLock) ---
        let mut nudge_left = ctx.input(|i| i.key_pressed(egui::Key::Num1) || i.key_pressed(egui::Key::End));
        let mut nudge_right = ctx.input(|i| i.key_pressed(egui::Key::Num3) || i.key_pressed(egui::Key::PageDown));

        // Also check text events for keypad with NumLock on
        ctx.input(|i| {
            for event in &i.events {
                if let egui::Event::Text(t) = event {
                    if let Some(digit) = t.chars().next().and_then(|c| c.to_digit(10)) {
                        if digit == 1 {
                            nudge_left = true;
                        } else if digit == 3 {
                            nudge_right = true;
                        }
                    }
                }
            }
        });

        if nudge_left || nudge_right {
            let mut s = self.state.lock().unwrap();
            if s.beat_markers.len() >= 2 {
                let bar_gap = s.beat_markers[1].saturating_sub(s.beat_markers[0]);
                let beat_nudge = bar_gap / 4;

                for marker in s.beat_markers.iter_mut() {
                    if nudge_left {
                        *marker = marker.saturating_sub(beat_nudge);
                    } else {
                        *marker += beat_nudge;
                    }
                }
                let total_frames = s.total_samples / s.channels.max(1);
                s.beat_markers.retain(|&m| m < total_frames);
            }
        }

        // Handle number row 0 and keypad 0 for rewind (with and without NumLock)
        // Keypad 0 with NumLock off sends Insert key
        if ctx.input(|i| i.key_pressed(egui::Key::Num0) || i.key_pressed(egui::Key::Insert)) {
            trigger_index = Some(0);
        }

        // Also check text events for '0' (keypad with NumLock on)
        ctx.input(|i| {
            for event in &i.events {
                if let egui::Event::Text(t) = event {
                    if let Some(digit) = t.chars().next().and_then(|c| c.to_digit(10)) {
                        if digit == 0 {
                            trigger_index = Some(0);
                        }
                    }
                }
            }
        });

        if let Some(i) = trigger_index
            && i == 0
        {
            self.controls.cursor.store(0, Ordering::Relaxed);
            self.controls.is_playing.store(true, Ordering::Relaxed);
        }

        // --- NAVIGATION & LOOPING ---
        if !ctx.input(|i| i.modifiers.command) {
            let current_cursor = self.controls.cursor.load(Ordering::Relaxed);
            let current_frame = current_cursor / channels.max(1);
            let l_start = self.controls.loop_start.load(Ordering::Relaxed);
            let l_end = self.controls.loop_end.load(Ordering::Relaxed);

            let is_looping = l_start > 0 || l_end < total_samples;

            // Mute audio when using Left/Right keys to navigate (if no loop is active)
            let is_navigating = ctx.input(|i| {
                i.key_down(egui::Key::ArrowLeft)
                    || i.key_down(egui::Key::ArrowRight)
                    || i.pointer.any_down() // Mutes during any mouse interaction/drag
            });

            // 2. Store the mute state
            self.controls
                .is_seeking
                .store(is_navigating, Ordering::SeqCst);

            // --- UP ARROW: Create or Increase Loop ---
            if ctx.input(|i| i.key_pressed(egui::Key::ArrowUp))
                && let Some(idx) = beat_markers.iter().position(|&m| m > current_frame)
            {
                let bar_start_frame = if idx > 0 { beat_markers[idx - 1] } else { 0 };

                if !is_looping {
                    let bar_end_frame = beat_markers[idx];
                    self.controls
                        .loop_start
                        .store(bar_start_frame * channels, Ordering::Relaxed);
                    self.controls
                        .loop_end
                        .store(bar_end_frame * channels, Ordering::Relaxed);
                    self.controls
                        .cursor
                        .store(bar_start_frame * channels, Ordering::Relaxed);
                } else {
                    let current_end_frame = l_end / channels;
                    if let Some(&next_bar_end) =
                        beat_markers.iter().find(|&&m| m > current_end_frame)
                    {
                        self.controls
                            .loop_end
                            .store(next_bar_end * channels, Ordering::Relaxed);
                    }
                }
            }

            // --- DOWN ARROW: Decrease or Clear Loop ---
            if ctx.input(|i| i.key_pressed(egui::Key::ArrowDown)) && is_looping {
                let current_end_frame = l_end / channels;
                let current_start_frame = l_start / channels;

                let prev_marker = beat_markers
                    .iter()
                    .rfind(|&&m| m < current_end_frame && m > current_start_frame);

                if let Some(&new_end) = prev_marker {
                    self.controls
                        .loop_end
                        .store(new_end * channels, Ordering::Relaxed);
                } else {
                    self.controls.loop_start.store(0, Ordering::Relaxed);
                    self.controls
                        .loop_end
                        .store(total_samples, Ordering::Relaxed);
                }
            }

            // --- LEFT ARROW: Move Loop or Cursor ---
            if ctx.input(|i| i.key_pressed(egui::Key::ArrowLeft)) {
                if is_looping {
                    let start_frame = l_start / channels;
                    let end_frame = l_end / channels;
                    if let Some(idx) = beat_markers.iter().position(|&m| m >= start_frame)
                        && idx > 0
                    {
                        let diff = start_frame.saturating_sub(beat_markers[idx - 1]);
                        self.controls
                            .loop_start
                            .store(beat_markers[idx - 1] * channels, Ordering::Relaxed);
                        self.controls.loop_end.store(
                            (end_frame.saturating_sub(diff)) * channels,
                            Ordering::Relaxed,
                        );
                    }
                } else {
                    let current_marker_idx = beat_markers.iter().position(|&m| m >= current_frame);
                    match current_marker_idx {
                        Some(i) if i >= 2 => {
                            let target_frame = beat_markers[i - 2];
                            self.controls
                                .cursor
                                .store(target_frame * channels, Ordering::Relaxed);
                        }
                        _ => {
                            self.controls.cursor.store(0, Ordering::Relaxed);
                        }
                    }
                }
            }

            // --- RIGHT ARROW: Move Loop or Cursor ---
            if ctx.input(|i| i.key_pressed(egui::Key::ArrowRight)) {
                if is_looping {
                    let start_frame = l_start / channels;
                    let end_frame = l_end / channels;

                    // Find the next beat marker relative to the current loop start
                    if let Some(&next_start) = beat_markers.iter().find(|&&m| m > start_frame) {
                        let diff = next_start - start_frame;

                        // Check if moving the loop right stays within file bounds
                        if (end_frame + diff) * channels <= total_samples {
                            let new_start_samples = next_start * channels;

                            // 1. Move the loop boundaries
                            self.controls
                                .loop_start
                                .store(new_start_samples, Ordering::Relaxed);
                            self.controls
                                .loop_end
                                .store((end_frame + diff) * channels, Ordering::Relaxed);

                            // 2. Move the cursor with the loop (Aligning behavior with Left Arrow)
                            self.controls
                                .cursor
                                .store(new_start_samples, Ordering::Relaxed);
                        }
                    }
                } else {
                    // Standard navigation when not looping
                    let right_buffer = (sample_rate as usize / 20).max(500);
                    let next_marker = beat_markers
                        .iter()
                        .find(|&&m| m > current_frame + right_buffer);
                    if let Some(&marker_frame) = next_marker {
                        self.controls
                            .cursor
                            .store(marker_frame * channels, Ordering::Relaxed);
                    }
                }
            }
        }

        // --- GLOBAL HOTKEYS ---
        if ctx.input(|i| i.key_pressed(egui::Key::Q) || i.key_pressed(egui::Key::Escape)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        if ctx.input(|i| i.key_pressed(egui::Key::R)) {
            self.controls
                .speed
                .store(1.0f32.to_bits(), Ordering::Relaxed);
            self.controls
                .pitch
                .store(1.0f32.to_bits(), Ordering::Relaxed);
            let _ = self.tx.send(ParamUpdate::Speed(1.0));
            let _ = self.tx.send(ParamUpdate::Pitch(1.0));
        }

        if ctx.input(|i| i.key_pressed(egui::Key::C)) {
            self.controls.loop_start.store(0, Ordering::Relaxed);
            self.controls
                .loop_end
                .store(total_samples, Ordering::Relaxed);
        }

        // Command + Arrows for nudging loop position
        if ctx.input(|i| i.modifiers.command) {
            let l_start = self.controls.loop_start.load(Ordering::Relaxed);
            let l_end = self.controls.loop_end.load(Ordering::Relaxed);
            let loop_width = l_end.saturating_sub(l_start);

            let s = self.state.lock().unwrap();
            if s.beat_markers.len() >= 2 {
                let bar_gap_samples = (s.beat_markers[1] - s.beat_markers[0]) * s.channels;

                if bar_gap_samples > 0 {
                    if ctx.input(|i| i.key_pressed(egui::Key::ArrowLeft)) {
                        let new_start = l_start.saturating_sub(bar_gap_samples);
                        self.controls.loop_start.store(new_start, Ordering::Relaxed);
                        self.controls
                            .loop_end
                            .store(new_start + loop_width, Ordering::Relaxed);
                    }

                    if ctx.input(|i| i.key_pressed(egui::Key::ArrowRight)) {
                        let mut new_start = l_start + bar_gap_samples;
                        if new_start + loop_width > s.total_samples {
                            new_start = s.total_samples.saturating_sub(loop_width);
                        }
                        new_start = (new_start / s.channels) * s.channels;
                        self.controls.loop_start.store(new_start, Ordering::Relaxed);
                        self.controls
                            .loop_end
                            .store(new_start + loop_width, Ordering::Relaxed);
                    }
                }
            }
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            if self.controls.is_loading.load(Ordering::Relaxed) {
                ui.centered_and_justified(|ui| ui.label("Loading..."));
                return;
            }

            ui.vertical_centered(|ui| {
                ui.add_space(10.0);
                let is_loading = self.controls.is_loading.load(Ordering::Relaxed);

                ui.add_enabled_ui(!is_loading, |ui| {
                    if ui.button("Open File").clicked() {
                        // 1. Open the dialog (this blocks the UI thread)
                        if let Some(path) = FileDialog::new().pick_file() {
                            // 2. If a file was picked, load_audio_file handles setting/clearing is_loading
                            self.load_audio_file(path);
                        } else {
                            // 3. If cancelled, ensure we aren't stuck in "loading"
                            self.controls.is_loading.store(false, Ordering::SeqCst);
                        }
                    }
                });

                let current_cursor = self.controls.cursor.load(Ordering::Relaxed);
                let sample_div = (sample_rate as f32 * channels as f32).max(1.0);

                ui.add_space(10.0);
                ui.label(&file_path);
                ui.label(format!(
                    "{:.2}s : {:.2}s",
                    current_cursor as f32 / sample_div,
                    total_samples as f32 / sample_div
                ));
                ui.label(format!("{:.1} BPM", bpm));

                let full_width = ui.available_width();

                let (rect, response) = ui.allocate_at_least(
                    egui::vec2(ui.available_width(), 100.0),
                    egui::Sense::click_and_drag(),
                );

                let mut l_start = self.controls.loop_start.load(Ordering::Relaxed);
                let mut l_end = self.controls.loop_end.load(Ordering::Relaxed);
                let total = total_samples.max(1);

                let start_x = rect.left() + (l_start as f32 / total as f32) * rect.width();
                let end_x = rect.left() + (l_end as f32 / total as f32) * rect.width();

                // Mouse interaction for seeking and defining loops
                if response.clicked()
                    && let Some(pointer) = response.interact_pointer_pos()
                {
                    let total_frames = total_samples / channels.max(1);
                    let clicked_frame = (((pointer.x - rect.left()) / rect.width()).clamp(0.0, 1.0)
                        * total_frames as f32) as usize;

                    if response.double_clicked() || response.triple_clicked() {
                        let mut start_marker = 0;
                        let mut end_marker_idx = 0;
                        for (i, _marker) in beat_markers.iter().enumerate() {
                            if beat_markers[i] <= clicked_frame {
                                start_marker = beat_markers[i];
                                end_marker_idx = i + 1;
                            } else {
                                break;
                            }
                        }
                        let bars_to_loop = if response.triple_clicked() { 2 } else { 1 };
                        let target_end_idx =
                            (end_marker_idx + (bars_to_loop - 1)).min(beat_markers.len());
                        let end_marker = if target_end_idx < beat_markers.len() {
                            beat_markers[target_end_idx]
                        } else {
                            total_frames
                        };

                        l_start = start_marker * channels;
                        l_end = end_marker * channels;
                        self.controls.loop_start.store(l_start, Ordering::Relaxed);
                        self.controls.loop_end.store(l_end, Ordering::Relaxed);
                        self.controls.cursor.store(l_start, Ordering::Relaxed);
                    }
                }

                if response.drag_started() || response.clicked() {
                    self.controls.is_seeking.store(true, Ordering::Relaxed);
                }

                if let Some(pointer) = response.interact_pointer_pos() {
                    let is_near_start = (pointer.x - start_x).abs() < 12.0;
                    let is_near_end = (pointer.x - end_x).abs() < 12.0;

                    if response.drag_started() || response.clicked() {
                        if is_near_start {
                            self.dragging_marker = Some(true);
                        } else if is_near_end {
                            self.dragging_marker = Some(false);
                        } else {
                            self.dragging_marker = None;
                            let val = (((pointer.x - rect.left()) / rect.width()).clamp(0.0, 1.0)
                                * total as f32) as usize;
                            self.controls
                                .cursor
                                .store(val - (val % channels.max(1)), Ordering::Relaxed);
                        }
                    }
                }

                if response.dragged()
                    && let Some(pointer) = response.interact_pointer_pos()
                {
                    let val = (((pointer.x - rect.left()) / rect.width()).clamp(0.0, 1.0)
                        * total as f32) as usize;
                    let val = (val / channels.max(1)) * channels.max(1);

                    if ctx.input(|i| i.modifiers.command) && self.dragging_marker.is_some() {
                        let width = l_end.saturating_sub(l_start);

                        let (new_start, new_end) = if self.dragging_marker == Some(true) {
                            // Dragging START: Move start to mouse, end follows
                            let s = val.min(total_samples.saturating_sub(width));
                            (s, s + width)
                        } else {
                            // Dragging END: Move end to mouse, start follows
                            let e = val.max(width).min(total_samples);
                            (e - width, e)
                        };

                        // 1. Update Loop Boundaries
                        self.controls.loop_start.store(new_start, Ordering::Relaxed);
                        self.controls.loop_end.store(new_end, Ordering::Relaxed);

                        // 2. MOVE CURSOR: Force the playhead to the new start of the loop
                        self.controls.cursor.store(new_start, Ordering::Relaxed);
                    } else if self.dragging_marker == Some(true) {
                        self.controls.loop_start.store(val, Ordering::Relaxed);
                    } else if self.dragging_marker == Some(false) {
                        self.controls.loop_end.store(val, Ordering::Relaxed);
                    } else {
                        // Standard non-modifier drag: just move the cursor
                        self.controls.cursor.store(val, Ordering::Relaxed);
                    }
                }

                if response.drag_stopped() || response.clicked() {
                    self.controls.is_seeking.store(false, Ordering::Relaxed);
                }

                // Drawing Waveform and Overlays
                ui.painter()
                    .rect_filled(rect, 2.0, egui::Color32::from_rgb(10, 10, 10));

                if l_start > 0 || l_end < total_samples {
                    let loop_draw_start =
                        rect.left() + (l_start as f32 / total as f32) * rect.width();
                    let loop_draw_end = rect.left() + (l_end as f32 / total as f32) * rect.width();
                    let loop_rect = egui::Rect::from_x_y_ranges(
                        loop_draw_start..=loop_draw_end,
                        rect.top()..=rect.bottom(),
                    );
                    ui.painter().rect_filled(
                        loop_rect,
                        0.0,
                        egui::Color32::from_rgba_unmultiplied(0, 255, 0, 30),
                    );
                }

                if !waveform.is_empty() {
                    let wave_color = egui::Color32::from_rgb(0, 180, 100);
                    let bar_width = (rect.width() / waveform.len() as f32).max(1.0);
                    for (i, &peak) in waveform.iter().enumerate() {
                        let x = rect.left() + (i as f32 / waveform.len() as f32) * rect.width();
                        let h = (peak * rect.height() * 0.45).max(1.0);
                        ui.painter().line_segment(
                            [
                                egui::pos2(x, rect.center().y - h),
                                egui::pos2(x, rect.center().y + h),
                            ],
                            egui::Stroke::new(bar_width, wave_color),
                        );
                    }
                }

                let beat_color = egui::Color32::from_rgb(255, 100, 0);
                for &beat_frame in &beat_markers {
                    let bx = rect.left()
                        + (beat_frame as f32 / (total_samples / channels) as f32) * rect.width();
                    if bx >= rect.left() && bx <= rect.right() {
                        ui.painter().line_segment(
                            [egui::pos2(bx, rect.top()), egui::pos2(bx, rect.bottom())],
                            egui::Stroke::new(1.0, beat_color.linear_multiply(0.5)),
                        );
                    }
                }

                let cur_x = rect.left()
                    + ((current_cursor / channels) as f32 / (total_samples / channels) as f32)
                        * rect.width();
                ui.painter().line_segment(
                    [
                        egui::pos2(cur_x, rect.top()),
                        egui::pos2(cur_x, rect.bottom()),
                    ],
                    (1.5, egui::Color32::WHITE),
                );

                ui.add_space(15.0);
                ui.spacing_mut().slider_width = full_width - 60.0;

                ui.label("Speed");
                let mut speed = f32::from_bits(self.controls.speed.load(Ordering::Relaxed));
                if ui
                    .add(
                        egui::Slider::new(&mut speed, 0.25..=4.0)
                            .logarithmic(true)
                            .suffix("x")
                            .max_decimals(1),
                    )
                    .changed()
                {
                    self.controls
                        .speed
                        .store(speed.to_bits(), Ordering::Relaxed);
                    let _ = self.tx.send(ParamUpdate::Speed(speed));
                }

                ui.label("Pitch");
                let mut pitch = f32::from_bits(self.controls.pitch.load(Ordering::Relaxed));
                if ui
                    .add(
                        egui::Slider::new(&mut pitch, 0.5..=2.0)
                            .logarithmic(true)
                            .suffix("x"),
                    )
                    .changed()
                {
                    self.controls
                        .pitch
                        .store(pitch.to_bits(), Ordering::Relaxed);
                    let _ = self.tx.send(ParamUpdate::Pitch(pitch));
                }

                ui.label("Volume");
                let mut vol = f32::from_bits(self.controls.volume.load(Ordering::Relaxed));
                if ui
                    .add(egui::Slider::new(&mut vol, 0.0..=2.0).max_decimals(1))
                    .changed()
                {
                    self.controls.volume.store(vol.to_bits(), Ordering::Relaxed);
                }

                ui.add_space(20.0);
                ui.horizontal(|ui| {
                    let is_p = self.controls.is_playing.load(Ordering::Relaxed);
                    if ui.button(if is_p { "Pause" } else { "Play" }).clicked() {
                        self.controls.is_playing.store(!is_p, Ordering::Relaxed);
                    }
                    if ui.button("(R)eset").clicked() {
                        self.controls
                            .speed
                            .store(1.0f32.to_bits(), Ordering::Relaxed);
                        self.controls
                            .pitch
                            .store(1.0f32.to_bits(), Ordering::Relaxed);
                        let _ = self.tx.send(ParamUpdate::Speed(1.0));
                        let _ = self.tx.send(ParamUpdate::Pitch(1.0));
                    }
                    ui.separator();
                    if ui.button("(C)lear Loop").clicked() {
                        self.controls.loop_start.store(0, Ordering::Relaxed);
                        self.controls
                            .loop_end
                            .store(total_samples, Ordering::Relaxed);
                    }
                    ui.separator();
                    let current_l_start = self.controls.loop_start.load(Ordering::Relaxed);
                    let current_l_end = self.controls.loop_end.load(Ordering::Relaxed);
                    if current_l_start > 0 || current_l_end < total_samples {
                        ui.label(
                            egui::RichText::new("LOOP ACTIVE")
                                .color(egui::Color32::GREEN)
                                .strong(),
                        );
                    } else {
                        ui.label("No Loop");
                    }
                });
            });
        });
        ctx.request_repaint();
    }
}

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let initial_path = args.get(1).map(PathBuf::from);
    eframe::run_native(
        "Reh",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([550.0, 450.0])
                .with_min_inner_size([300.0, 300.0]),
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(PlayerApp::new(cc, initial_path)))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_beat_detection_logic() {
        // Create a 1-second silent stereo buffer
        let sample_rate = 44100;
        let pcm = vec![0.0; sample_rate * 2];

        let (markers, bpm) = beats::detect_beats(&pcm, sample_rate as u32, 2);

        // Ensure it handles silence without crashing
        assert!(bpm >= 0.0);
        // Ensure markers are within bounds
        for &m in &markers {
            assert!(m < sample_rate);
        }
    }
}
