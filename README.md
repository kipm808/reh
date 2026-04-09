[![Latest Release](https://img.shields.io/github/v/release/kipm808/reh)](https://github.com/kipm808/reh/releases/latest)

# Reh

reh (rehearse) is an mp3 player tailored for musicians to learn or transcribe recorded music.

## Pre-compiled Binaries

- Click on the release badge above or the Releases section in the right panel.
- chmod +x reh_linux

![reh screen shot](/assets/reh.png)

## Features

- rust project
- MIT License

## How to install on Ubuntu 24.04

```cpp
sudo apt install libasound2-dev -y
sudo apt install clang libclang-dev llvm-dev libxml2-dev -y
sudo apt install libaubio5 libaubio-dev aubio-tools -y
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
. "$HOME/.cargo/env"
cargo build --release
cargo run --release
```

- Rust Crates

| Crate | Function |
| :--- | :--- |
| **cpal** | Manages cross-platform audio device enumeration and low-latency output stream handling. |
| **symphonia** | Handles media probing and decoding of various audio formats into raw PCM data. |
| **signalsmith-stretch** | Provides the DSP engine for real-time time-stretching and pitch-shifting. |
| **egui / eframe** | The immediate-mode GUI framework used for the hardware-accelerated interface and waveform rendering. |
| **crossbeam-channel** | Facilitates high-performance, thread-safe message passing of parameter updates (speed/pitch) to the audio thread. |
| **rfd** | Provides native system file dialogs for selecting audio tracks. |
| **std::sync** | Utilized for `Arc`, `Mutex`, and `Atomic` primitives to safely share state between the UI and background threads. |
| **std::ptr** | Used for manual memory management of the PCM buffer to ensure zero-copy access on the audio thread. |


- Supported Containers;
.wav .ogg .webm .mkv .mp4 .m4a .aiff .caf 

- Supported Codecs:
 MP3 AAC-LC Vorbis Opus FLAC ALAC PCM ADPCM WavPack 

## How to use

```cpp
target/release/reh <audio file>
or
target/release/reh # select 'Open' for the file dialog
or
cp target/release/reh into a directory in your $PATH
(if necessary, restart the shell to update the path cache)
```

# Audio Player Controls

## Keyboard Hotkeys

### Playback
- **Space**: Toggle Play/Pause.
- **0 (Number Row/Keypad)**: Reset playhead to start and begin playback.
- **R**: Reset Speed and Pitch to 1.0.
- **Q / Escape**: Quit application.

### Navigation & Looping (Standard)
- **Left Arrow**:
    - *If Looping*: Move the entire loop window back to the previous beat marker.
    - *If Not Looping*: Jump the cursor back two beat markers.
- **Right Arrow**:
    - *If Looping*: Move the entire loop window forward to the next beat marker.
    - *If Not Looping*: Jump the cursor forward to the next beat marker.
- **Up Arrow**:
    - *If No Loop*: Create a 1-bar loop based on current beat markers.
    - *If Looping*: Extend the loop end to the next beat marker.
- **Down Arrow**:
    - *If Looping*: Shorten the loop by one beat marker or clear the loop if at minimum size.
- **C**: Clear current loop (Reset to full track length).

### Beat Grid Adjustment
- **Keypad 1**: Nudge all beat markers left (earlier).
- **Keypad 3**: Nudge all beat markers right (later).

### Loop Nudging (Command Modifier)
- **Cmd + Left Arrow**: Shift the current loop window backward by exactly one bar.
- **Cmd + Right Arrow**: Shift the current loop window forward by exactly one bar.

---

## Mouse Functions

### Waveform Interaction
- **Single Click**: Seek playhead to clicked position.
- **Click & Drag**:
    - *Standard*: Scrub through the waveform.
    - *On Loop Edge*: Resize the loop start or end point.
- **Cmd + Drag Loop Edge**: Move the entire loop window while maintaining its current width.
- **Double Click**: Automatically create a 1-bar loop at the clicked beat.
- **Triple Click**: Automatically create a 2-bar loop at the clicked beat.

### Audio Seek Behavior
- **Auto-Mute**: The audio is automatically silenced during any active seeking, scrubbing, or arrow-key navigation to prevent audio artifacts.

