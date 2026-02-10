# Reh

reh (rehearse) is an mp3 player tailored for musicians to learn or transcribe recorded music.

![reh screen shot](/assets/reh.png)

## Features

- Self contained rust project
- MIT License
    
## How to install on Ubuntu 24.04

```cpp
sudo apt install libasound2-dev -y
sudo apt install clang libclang-dev llvm-dev libxml2-dev -y
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
. "$HOME/.cargo/env"
cargo build --release
cargo run --release
```

- Rust Crates

| Crate | Function | 
| :--- | :----: |
| eframe | framework for Egui, immediate mode screen rendering
| symphonia | audio library akin to ffmpeg |
| signalsmith-stretch | high-quality, polyphonic pitch-shifting and time-stretching library |
| rfd | rust file dialog, for choosing the audio file path |
| cpal | cross platform audio layer, API for OS audio backends like ALSA through PipeWire (libsound2-dev) |
| ringbuf | thread-safe audio buffering |

- Supported Containers;
| Name | Suffix | 
| :--- | :----: |
| Wave | .wav |
| OGG | .ogg |
| WebM | .webm |
| MKV | .mkv |
| MP4 | .mp4 |
| ISO | .m4a |
| AIFF | .aiff |
| CAF | .caf |

- Supported Codecs:
| Codec | Uses | 
| :--- | :----: |
| MP3 | Standard music files (MP1, MP2, MP3) |
| AAC-LC | Tunes, YouTube, modern web audio |
| Vorbis | Spotify, game audio (inside OGG) |
| Opus | Discord, modern VoIP, high-efficiency web |
| FLAC | Audiophile music, archiving |
| ALAC | Apple Music, Lossless iTunes files |
| PCM | Raw audio found in WAV and AIFF |
| ADPCM | Legacy game audio, telephony |
| WavPack | High-end compression |

## How to use

```cpp
target/release/reh <audio file>
or
target/release/reh # select 'Open' for the file dialog
or
cp target/release/reh into a directory in your $PATH
(if necessary, restart the shell to update the path cache)
```

- Keyboard Shortcuts:
| Key | Function | 
| :--- | :----: |
| Space | play/pause |
| OpenBracket | loop start |
| CloseBracket | loop end |
| Num0 | rewind to 0 |
| Num1 | rewind 1 second |
| Num2 | rewind 2 seconds |
| Num3 | rewind 3 seconds |
| Num4 | rewind 4 seconds |
| Num5 | rewind 5 seconds |
| Num6 | rewind 6 seconds |
| Num7 | rewind 7 seconds |
| Num8 | rewind 8 seconds |
| Num9 | rewind 9 seconds |
| ArrowLeft | forward 5 seconds |
| ArrowRight | back 5 seconds |

