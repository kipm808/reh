use eframe::egui;
use rfd::FileDialog;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use signalsmith_stretch::Stretch;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, AtomicUsize, AtomicU32, Ordering};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::probe::Hint;
use std::thread;
use std::path::PathBuf;
use crossbeam_channel::{unbounded, Receiver, Sender};

struct AppState {
    file_path: String,
    total_samples: usize,
    sample_rate: u32,
    channels: usize,
    waveform: Vec<f32>,
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
    is_seeking: AtomicBool, // Restored to prevent chirping
    pcm_data: Mutex<Arc<Vec<f32>>>, 
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
            pcm_data: Mutex::new(Arc::new(Vec::new())),
        });

        let state = Arc::new(Mutex::new(AppState {
            file_path: "No file selected".to_string(),
            total_samples: 0,
            sample_rate: 44100,
            channels: 2,
            waveform: Vec::new(),
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
        if !path.exists() { return; }
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
                Err(_) => { c.is_loading.store(false, Ordering::SeqCst); return; }
            };
            let mss = MediaSourceStream::new(Box::new(file), Default::default());
            let mut hint = Hint::new();
            if let Some(ext) = path.extension() { hint.with_extension(&ext.to_string_lossy()); }
            
            let probed = symphonia::default::get_probe().format(&hint, mss, &Default::default(), &Default::default());
            let mut format = match probed {
                Ok(p) => p.format,
                Err(_) => { c.is_loading.store(false, Ordering::SeqCst); return; }
            };
            let track = match format.default_track() {
                Some(t) => t,
                None => { c.is_loading.store(false, Ordering::SeqCst); return; }
            };

            let params = track.codec_params.clone();
            let mut decoder = symphonia::default::get_codecs().make(&params, &Default::default()).unwrap();
            let mut pcm = Vec::new();

            while let Ok(packet) = format.next_packet() {
                if let Ok(decoded) = decoder.decode(&packet) {
                    let mut sb = SampleBuffer::<f32>::new(decoded.capacity() as u64, *decoded.spec());
                    sb.copy_interleaved_ref(decoded);
                    pcm.extend_from_slice(sb.samples());
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

            c.cursor.store(0, Ordering::SeqCst);
            c.loop_start.store(0, Ordering::SeqCst);
            c.loop_end.store(total_samples, Ordering::SeqCst);
            *c.pcm_data.lock().unwrap() = Arc::new(pcm);

            let mut s = s_ptr.lock().unwrap();
            s.total_samples = total_samples;
            s.sample_rate = sample_rate;
            s.channels = channels;
            s.waveform = waveform;
            
            c.is_loading.store(false, Ordering::SeqCst);
        });
    }

    fn start_playback(&mut self, rx: Receiver<ParamUpdate>) {
        let c = self.controls.clone();
        let host = cpal::default_host();
        let device = host.default_output_device().expect("No output device");
        let config = device.default_output_config().unwrap().config();
        
        let mut stretchers: Vec<Stretch> = (0..config.channels as usize)
            .map(|_| Stretch::preset_default(1, config.sample_rate.0)).collect();
        
        let mut input_scratch = vec![0.0f32; 8192];
        let mut output_scratch = vec![0.0f32; 8192];

        let mut local_speed = 1.0f32;
        let mut local_pitch = 1.0f32;

        let stream = device.build_output_stream(&config, move |data: &mut [f32], _| {
            while let Ok(update) = rx.try_recv() {
                match update {
                    ParamUpdate::Speed(s) => local_speed = s,
                    ParamUpdate::Pitch(p) => local_pitch = p,
                }
            }

            // Mute during seeking, loading, or if paused
            if !c.is_playing.load(Ordering::Relaxed) || 
               c.is_loading.load(Ordering::Relaxed) || 
               c.is_seeking.load(Ordering::Relaxed) {
                data.fill(0.0);
                return;
            }

            let pcm = Arc::clone(&*c.pcm_data.lock().unwrap());
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

            if cursor + (input_frames_needed * channels) < pcm.len() && input_frames_needed < 8192 {
                let mut active_cursor = cursor;
                if active_cursor >= l_end && l_end > l_start { active_cursor = l_start; }

                for ch in 0..channels {
                    stretchers[ch].set_transpose_factor(local_pitch, None);
                    for i in 0..input_frames_needed { 
                        input_scratch[i] = pcm[active_cursor + (i * channels) + ch]; 
                    }
                    let mut output_view = &mut output_scratch[..output_frames];
                    stretchers[ch].process(&input_scratch[..input_frames_needed], &mut output_view);
                    for i in 0..output_frames { 
                        data[i * channels + ch] = output_scratch[i] * volume; 
                    }
                }
                c.cursor.store(active_cursor + input_frames_needed * channels, Ordering::Relaxed);
            } else {
                data.fill(0.0);
            }
        }, |e| eprintln!("{}", e), None).unwrap();

        stream.play().unwrap();
        self._stream = Some(stream);
    }
}

impl eframe::App for PlayerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let (file_path, total_samples, sample_rate, channels, waveform) = {
            let s = self.state.lock().unwrap();
            (s.file_path.clone(), s.total_samples, s.sample_rate, s.channels, s.waveform.clone())
        };

        // Keyboard Shortcuts
        if ctx.input(|i| i.key_pressed(egui::Key::Space)) {
            let p = self.controls.is_playing.load(Ordering::Relaxed);
            self.controls.is_playing.store(!p, Ordering::Relaxed);
        }

        // quit keys
        if ctx.input(|i| i.key_pressed(egui::Key::Q) || i.key_pressed(egui::Key::Escape)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        // reset key
        if ctx.input(|i| i.key_pressed(egui::Key::R)) {
            self.controls.speed.store(1.0f32.to_bits(), Ordering::Relaxed);
            self.controls.pitch.store(1.0f32.to_bits(), Ordering::Relaxed);
            let _ = self.tx.send(ParamUpdate::Speed(1.0));
            let _ = self.tx.send(ParamUpdate::Pitch(1.0));
        }

        // loop clear key
        if ctx.input(|i| i.key_pressed(egui::Key::C)) {
            self.controls.loop_start.store(0, Ordering::Relaxed);
            self.controls.loop_end.store(total_samples, Ordering::Relaxed);
        }

        // loop keys
        if ctx.input(|i| i.key_pressed(egui::Key::OpenBracket)) {
            self.controls.loop_start.store(self.controls.cursor.load(Ordering::Relaxed), Ordering::Relaxed);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::CloseBracket)) {
            self.controls.loop_end.store(self.controls.cursor.load(Ordering::Relaxed), Ordering::Relaxed);
        }

        // ctl arrow seeking
        if ctx.input(|i| i.modifiers.command) {
            let l_start = self.controls.loop_start.load(Ordering::Relaxed);
            let l_end = self.controls.loop_end.load(Ordering::Relaxed);
            let width = l_end.saturating_sub(l_start);
            if ctx.input(|i| i.key_pressed(egui::Key::ArrowLeft)) {
                let shift = l_start.min(width);
                self.controls.loop_start.store(l_start - shift, Ordering::Relaxed);
                self.controls.loop_end.store(l_end - shift, Ordering::Relaxed);
            }
            if ctx.input(|i| i.key_pressed(egui::Key::ArrowRight)) {
                let shift = (total_samples.saturating_sub(l_end)).min(width);
                self.controls.loop_start.store(l_start + shift, Ordering::Relaxed);
                self.controls.loop_end.store(l_end + shift, Ordering::Relaxed);
            }
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            if self.controls.is_loading.load(Ordering::Relaxed) {
                ui.centered_and_justified(|ui| ui.label("Loading..."));
                return;
            }

            ui.vertical_centered(|ui| {
                ui.add_space(10.0);
                if ui.button("Open File").clicked() {
                    if let Some(path) = FileDialog::new().pick_file() { 
                        self.load_audio_file(path); 
                    }
                }

                let current_cursor = self.controls.cursor.load(Ordering::Relaxed);
                let sample_div = (sample_rate as f32 * channels as f32).max(1.0);
                
                ui.add_space(10.0);
                ui.label(&file_path);
                ui.label(format!("{:.2}s : {:.2}s", current_cursor as f32 / sample_div, total_samples as f32 / sample_div));

                let full_width = ui.available_width();
                let (rect, response) = ui.allocate_at_least(egui::vec2(full_width, 100.0), egui::Sense::click_and_drag());
                
                let mut l_start = self.controls.loop_start.load(Ordering::Relaxed);
                let mut l_end = self.controls.loop_end.load(Ordering::Relaxed);
                let total = total_samples.max(1);

                let start_x = rect.left() + (l_start as f32 / total as f32) * rect.width();
                let end_x = rect.left() + (l_end as f32 / total as f32) * rect.width();

                if response.drag_started() || response.clicked() {
                    self.controls.is_seeking.store(true, Ordering::Relaxed);
                }

                if let Some(pointer) = response.interact_pointer_pos() {
                    let is_near_start = (pointer.x - start_x).abs() < 12.0;
                    let is_near_end = (pointer.x - end_x).abs() < 12.0;

                    if response.drag_started() || response.clicked() {
                        if is_near_start { self.dragging_marker = Some(true); }
                        else if is_near_end { self.dragging_marker = Some(false); }
                        else {
                            self.dragging_marker = None;
                            let val = (((pointer.x - rect.left()) / rect.width()).clamp(0.0, 1.0) * total as f32) as usize;
                            self.controls.cursor.store(val - (val % channels.max(1)), Ordering::Relaxed);
                        }
                    }
                }

                if response.dragged() {
                    if let Some(pointer) = response.interact_pointer_pos() {
                        let val = (((pointer.x - rect.left()) / rect.width()).clamp(0.0, 1.0) * total as f32) as usize;
                        let val = val - (val % channels.max(1));
                        
                        // ctl-drag loop markers
                        if ctx.input(|i| i.modifiers.command) && self.dragging_marker.is_some() {
                            let width = l_end.saturating_sub(l_start);
                            if self.dragging_marker == Some(true) {
                                l_start = val.min(total_samples.saturating_sub(width));
                                l_end = l_start + width;
                            } else {
                                l_end = val.max(width);
                                l_start = l_end - width;
                            }
                            self.controls.loop_start.store(l_start, Ordering::Relaxed);
                            self.controls.loop_end.store(l_end, Ordering::Relaxed);
                        } else {
                            if self.dragging_marker == Some(true) { self.controls.loop_start.store(val, Ordering::Relaxed); }
                            else if self.dragging_marker == Some(false) { self.controls.loop_end.store(val, Ordering::Relaxed); }
                            else { self.controls.cursor.store(val, Ordering::Relaxed); }
                        }
                    }
                }

                if response.drag_stopped() || response.clicked() {
                    self.controls.is_seeking.store(false, Ordering::Relaxed);
                }

                ui.painter().rect_filled(rect, 2.0, egui::Color32::from_rgb(10, 10, 10));
                if l_start > 0 || l_end < total_samples {
                    let loop_rect = egui::Rect::from_x_y_ranges(start_x..=end_x, rect.top()..=rect.bottom());
                    ui.painter().rect_filled(loop_rect, 0.0, egui::Color32::from_rgba_unmultiplied(0, 255, 0, 30));
                }

                if !waveform.is_empty() {
                    let wave_color = egui::Color32::from_rgb(0, 180, 100);
                    let bar_width = (rect.width() / waveform.len() as f32).max(1.0);
                    for (i, &peak) in waveform.iter().enumerate() {
                        let x = rect.left() + (i as f32 / waveform.len() as f32) * rect.width();
                        let h = (peak * rect.height() * 0.45).max(1.0);
                        ui.painter().line_segment([egui::pos2(x, rect.center().y - h), egui::pos2(x, rect.center().y + h)], egui::Stroke::new(bar_width, wave_color));
                    }
                }

                let cur_x = rect.left() + (current_cursor as f32 / total as f32) * rect.width();
                ui.painter().line_segment([egui::pos2(cur_x, rect.top()), egui::pos2(cur_x, rect.bottom())], (1.5, egui::Color32::WHITE));
                ui.painter().line_segment([egui::pos2(start_x, rect.top()), egui::pos2(start_x, rect.bottom())], (2.0, egui::Color32::YELLOW));
                ui.painter().line_segment([egui::pos2(end_x, rect.top()), egui::pos2(end_x, rect.bottom())], (2.0, egui::Color32::from_rgb(50, 80, 255)));

                ui.add_space(15.0);
                ui.spacing_mut().slider_width = full_width - 60.0;

                ui.label("Speed");
                let mut speed = f32::from_bits(self.controls.speed.load(Ordering::Relaxed));
                if ui.add(egui::Slider::new(&mut speed, 0.25..=4.0).logarithmic(true).suffix("x")).changed() {
                    self.controls.speed.store(speed.to_bits(), Ordering::Relaxed);
                    let _ = self.tx.send(ParamUpdate::Speed(speed));
                }

                ui.label("Pitch");
                let mut pitch = f32::from_bits(self.controls.pitch.load(Ordering::Relaxed));
                if ui.add(egui::Slider::new(&mut pitch, 0.5..=2.0).logarithmic(true).suffix("x")).changed() {
                    self.controls.pitch.store(pitch.to_bits(), Ordering::Relaxed);
                    let _ = self.tx.send(ParamUpdate::Pitch(pitch));
                }

                ui.label("Volume");
                let mut vol = f32::from_bits(self.controls.volume.load(Ordering::Relaxed));
                if ui.add(egui::Slider::new(&mut vol, 0.0..=2.0)).changed() {
                    self.controls.volume.store(vol.to_bits(), Ordering::Relaxed);
                }

                ui.add_space(10.0);

                ui.horizontal(|ui| {
                    let is_p = self.controls.is_playing.load(Ordering::Relaxed);
                    if ui.button(if is_p { "Pause" } else { "Play" }).clicked() { self.controls.is_playing.store(!is_p, Ordering::Relaxed); }
                    if ui.button("Reset").clicked() {
                        self.controls.speed.store(1.0f32.to_bits(), Ordering::Relaxed);
                        self.controls.pitch.store(1.0f32.to_bits(), Ordering::Relaxed);
                        let _ = self.tx.send(ParamUpdate::Speed(1.0));
                        let _ = self.tx.send(ParamUpdate::Pitch(1.0));
                    }
                    ui.separator();
                    if ui.button("[ Set Start").clicked() { self.controls.loop_start.store(current_cursor, Ordering::Relaxed); }
                    if ui.button("] Set End").clicked() { self.controls.loop_end.store(current_cursor, Ordering::Relaxed); }
                    if ui.button("Clear Loop").clicked() { 
                        self.controls.loop_start.store(0, Ordering::Relaxed); 
                        self.controls.loop_end.store(total_samples, Ordering::Relaxed); 
                    }
                    ui.separator();
                    ui.label(format!("Loop: {:.2}s - {:.2}s", l_start as f32 / sample_div, l_end as f32 / sample_div));
                });
            });
        });
        ctx.request_repaint();
    }
}

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let initial_path = args.get(1).map(PathBuf::from);
    eframe::run_native("Reh", eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([550.0, 350.0])
            .with_min_inner_size([300.0, 200.0]),
        ..Default::default()
    }, Box::new(|cc| Ok(Box::new(PlayerApp::new(cc, initial_path)))))
}
