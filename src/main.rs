use eframe::egui;
use rfd::FileDialog;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use signalsmith_stretch::Stretch;
use std::sync::{Arc, Mutex};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::probe::Hint;
use std::thread;
use std::path::PathBuf;

struct SharedSettings {
    speed: f32,
    pitch: f32,
    volume: f32,
    cursor: usize,
    total_samples: usize,
    loop_start: usize,
    loop_end: usize,
    is_seeking: bool,
    is_playing: bool,
    pcm_data: Vec<f32>,
    waveform: Vec<f32>,
    sample_rate: u32,
    channels: usize,
}

struct PlayerApp {
    settings: Arc<Mutex<SharedSettings>>,
    file_path: String,
    is_loading: Arc<Mutex<bool>>,
    dragging_marker: Option<bool>, 
    _stream: Option<cpal::Stream>,
}

impl PlayerApp {
    fn new(_cc: &eframe::CreationContext<'_>, initial_path: Option<PathBuf>) -> Self {
        let mut app = Self {
            settings: Arc::new(Mutex::new(SharedSettings {
                speed: 1.0, pitch: 1.0, volume: 1.0,
                cursor: 0, total_samples: 0,
                loop_start: 0, loop_end: 0,
                is_seeking: false, is_playing: true,
                pcm_data: Vec::new(), waveform: Vec::new(),
                sample_rate: 44100, channels: 2,
            })),
            file_path: "No file selected".to_string(),
            is_loading: Arc::new(Mutex::new(false)),
            dragging_marker: None,
            _stream: None,
        };

        if let Some(path) = initial_path {
            app.load_audio_file(path);
        }
        app
    }

    fn load_audio_file(&mut self, path: PathBuf) {
        if !path.exists() { return; }
        self.file_path = path.to_string_lossy().into_owned();
        let s_ptr = self.settings.clone();
        let l_ptr = self.is_loading.clone();
        *l_ptr.lock().unwrap() = true;

        thread::spawn(move || {
            let file = match std::fs::File::open(&path) {
                Ok(f) => f,
                Err(_) => { *l_ptr.lock().unwrap() = false; return; }
            };
            let mss = MediaSourceStream::new(Box::new(file), Default::default());
            let mut hint = Hint::new();
            if let Some(ext) = path.extension() { hint.with_extension(&ext.to_string_lossy()); }
            let probed = symphonia::default::get_probe().format(&hint, mss, &Default::default(), &Default::default());
            let mut format = match probed {
                Ok(p) => p.format,
                Err(_) => { *l_ptr.lock().unwrap() = false; return; }
            };
            let track = match format.default_track() {
                Some(t) => t,
                None => { *l_ptr.lock().unwrap() = false; return; }
            };
            let params = track.codec_params.clone();
            let mut decoder = match symphonia::default::get_codecs().make(&params, &Default::default()) {
                Ok(d) => d,
                Err(_) => { *l_ptr.lock().unwrap() = false; return; }
            };
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
            let mut s = s_ptr.lock().unwrap();
            s.pcm_data = pcm; 
            s.waveform = waveform; 
            s.total_samples = s.pcm_data.len();
            s.loop_start = 0; 
            s.loop_end = s.total_samples;
            s.sample_rate = params.sample_rate.unwrap_or(44100);
            s.channels = params.channels.map(|c| c.count()).unwrap_or(2);
            s.cursor = 0; 
           *l_ptr.lock().unwrap() = false;
        });
        if self._stream.is_none() { self.start_playback(); }
    }

    fn start_playback(&mut self) {
        let settings_ptr = self.settings.clone();
        let host = cpal::default_host();
        let device = host.default_output_device().expect("No output device");
        let config = device.default_output_config().unwrap().config();
        let mut stretchers: Vec<Stretch> = (0..config.channels as usize)
            .map(|_| Stretch::preset_default(1, config.sample_rate.0)).collect();
        let mut input_scratch = vec![0.0f32; 8192];
        let mut output_scratch = vec![0.0f32; 8192];
        let stream = device.build_output_stream(&config, move |data: &mut [f32], _| {
            let mut s = settings_ptr.lock().unwrap();
            if !s.is_playing || s.pcm_data.is_empty() || s.is_seeking { 
                data.fill(0.0); 
                return; 
            }
            let stretch_ratio = 1.0 / s.speed; 
            let output_frames = data.len() / s.channels;
            let input_frames_needed = (output_frames as f32 / stretch_ratio) as usize;
            if s.cursor + (input_frames_needed * s.channels) < s.pcm_data.len() && input_frames_needed < 8192 {
                if s.cursor >= s.loop_end && s.loop_end > s.loop_start { s.cursor = s.loop_start; }
                for ch in 0..s.channels {
                    stretchers[ch].set_transpose_factor(s.pitch, None);
                    for i in 0..input_frames_needed { input_scratch[i] = s.pcm_data[s.cursor + (i * s.channels) + ch]; }
                    let mut output_view = &mut output_scratch[..output_frames];
                    stretchers[ch].process(&input_scratch[..input_frames_needed], &mut output_view);
                    for i in 0..output_frames { data[i * s.channels + ch] = output_scratch[i] * s.volume; }
                }
                s.cursor += input_frames_needed * s.channels;
            } else { data.fill(0.0); }
        }, |e| eprintln!("{}", e), None).unwrap();
        stream.play().unwrap();
        self._stream = Some(stream);
    }
}

impl eframe::App for PlayerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let mut s = self.settings.lock().unwrap();
        
        if ctx.input(|i| i.key_pressed(egui::Key::Space)) { s.is_playing = !s.is_playing; }
        if ctx.input(|i| i.key_pressed(egui::Key::OpenBracket)) { s.loop_start = s.cursor; }
        if ctx.input(|i| i.key_pressed(egui::Key::CloseBracket)) { s.loop_end = s.cursor; }
        if ctx.input(|i| i.key_pressed(egui::Key::Num0)) { s.cursor = 0; }
        
        let back_keys = [
            (egui::Key::Num1, 1), (egui::Key::Num2, 2), (egui::Key::Num3, 3),
            (egui::Key::Num4, 4), (egui::Key::Num5, 5), (egui::Key::Num6, 6),
            (egui::Key::Num7, 7), (egui::Key::Num8, 8), (egui::Key::Num9, 9),
        ];
        for (key, seconds) in back_keys {
            if ctx.input(|i| i.key_pressed(key)) {
                let jump = seconds * s.sample_rate as usize * s.channels;
                s.cursor = s.cursor.saturating_sub(jump);
            }
        }

        let jump_5s = 5 * s.sample_rate as usize * s.channels;
        if ctx.input(|i| i.key_pressed(egui::Key::ArrowLeft)) { s.cursor = s.cursor.saturating_sub(jump_5s); }
        if ctx.input(|i| i.key_pressed(egui::Key::ArrowRight)) { s.cursor = (s.cursor + jump_5s).min(s.total_samples); }
        
        drop(s);

        egui::CentralPanel::default().show(ctx, |ui| {
            let full_width = ui.available_width();

            if *self.is_loading.lock().unwrap() {
                ui.centered_and_justified(|ui| { ui.label("Loading..."); });
            } else {
                ui.vertical_centered(|ui| {
                    ui.add_space(10.0);
                    if ui.button("Open File").clicked() {
                        if let Some(path) = FileDialog::new().pick_file() { self.load_audio_file(path); }
                    }
                    
                    let mut s = self.settings.lock().unwrap();
                    let sample_div = (s.sample_rate as f32 * s.channels as f32).max(1.0);
                    
                    ui.add_space(10.0);
                    ui.label(&self.file_path);
                    ui.label(format!("{:.2}s : {:.2}s", s.cursor as f32 / sample_div, s.total_samples as f32 / sample_div));

                    let (rect, response) = ui.allocate_at_least(egui::vec2(full_width, 100.0), egui::Sense::click_and_drag());
                    let start_x = rect.left() + (s.loop_start as f32 / s.total_samples.max(1) as f32) * rect.width();
                    let end_x = rect.left() + (s.loop_end as f32 / s.total_samples.max(1) as f32) * rect.width();
                    
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
                                s.is_seeking = true; 
                                let val = (((pointer.x - rect.left()) / rect.width()).clamp(0.0, 1.0) * s.total_samples as f32) as usize;
                                s.cursor = val - (val % s.channels.max(1));
                            }
                        }
                    }

                    if response.dragged() {
                        if let Some(pointer) = response.interact_pointer_pos() {
                            let val = (((pointer.x - rect.left()) / rect.width()).clamp(0.0, 1.0) * s.total_samples as f32) as usize;
                            if self.dragging_marker == Some(true) { 
                                s.loop_start = val; 
                            } else if self.dragging_marker == Some(false) { 
                                s.loop_end = val; 
                            } else { 
                                s.is_seeking = true; 
                                s.cursor = val - (val % s.channels.max(1)); 
                            }
                        }
                    }
                    
                    if response.drag_stopped() || response.clicked() { 
                        self.dragging_marker = None; 
                        s.is_seeking = false; 
                    }

                    ui.painter().rect_filled(rect, 2.0, egui::Color32::BLACK);
                    
                    if s.loop_start > 0 || s.loop_end < s.total_samples {
                        let loop_rect = egui::Rect::from_x_y_ranges(start_x..=end_x, rect.top()..=rect.bottom());
                        ui.painter().rect_filled(loop_rect, 0.0, egui::Color32::from_rgba_unmultiplied(0, 255, 0, 40));
                    }

                    if !s.waveform.is_empty() {
                        for (i, &peak) in s.waveform.iter().enumerate() {
                            let x = rect.left() + (i as f32 / s.waveform.len() as f32) * rect.width();
                            ui.painter().line_segment(
                                [egui::pos2(x, rect.center().y - (peak * rect.height() * 0.4)), egui::pos2(x, rect.center().y + (peak * rect.height() * 0.4))], 
                                (1.0, egui::Color32::from_rgb(0, 220, 0))
                            );
                        }
                    }

                    let start_color = if self.dragging_marker == Some(true) { egui::Color32::from_rgb(255, 255, 180) } else { egui::Color32::YELLOW };
                    ui.painter().line_segment([egui::pos2(start_x, rect.top()), egui::pos2(start_x, rect.bottom())], (2.5, start_color));
                    let end_color = if self.dragging_marker == Some(false) { egui::Color32::from_rgb(0, 255, 255) } else { egui::Color32::from_rgb(0, 40, 150) };
                    ui.painter().line_segment([egui::pos2(end_x, rect.top()), egui::pos2(end_x, rect.bottom())], (2.5, end_color));
                    let ph_x = rect.left() + (s.cursor as f32 / s.total_samples.max(1) as f32) * rect.width();
                    ui.painter().line_segment([egui::pos2(ph_x, rect.top()), egui::pos2(ph_x, rect.bottom())], (1.5, egui::Color32::WHITE));

                    ui.add_space(15.0);
                    
                    ui.spacing_mut().slider_width = ui.available_width() - 60.0; 
                    
                    ui.label("Speed");
                    ui.add(egui::Slider::new(&mut s.speed, 0.25..=4.0).logarithmic(true).suffix("x").max_decimals(1));
                    ui.label("Pitch");
                    ui.add(egui::Slider::new(&mut s.pitch, 0.5..=2.0).logarithmic(true).suffix("x").max_decimals(2));
                    ui.label("Volume");
                    ui.add(egui::Slider::new(&mut s.volume, 0.0..=2.0).max_decimals(1));
                    
                    ui.add_space(10.0);

                    ui.horizontal(|ui| {
                        let icon = if s.is_playing { "Pause" } else { "Play" };
                        if ui.button(icon).clicked() { s.is_playing = !s.is_playing; }
                        
                        if ui.button("Reset").clicked() { s.speed = 1.0; s.pitch = 1.0; s.volume = 1.0; }
                        
                        ui.separator();
                        if ui.button("[ Set Start").clicked() { s.loop_start = s.cursor; }
                        if ui.button("] Set End").clicked() { s.loop_end = s.cursor; }
                        if ui.button("Clear Loop").clicked() { s.loop_start = 0; s.loop_end = s.total_samples; }
                        ui.separator();
                        
                        ui.label(format!("Loop: {:.2}s - {:.2}s", s.loop_start as f32 / sample_div, s.loop_end as f32 / sample_div));
                    });
                });
            }
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
