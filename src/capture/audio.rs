use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::{self, JoinHandle};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, SampleFormat, StreamConfig, SupportedBufferSize, SupportedStreamConfig};
use parking_lot::Mutex;

/// Live UAC monitor: capture from the ShadowCast microphone endpoint and play
/// it on the system default output.
pub struct AudioMonitor {
    stop: Arc<AtomicBool>,
    volume: Arc<AtomicU32>,
    muted: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl AudioMonitor {
    pub fn start(matchers: &[String], audio_buffer: u32) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let volume = Arc::new(AtomicU32::new(1.0f32.to_bits()));
        let muted = Arc::new(AtomicBool::new(false));
        let matchers = matchers.to_vec();
        let audio_buffer = audio_buffer.clamp(64, 1024);
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();

        let stop_thread = Arc::clone(&stop);
        let volume_thread = Arc::clone(&volume);
        let muted_thread = Arc::clone(&muted);
        let thread = thread::Builder::new()
            .name("shadowcast-audio".into())
            .spawn(move || {
                run_monitor(
                    &matchers,
                    audio_buffer,
                    stop_thread,
                    volume_thread,
                    muted_thread,
                    ready_tx,
                );
            })
            .map_err(|err| format!("Failed to start audio thread: {err}"))?;

        match ready_rx.recv_timeout(std::time::Duration::from_secs(4)) {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                let _ = thread.join();
                return Err(err);
            }
            Err(_) => {
                log::warn!("Audio monitor is still starting");
            }
        }

        Ok(Self {
            stop,
            volume,
            muted,
            thread: Some(thread),
        })
    }

    pub fn set_volume(&self, volume: f32) {
        self.volume
            .store(volume.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }
}

impl Drop for AudioMonitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run_monitor(
    matchers: &[String],
    audio_buffer: u32,
    stop: Arc<AtomicBool>,
    volume: Arc<AtomicU32>,
    muted: Arc<AtomicBool>,
    ready_tx: std::sync::mpsc::Sender<Result<(), String>>,
) {
    match run_monitor_inner(matchers, audio_buffer, stop, volume, muted, &ready_tx) {
        Ok(()) => {}
        Err(err) => {
            let _ = ready_tx.send(Err(err));
        }
    }
}

fn run_monitor_inner(
    matchers: &[String],
    audio_buffer: u32,
    stop: Arc<AtomicBool>,
    volume: Arc<AtomicU32>,
    muted: Arc<AtomicBool>,
    ready_tx: &std::sync::mpsc::Sender<Result<(), String>>,
) -> Result<(), String> {
    let host = cpal::default_host();
    let input_device = find_input_device(&host, matchers)?;
    let input_supported = pick_input_config(&input_device)?;
    let input_format = input_supported.sample_format();
    let input_config = low_latency_config(&input_supported, audio_buffer);

    log::info!(
        "Audio input: {} @ {} Hz, {} ch, {input_format:?}, buffer {:?}",
        input_device,
        input_config.sample_rate,
        input_config.channels,
        input_config.buffer_size
    );

    let output_device = host
        .default_output_device()
        .ok_or_else(|| "No default audio output device".to_string())?;
    let output_supported = pick_output_config(
        &output_device,
        input_config.sample_rate,
        input_config.channels,
    )?;
    let output_format = output_supported.sample_format();
    let output_config = low_latency_config(&output_supported, audio_buffer);

    log::info!(
        "Audio output: {} @ {} Hz, {} ch, {output_format:?}, buffer {:?}",
        output_device,
        output_config.sample_rate,
        output_config.channels,
        output_config.buffer_size
    );

    let max_queued = ((input_config.sample_rate as usize)
        .saturating_mul(input_config.channels as usize)
        .saturating_mul(20)
        / 1000)
        .max(256);
    let buffer = Arc::new(Mutex::new(VecDeque::<f32>::with_capacity(max_queued)));
    let err_fn = |err| log::error!("Audio stream error: {err}");

    let input_buf = Arc::clone(&buffer);
    let volume_in = Arc::clone(&volume);
    let muted_in = Arc::clone(&muted);
    let cap = max_queued;
    let input_stream = match input_format {
        SampleFormat::F32 => input_device.build_input_stream(
            input_config,
            move |data: &[f32], _| {
                push_samples(&input_buf, data.iter().copied(), &volume_in, &muted_in, cap);
            },
            err_fn,
            None,
        ),
        SampleFormat::I16 => input_device.build_input_stream(
            input_config,
            move |data: &[i16], _| {
                push_samples(
                    &input_buf,
                    data.iter().map(|s| *s as f32 / 32768.0),
                    &volume_in,
                    &muted_in,
                    cap,
                );
            },
            err_fn,
            None,
        ),
        SampleFormat::I8 => input_device.build_input_stream(
            input_config,
            move |data: &[i8], _| {
                push_samples(
                    &input_buf,
                    data.iter().map(|s| *s as f32 / 128.0),
                    &volume_in,
                    &muted_in,
                    cap,
                );
            },
            err_fn,
            None,
        ),
        other => return Err(format!("Unsupported capture sample format: {other:?}")),
    }
    .map_err(|err| format!("Failed to build audio input stream: {err}"))?;

    let output_buf = Arc::clone(&buffer);
    let output_stream = match output_format {
        SampleFormat::F32 => output_device.build_output_stream(
            output_config,
            move |data: &mut [f32], _| {
                let mut buf = output_buf.lock();
                for sample in data.iter_mut() {
                    *sample = buf.pop_front().unwrap_or(0.0);
                }
            },
            err_fn,
            None,
        ),
        SampleFormat::I16 => output_device.build_output_stream(
            output_config,
            move |data: &mut [i16], _| {
                let mut buf = output_buf.lock();
                for sample in data.iter_mut() {
                    *sample = (buf.pop_front().unwrap_or(0.0).clamp(-1.0, 1.0) * 32767.0) as i16;
                }
            },
            err_fn,
            None,
        ),
        SampleFormat::I8 => output_device.build_output_stream(
            output_config,
            move |data: &mut [i8], _| {
                let mut buf = output_buf.lock();
                for sample in data.iter_mut() {
                    *sample = (buf.pop_front().unwrap_or(0.0).clamp(-1.0, 1.0) * 127.0) as i8;
                }
            },
            err_fn,
            None,
        ),
        other => return Err(format!("Unsupported output sample format: {other:?}")),
    }
    .map_err(|err| format!("Failed to build audio output stream: {err}"))?;

    input_stream
        .play()
        .map_err(|err| format!("Failed to start audio input: {err}"))?;
    output_stream
        .play()
        .map_err(|err| format!("Failed to start audio output: {err}"))?;

    let _ = ready_tx.send(Ok(()));

    while !stop.load(Ordering::Relaxed) {
        thread::sleep(std::time::Duration::from_millis(50));
    }

    drop(input_stream);
    drop(output_stream);
    Ok(())
}

fn push_samples<I>(
    buffer: &Mutex<VecDeque<f32>>,
    samples: I,
    volume: &AtomicU32,
    muted: &AtomicBool,
    max_queued: usize,
) where
    I: IntoIterator<Item = f32>,
{
    let gain = if muted.load(Ordering::Relaxed) {
        0.0
    } else {
        f32::from_bits(volume.load(Ordering::Relaxed))
    };
    let mut buf = buffer.lock();
    for sample in samples {
        if buf.len() >= max_queued {
            buf.pop_front();
        }
        buf.push_back(sample * gain);
    }
}

fn low_latency_config(supported: &SupportedStreamConfig, preferred: u32) -> StreamConfig {
    let mut config = supported.config();
    if let SupportedBufferSize::Range { min, max } = *supported.buffer_size() {
        config.buffer_size = BufferSize::Fixed(preferred.clamp(min, max));
    }
    config
}

fn find_input_device(host: &cpal::Host, matchers: &[String]) -> Result<cpal::Device, String> {
    let patterns: Vec<String> = matchers.iter().map(|s| s.to_lowercase()).collect();
    let devices: Vec<cpal::Device> = host
        .input_devices()
        .map_err(|err| format!("Failed to enumerate audio inputs: {err}"))?
        .collect();

    for device in &devices {
        log::debug!("Audio input candidate: {device}");
    }

    let mut matched: Vec<&cpal::Device> = devices
        .iter()
        .filter(|device| {
            let name = device.to_string().to_lowercase();
            patterns.iter().any(|pattern| name.contains(pattern))
        })
        .collect();

    matched.sort_by_key(|device| {
        let name = device.to_string().to_lowercase();
        if name.starts_with("front:") {
            0
        } else if name.starts_with("hw:") {
            1
        } else if name.contains("shadowcast") {
            2
        } else if name.starts_with("sysdefault:") {
            3
        } else {
            4
        }
    });

    matched
        .into_iter()
        .next()
        .cloned()
        .ok_or_else(|| format!("No audio input matching {matchers:?}"))
}

fn format_preference(fmt: SampleFormat) -> Option<u8> {
    match fmt {
        SampleFormat::F32 => Some(0),
        SampleFormat::I16 => Some(1),
        SampleFormat::I8 => Some(2),
        _ => None,
    }
}

fn pick_input_config(device: &cpal::Device) -> Result<SupportedStreamConfig, String> {
    let target = 48_000;
    if let Ok(ranges) = device.supported_input_configs() {
        let mut best: Option<(u8, cpal::SupportedStreamConfigRange)> = None;
        for range in ranges {
            if range.min_sample_rate() > target || range.max_sample_rate() < target {
                continue;
            }
            let Some(pref) = format_preference(range.sample_format()) else {
                continue;
            };
            if best.as_ref().is_none_or(|(p, _)| pref < *p) {
                best = Some((pref, range));
            }
        }
        if let Some((_, range)) = best {
            return Ok(range.with_sample_rate(target));
        }
    }
    device
        .default_input_config()
        .map_err(|err| format!("No compatible audio input config: {err}"))
}

fn pick_output_config(
    device: &cpal::Device,
    rate: u32,
    channels: u16,
) -> Result<SupportedStreamConfig, String> {
    if let Ok(ranges) = device.supported_output_configs() {
        let mut best: Option<(u8, cpal::SupportedStreamConfigRange)> = None;
        for range in ranges {
            if range.channels() != channels
                || range.min_sample_rate() > rate
                || range.max_sample_rate() < rate
            {
                continue;
            }
            let Some(pref) = format_preference(range.sample_format()) else {
                continue;
            };
            if best.as_ref().is_none_or(|(p, _)| pref < *p) {
                best = Some((pref, range));
            }
        }
        if let Some((_, range)) = best {
            return Ok(range.with_sample_rate(rate));
        }
    }
    device
        .default_output_config()
        .map_err(|err| format!("No compatible audio output config: {err}"))
}
