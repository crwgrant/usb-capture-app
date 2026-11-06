use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use image::RgbaImage;
use nokhwa::Camera;
use nokhwa::pixel_format::{FormatDecoder, RgbAFormat};
use nokhwa::utils::{
    ApiBackend, CameraFormat, CameraIndex, FrameFormat, RequestedFormat, RequestedFormatType,
    Resolution,
};
use parking_lot::Mutex;

use super::audio::AudioMonitor;
use super::device::CaptureDevice;
use crate::settings::{AppSettings, CapturePreset, PixelFormatPref};

/// One decoded BGRA frame ready for GPUI's `RenderImage`.
#[derive(Clone)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

impl VideoFrame {
    pub fn into_rgba_image(self) -> Option<RgbaImage> {
        RgbaImage::from_raw(self.width, self.height, self.bgra)
    }
}

/// A negotiated capture mode advertised by the UVC device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureFormat {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub fourcc: FrameFormat,
}

impl CaptureFormat {
    pub fn label(&self) -> String {
        format!(
            "{}×{} @ {} fps ({})",
            self.width,
            self.height,
            self.fps,
            fourcc_name(self.fourcc)
        )
    }

    pub fn size_id(&self) -> String {
        format!("{}x{}", self.width, self.height)
    }

    pub fn size_label(&self) -> String {
        format!("{}×{}", self.width, self.height)
    }

    fn from_camera(format: CameraFormat) -> Self {
        let resolution = format.resolution();
        Self {
            width: resolution.width(),
            height: resolution.height(),
            fps: format.frame_rate(),
            fourcc: format.format(),
        }
    }

    fn to_camera(&self) -> CameraFormat {
        CameraFormat::new(
            Resolution::new(self.width, self.height),
            self.fourcc,
            self.fps,
        )
    }
}

/// Running video (and optional audio) capture session.
pub struct CaptureSession {
    stop: Arc<AtomicBool>,
    frames: Arc<Mutex<Option<VideoFrame>>>,
    error: Arc<Mutex<Option<String>>>,
    active_format: Arc<Mutex<Option<CaptureFormat>>>,
    video_thread: Option<JoinHandle<()>>,
    audio: Option<AudioMonitor>,
}

impl CaptureSession {
    pub fn start(
        device: CaptureDevice,
        requested: Option<CaptureFormat>,
        settings: &AppSettings,
    ) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let frames = Arc::new(Mutex::new(None));
        let error = Arc::new(Mutex::new(None));
        let active_format = Arc::new(Mutex::new(None));

        let stop_thread = Arc::clone(&stop);
        let frames_thread = Arc::clone(&frames);
        let error_thread = Arc::clone(&error);
        let format_thread = Arc::clone(&active_format);
        let index = device.index.clone();

        let video_path = device.video_path.clone();
        let buffers = settings.capture_buffers.clamp(2, 4);
        let video_thread = thread::Builder::new()
            .name("shadowcast-video".into())
            .spawn(move || {
                if let Err(err) = run_capture(
                    index,
                    video_path,
                    requested,
                    buffers,
                    stop_thread,
                    frames_thread,
                    format_thread,
                ) {
                    log::error!("Capture stopped: {err}");
                    *error_thread.lock() = Some(err);
                }
            })
            .map_err(|err| format!("Failed to start capture thread: {err}"))?;

        let audio = match AudioMonitor::start(&device.audio_matches, settings.audio_buffer) {
            Ok(monitor) => Some(monitor),
            Err(err) => {
                log::warn!("Audio monitor unavailable: {err}");
                None
            }
        };

        Ok(Self {
            stop,
            frames,
            error,
            active_format,
            video_thread: Some(video_thread),
            audio,
        })
    }

    pub fn frame_slot(&self) -> Arc<Mutex<Option<VideoFrame>>> {
        Arc::clone(&self.frames)
    }

    pub fn error_slot(&self) -> Arc<Mutex<Option<String>>> {
        Arc::clone(&self.error)
    }

    pub fn active_format(&self) -> Option<CaptureFormat> {
        self.active_format.lock().clone()
    }

    pub fn audio_available(&self) -> bool {
        self.audio.is_some()
    }

    pub fn set_volume(&self, volume: f32) {
        if let Some(audio) = &self.audio {
            audio.set_volume(volume);
        }
    }

    pub fn set_muted(&self, muted: bool) {
        if let Some(audio) = &self.audio {
            audio.set_muted(muted);
        }
    }

    pub fn is_finished(&self) -> bool {
        self.video_thread
            .as_ref()
            .map(|thread| thread.is_finished())
            .unwrap_or(true)
    }
}

impl Drop for CaptureSession {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.video_thread.take() {
            let _ = thread.join();
        }
    }
}

pub fn list_formats(index: &CameraIndex) -> Result<Vec<CaptureFormat>, String> {
    let mut camera = open_camera(index, None)?;
    let mut formats = camera
        .compatible_camera_formats()
        .map_err(|err| format!("Failed to query formats: {err}"))?
        .into_iter()
        .map(CaptureFormat::from_camera)
        .collect::<Vec<_>>();
    formats.sort_by(|a, b| compare_formats(a, b, &AppSettings::default()));
    formats.dedup();
    Ok(formats)
}

pub fn preferred_format(
    formats: &[CaptureFormat],
    settings: &AppSettings,
) -> Option<CaptureFormat> {
    formats
        .iter()
        .min_by(|a, b| compare_formats(a, b, settings))
        .cloned()
}

/// Pick a device mode from resolution, fps, and pixel-format preferences.
pub fn resolve_format(
    formats: &[CaptureFormat],
    resolution_id: &str,
    fps_id: &str,
    settings: &AppSettings,
) -> Option<CaptureFormat> {
    let size = parse_size_id(resolution_id);
    let fps = parse_fps_id(fps_id);
    let candidates = formats
        .iter()
        .filter(|format| {
            size.is_none_or(|(width, height)| format.width == width && format.height == height)
        })
        .filter(|format| fps.is_none_or(|fps| format.fps == fps))
        .cloned()
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return preferred_format(formats, settings);
    }
    preferred_format(&candidates, settings)
}

fn parse_size_id(id: &str) -> Option<(u32, u32)> {
    if id.is_empty() || id == "auto" {
        return None;
    }
    let (width, height) = id.split_once('x')?;
    Some((width.parse().ok()?, height.parse().ok()?))
}

fn parse_fps_id(id: &str) -> Option<u32> {
    if id.is_empty() || id == "auto" {
        return None;
    }
    id.parse().ok()
}

fn compare_formats(
    a: &CaptureFormat,
    b: &CaptureFormat,
    settings: &AppSettings,
) -> std::cmp::Ordering {
    fourcc_rank(a.fourcc, settings)
        .cmp(&fourcc_rank(b.fourcc, settings))
        .then_with(|| over_target(a, settings).cmp(&over_target(b, settings)))
        .then_with(|| fps_distance(a, settings).cmp(&fps_distance(b, settings)))
        .then_with(|| pixel_distance(a, settings).cmp(&pixel_distance(b, settings)))
}

fn over_target(format: &CaptureFormat, settings: &AppSettings) -> u8 {
    u8::from(format.width * format.height > settings.capture_preset.target_pixels())
}

fn fps_distance(format: &CaptureFormat, settings: &AppSettings) -> u32 {
    format.fps.abs_diff(settings.capture_preset.target_fps())
}

fn pixel_distance(format: &CaptureFormat, settings: &AppSettings) -> u32 {
    (format.width * format.height).abs_diff(settings.capture_preset.target_pixels())
}

fn fourcc_rank(format: FrameFormat, settings: &AppSettings) -> u8 {
    let preferred = match settings.pixel_format {
        PixelFormatPref::Auto => match settings.capture_preset {
            CapturePreset::LowLatency => match format {
                FrameFormat::NV12 => 0,
                FrameFormat::YUYV => 1,
                FrameFormat::MJPEG => 2,
                FrameFormat::RAWRGB | FrameFormat::RAWBGR => 3,
                FrameFormat::GRAY => 4,
            },
            CapturePreset::Balanced | CapturePreset::HighDetail => match format {
                FrameFormat::MJPEG => 0,
                FrameFormat::NV12 => 1,
                FrameFormat::YUYV => 2,
                FrameFormat::RAWRGB | FrameFormat::RAWBGR => 3,
                FrameFormat::GRAY => 4,
            },
        },
        PixelFormatPref::Nv12 => match format {
            FrameFormat::NV12 => 0,
            FrameFormat::YUYV => 1,
            FrameFormat::MJPEG => 2,
            _ => 5,
        },
        PixelFormatPref::Yuy2 => match format {
            FrameFormat::YUYV => 0,
            FrameFormat::NV12 => 1,
            FrameFormat::MJPEG => 2,
            _ => 5,
        },
        PixelFormatPref::Mjpeg => match format {
            FrameFormat::MJPEG => 0,
            FrameFormat::NV12 => 1,
            FrameFormat::YUYV => 2,
            _ => 5,
        },
    };
    preferred
}

fn fourcc_name(format: FrameFormat) -> &'static str {
    match format {
        FrameFormat::MJPEG => "MJPEG",
        FrameFormat::YUYV => "YUY2",
        FrameFormat::NV12 => "NV12",
        FrameFormat::GRAY => "GRAY",
        FrameFormat::RAWRGB => "RGB",
        FrameFormat::RAWBGR => "BGR",
    }
}

fn open_camera(index: &CameraIndex, requested: Option<&CaptureFormat>) -> Result<Camera, String> {
    let request = match requested {
        Some(format) => {
            RequestedFormat::new::<RgbAFormat>(RequestedFormatType::Closest(format.to_camera()))
        }
        None => RequestedFormat::new::<RgbAFormat>(RequestedFormatType::AbsoluteHighestFrameRate),
    };
    Camera::with_backend(index.clone(), request, ApiBackend::Auto).map_err(|err| {
        let hint = if err.to_string().to_lowercase().contains("busy")
            || err.to_string().to_lowercase().contains("resource")
        {
            " Close OBS or any other app using the capture device."
        } else {
            ""
        };
        format!("Failed to open camera: {err}.{hint}")
    })
}

#[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
fn run_capture(
    index: CameraIndex,
    video_path: Option<String>,
    requested: Option<CaptureFormat>,
    buffers: u32,
    stop: Arc<AtomicBool>,
    frames: Arc<Mutex<Option<VideoFrame>>>,
    active_format: Arc<Mutex<Option<CaptureFormat>>>,
) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    if let Some(path) = video_path.as_deref() {
        match run_v4l_capture(
            path,
            requested.as_ref(),
            buffers,
            &stop,
            &frames,
            &active_format,
        ) {
            Ok(()) => return Ok(()),
            Err(err) => log::warn!("Low-latency V4L2 path failed ({err}); falling back to nokhwa"),
        }
    }

    run_nokhwa_capture(index, requested, stop, frames, active_format)
}

fn run_nokhwa_capture(
    index: CameraIndex,
    requested: Option<CaptureFormat>,
    stop: Arc<AtomicBool>,
    frames: Arc<Mutex<Option<VideoFrame>>>,
    active_format: Arc<Mutex<Option<CaptureFormat>>>,
) -> Result<(), String> {
    let mut camera = open_camera(&index, requested.as_ref())?;
    if let Some(format) = requested.as_ref() {
        let request =
            RequestedFormat::new::<RgbAFormat>(RequestedFormatType::Closest(format.to_camera()));
        if let Err(err) = camera.set_camera_requset(request) {
            log::warn!("Could not apply requested format {}: {err}", format.label());
        }
    }

    camera
        .open_stream()
        .map_err(|err| format!("Failed to start video stream: {err}"))?;

    let negotiated = CaptureFormat::from_camera(camera.camera_format());
    log::info!("Capture started: {}", negotiated.label());
    *active_format.lock() = Some(negotiated);

    while !stop.load(Ordering::Relaxed) {
        match camera.frame() {
            Ok(buffer) => match decode_bgra(buffer.source_frame_format(), &buffer) {
                Ok(frame) => *frames.lock() = Some(frame),
                Err(err) => log::debug!("Dropped undecodable frame: {err}"),
            },
            Err(err) => {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                log::warn!("Frame grab failed: {err}");
                thread::sleep(Duration::from_millis(8));
            }
        }
    }

    let _ = camera.stop_stream();
    Ok(())
}

#[cfg(target_os = "linux")]
fn run_v4l_capture(
    path: &str,
    requested: Option<&CaptureFormat>,
    buffers: u32,
    stop: &Arc<AtomicBool>,
    frames: &Arc<Mutex<Option<VideoFrame>>>,
    active_format: &Arc<Mutex<Option<CaptureFormat>>>,
) -> Result<(), String> {
    use v4l::Format;
    use v4l::buffer::Type;
    use v4l::io::traits::{CaptureStream, Stream as V4lStream};
    use v4l::prelude::*;
    use v4l::video::Capture as V4lCapture;

    let dev = Device::with_path(path).map_err(|err| format!("Failed to open {path}: {err}"))?;
    if let Some(format) = requested {
        let fourcc = fourcc_to_v4l(format.fourcc);
        let applied =
            V4lCapture::set_format(&dev, &Format::new(format.width, format.height, fourcc))
                .map_err(|err| format!("Failed to set V4L2 format: {err}"))?;
        if let Err(err) =
            V4lCapture::set_params(&dev, &v4l::video::capture::Parameters::with_fps(format.fps))
        {
            log::warn!("Could not set V4L2 frame rate {}: {err}", format.fps);
        }
        log::debug!(
            "V4L2 format applied: {}x{} {:?}",
            applied.width,
            applied.height,
            applied.fourcc
        );
    }

    let negotiated = v4l_negotiated_format(&dev)?;
    let mut stream = MmapStream::with_buffers(&dev, Type::VideoCapture, buffers)
        .map_err(|err| format!("Failed to map V4L2 buffers: {err}"))?;
    stream.set_timeout(Duration::from_millis(250));

    log::info!(
        "Capture started (V4L2, {buffers} buffers): {}",
        negotiated.label()
    );
    *active_format.lock() = Some(negotiated.clone());

    let mut scratch = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        match CaptureStream::next(&mut stream) {
            Ok((buf, meta)) => {
                copy_v4l_frame(&mut scratch, buf, meta.bytesused as usize);
                match decode_raw(
                    negotiated.fourcc,
                    negotiated.width,
                    negotiated.height,
                    &scratch,
                ) {
                    Ok(frame) => *frames.lock() = Some(frame),
                    Err(err) => log::debug!("Dropped undecodable frame: {err}"),
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::TimedOut => {
                // `next()` queues a buffer before polling. A timeout leaves that
                // buffer queued, so the next `next()` would VIDIOC_QBUF twice.
                let _ = V4lStream::stop(&mut stream);
            }
            Err(err) => {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                return Err(format!("V4L2 frame grab failed: {err}"));
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn copy_v4l_frame(scratch: &mut Vec<u8>, buf: &[u8], bytesused: usize) {
    let len = bytesused.min(buf.len());
    scratch.clear();
    scratch.extend_from_slice(&buf[..len]);
}

#[cfg(target_os = "linux")]
fn v4l_negotiated_format(dev: &v4l::Device) -> Result<CaptureFormat, String> {
    use v4l::video::Capture as V4lCapture;

    let format =
        V4lCapture::format(dev).map_err(|err| format!("Failed to read V4L2 format: {err}"))?;
    let params =
        V4lCapture::params(dev).map_err(|err| format!("Failed to read V4L2 params: {err}"))?;
    let fps = if params.interval.numerator == 0 {
        0
    } else {
        params.interval.denominator / params.interval.numerator
    };
    Ok(CaptureFormat {
        width: format.width,
        height: format.height,
        fps,
        fourcc: fourcc_from_v4l(format.fourcc),
    })
}

#[cfg(target_os = "linux")]
fn fourcc_to_v4l(format: FrameFormat) -> v4l::FourCC {
    use v4l::FourCC;
    match format {
        FrameFormat::MJPEG => FourCC::new(b"MJPG"),
        FrameFormat::YUYV => FourCC::new(b"YUYV"),
        FrameFormat::NV12 => FourCC::new(b"NV12"),
        FrameFormat::GRAY => FourCC::new(b"GREY"),
        FrameFormat::RAWRGB => FourCC::new(b"RGB3"),
        FrameFormat::RAWBGR => FourCC::new(b"BGR3"),
    }
}

#[cfg(target_os = "linux")]
fn fourcc_from_v4l(fourcc: v4l::FourCC) -> FrameFormat {
    match &fourcc.repr {
        b"MJPG" | b"JPEG" => FrameFormat::MJPEG,
        b"YUYV" | b"YUY2" => FrameFormat::YUYV,
        b"NV12" => FrameFormat::NV12,
        b"GREY" => FrameFormat::GRAY,
        b"RGB3" => FrameFormat::RAWRGB,
        b"BGR3" => FrameFormat::RAWBGR,
        _ => FrameFormat::MJPEG,
    }
}

fn decode_bgra(format: FrameFormat, buffer: &nokhwa::Buffer) -> Result<VideoFrame, String> {
    decode_raw(
        format,
        buffer.resolution().width(),
        buffer.resolution().height(),
        buffer.buffer(),
    )
}

fn decode_raw(
    format: FrameFormat,
    width: u32,
    height: u32,
    data: &[u8],
) -> Result<VideoFrame, String> {
    if format == FrameFormat::MJPEG {
        return decode_mjpeg_bgra(data);
    }

    let mut bgra = RgbAFormat::write_output(format, Resolution::new(width, height), data)
        .map_err(|err| err.to_string())?;
    for pixel in bgra.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Ok(VideoFrame {
        width,
        height,
        bgra,
    })
}

fn decode_mjpeg_bgra(data: &[u8]) -> Result<VideoFrame, String> {
    use zune_core::bytestream::ZCursor;
    use zune_core::colorspace::ColorSpace;
    use zune_core::options::DecoderOptions;
    use zune_jpeg::JpegDecoder;

    let options = DecoderOptions::new_fast().jpeg_set_out_colorspace(ColorSpace::BGRA);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(data), options);
    decoder
        .decode_headers()
        .map_err(|err| format!("JPEG headers: {err}"))?;
    let (width, height) = decoder
        .dimensions()
        .ok_or_else(|| "JPEG has no dimensions".to_string())?;
    let mut bgra = vec![
        0;
        decoder
            .output_buffer_size()
            .ok_or_else(|| "JPEG output size overflow".to_string())?
    ];
    decoder
        .decode_into(&mut bgra)
        .map_err(|err| format!("JPEG decode: {err}"))?;
    Ok(VideoFrame {
        width: width as u32,
        height: height as u32,
        bgra,
    })
}
