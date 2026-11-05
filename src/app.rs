use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, IndexPath, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    select::{Select, SelectEvent, SelectItem, SelectState},
    slider::{Slider, SliderEvent, SliderState},
    switch::Switch,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use image::Frame;
use parking_lot::Mutex;

use crate::capture::{
    CaptureDevice, CaptureFormat, CaptureSession, VideoFrame, enumerate_devices, list_formats,
    preferred_format,
};

const AUTO_FORMAT_ID: &str = "auto";

enum VideoEvent {
    Stats { fps: f32, size: Option<(u32, u32)> },
    Failed(String),
}

struct VideoStage {
    frames: Option<Arc<Mutex<Option<VideoFrame>>>>,
    error: Option<Arc<Mutex<Option<String>>>>,
    frame: Option<Arc<RenderImage>>,
    retired: Vec<Arc<RenderImage>>,
    live: bool,
    shown_frames: u64,
    fps: f32,
    last_fps_at: Instant,
    last_fps_count: u64,
    frame_size: Option<(u32, u32)>,
}

impl VideoStage {
    fn new() -> Self {
        Self {
            frames: None,
            error: None,
            frame: None,
            retired: Vec::new(),
            live: false,
            shown_frames: 0,
            fps: 0.0,
            last_fps_at: Instant::now(),
            last_fps_count: 0,
            frame_size: None,
        }
    }

    fn start(&mut self, session: &CaptureSession, cx: &mut Context<Self>) {
        self.frames = Some(session.frame_slot());
        self.error = Some(session.error_slot());
        self.live = true;
        self.shown_frames = 0;
        self.last_fps_count = 0;
        self.last_fps_at = Instant::now();
        self.fps = 0.0;
        cx.notify();
    }

    fn stop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.live = false;
        self.frames = None;
        self.error = None;
        if let Some(old) = self.frame.take() {
            self.retired.push(old);
        }
        self.frame_size = None;
        self.fps = 0.0;
        self.drop_retired(window, cx);
        cx.notify();
    }

    fn pull_frame(&mut self, cx: &mut Context<Self>) {
        if let Some(error) = self.error.as_ref().and_then(|slot| slot.lock().take()) {
            self.live = false;
            cx.emit(VideoEvent::Failed(error));
            return;
        }

        if let Some(frame) = self.frames.as_ref().and_then(|slot| slot.lock().take()) {
            self.frame_size = Some((frame.width, frame.height));
            if let Some(image) = frame_to_render(frame) {
                if let Some(old) = self.frame.replace(image) {
                    self.retired.push(old);
                }
                self.shown_frames += 1;
            }
        }

        let elapsed = self.last_fps_at.elapsed();
        if elapsed >= Duration::from_secs(1) {
            let delta = self.shown_frames.saturating_sub(self.last_fps_count);
            self.fps = delta as f32 / elapsed.as_secs_f32();
            self.last_fps_count = self.shown_frames;
            self.last_fps_at = Instant::now();
            log::info!(
                "Display {:.0} fps{}",
                self.fps,
                self.frame_size
                    .map(|(w, h)| format!(" ({w}×{h})"))
                    .unwrap_or_default()
            );
            cx.emit(VideoEvent::Stats {
                fps: self.fps,
                size: self.frame_size,
            });
        }
    }

    fn drop_retired(&mut self, window: &mut Window, cx: &mut App) {
        for image in self.retired.drain(..) {
            cx.drop_image(image, Some(window));
        }
    }
}

impl EventEmitter<VideoEvent> for VideoStage {}

impl Render for VideoStage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.drop_retired(window, cx);
        self.pull_frame(cx);
        if self.live {
            window.request_animation_frame();
        }

        let theme = cx.theme().clone();
        div()
            .id("stage")
            .size_full()
            .bg(gpui_kit::black())
            .overflow_hidden()
            .child(match &self.frame {
                Some(frame) => img(frame.clone())
                    .id("video-frame")
                    .size_full()
                    .object_fit(ObjectFit::Contain)
                    .into_any_element(),
                None => v_flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .text_color(theme.muted_foreground)
                    .child(Icon::new(IconName::Play).large())
                    .child(if self.live {
                        "Waiting for video…"
                    } else {
                        "Connect a Genki ShadowCast to start"
                    })
                    .child(
                        div()
                            .text_sm()
                            .child("The device appears as a standard UVC webcam. Close OBS if the device is busy."),
                    )
                    .into_any_element(),
            })
    }
}

#[derive(Clone)]
struct DeviceItem {
    id: SharedString,
    title: SharedString,
}

impl SelectItem for DeviceItem {
    type Value = SharedString;

    fn title(&self) -> SharedString {
        self.title.clone()
    }

    fn value(&self) -> &Self::Value {
        &self.id
    }
}

#[derive(Clone)]
struct FormatItem {
    id: SharedString,
    title: SharedString,
}

impl SelectItem for FormatItem {
    type Value = SharedString;

    fn title(&self) -> SharedString {
        self.title.clone()
    }

    fn value(&self) -> &Self::Value {
        &self.id
    }
}

pub struct Viewer {
    devices: Vec<CaptureDevice>,
    formats: Vec<CaptureFormat>,
    device_select: Entity<SelectState<Vec<DeviceItem>>>,
    format_select: Entity<SelectState<Vec<FormatItem>>>,
    volume_slider: Entity<SliderState>,
    session: Option<CaptureSession>,
    video_stage: Entity<VideoStage>,
    status: SharedString,
    muted: bool,
    volume: f32,
    audio_ok: bool,
    fps: f32,
    frame_size: Option<(u32, u32)>,
    _subscriptions: Vec<Subscription>,
}

impl Viewer {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let devices = enumerate_devices();
        let selected = default_device_index(&devices);
        let device_items = device_items(&devices);
        let device_select =
            cx.new(|cx| SelectState::new(device_items, selected.map(IndexPath::new), window, cx));

        let mut formats = Vec::new();
        if let Some(device) = selected.and_then(|index| devices.get(index)) {
            formats = list_formats(&device.index).unwrap_or_else(|err| {
                log::warn!("Could not list formats: {err}");
                Vec::new()
            });
        }
        let format_items = format_items(&formats);
        let preferred_row = preferred_format(&formats).and_then(|preferred| {
            formats
                .iter()
                .position(|format| format.id() == preferred.id())
                .map(|row| row + 1)
        });
        let format_select = cx.new(|cx| {
            SelectState::new(
                format_items,
                Some(IndexPath::new(preferred_row.unwrap_or(0))),
                window,
                cx,
            )
        });

        let volume_slider = cx.new(|_| {
            SliderState::new()
                .min(0.0)
                .max(1.0)
                .step(0.01)
                .default_value(1.0)
        });

        let video_stage = cx.new(|_| VideoStage::new());

        let mut viewer = Self {
            devices,
            formats,
            device_select,
            format_select,
            volume_slider,
            session: None,
            video_stage,
            status: "Idle".into(),
            muted: false,
            volume: 1.0,
            audio_ok: false,
            fps: 0.0,
            frame_size: None,
            _subscriptions: Vec::new(),
        };

        viewer._subscriptions.push(cx.subscribe_in(
            &viewer.device_select,
            window,
            |this, _, _: &SelectEvent<Vec<DeviceItem>>, window, cx| {
                this.on_device_changed(window, cx);
            },
        ));
        viewer._subscriptions.push(cx.subscribe_in(
            &viewer.video_stage,
            window,
            |this, _, event: &VideoEvent, window, cx| match event {
                VideoEvent::Stats { fps, size } => {
                    this.fps = *fps;
                    this.frame_size = *size;
                    if let Some(session) = &this.session {
                        if session.is_finished() {
                            this.status = "Capture stopped unexpectedly.".into();
                            this.disconnect(window, cx);
                            return;
                        }
                        if let Some(active) = session.active_format()
                            && this.status.to_string().contains('…')
                        {
                            this.status = format!("Live · {}", active.label()).into();
                        }
                    }
                    cx.notify();
                }
                VideoEvent::Failed(err) => {
                    this.status = err.clone().into();
                    this.disconnect(window, cx);
                }
            },
        ));
        viewer._subscriptions.push(cx.subscribe(
            &viewer.volume_slider,
            |this, _, event: &SliderEvent, cx| match event {
                SliderEvent::Change(value) | SliderEvent::Release(value) => {
                    this.volume = value.start();
                    if let Some(session) = &this.session {
                        session.set_volume(this.volume);
                    }
                    cx.notify();
                }
            },
        ));

        if selected.is_some() {
            viewer.connect(window, cx);
        } else if viewer.devices.is_empty() {
            viewer.status = "No capture devices found. Plug in a ShadowCast over USB 3.".into();
        } else {
            viewer.status = "Select a capture device and press Connect.".into();
        }

        viewer
    }

    fn selected_device(&self, cx: &App) -> Option<CaptureDevice> {
        let id = self.device_select.read(cx).selected_value()?.to_string();
        self.devices
            .iter()
            .find(|device| device.id() == id)
            .cloned()
    }

    fn selected_format(&self, cx: &App) -> Option<CaptureFormat> {
        let id = self.format_select.read(cx).selected_value()?.to_string();
        if id == AUTO_FORMAT_ID {
            return preferred_format(&self.formats);
        }
        self.formats
            .iter()
            .find(|format| format.id() == id)
            .cloned()
    }

    fn on_device_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(device) = self.selected_device(cx) else {
            return;
        };
        self.formats = list_formats(&device.index).unwrap_or_else(|err| {
            self.status = err.into();
            Vec::new()
        });
        let items = format_items(&self.formats);
        let preferred_row = preferred_format(&self.formats).and_then(|preferred| {
            self.formats
                .iter()
                .position(|format| format.id() == preferred.id())
                .map(|row| row + 1)
        });
        self.format_select.update(cx, |state, cx| {
            state.set_items(items, window, cx);
            state.set_selected_index(Some(IndexPath::new(preferred_row.unwrap_or(0))), window, cx);
        });
        cx.notify();
    }

    fn refresh_devices(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.devices = enumerate_devices();
        let selected = default_device_index(&self.devices);
        let items = device_items(&self.devices);
        self.device_select.update(cx, |state, cx| {
            state.set_items(items, window, cx);
            state.set_selected_index(selected.map(IndexPath::new), window, cx);
        });
        if self.session.is_none() {
            self.on_device_changed(window, cx);
            self.status = if self.devices.is_empty() {
                "No capture devices found.".into()
            } else {
                "Device list refreshed.".into()
            };
        }
        cx.notify();
    }

    fn toggle_connection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.session.is_some() {
            self.disconnect(window, cx);
            self.status = "Disconnected.".into();
        } else {
            self.connect(window, cx);
        }
    }

    fn connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(device) = self.selected_device(cx) else {
            self.status = "Select a capture device first.".into();
            cx.notify();
            return;
        };

        if self.formats.is_empty() {
            self.formats = list_formats(&device.index).unwrap_or_default();
            let items = format_items(&self.formats);
            self.format_select.update(cx, |state, cx| {
                state.set_items(items, window, cx);
            });
        }

        let format = self.selected_format(cx);
        self.status = format!("Opening {}…", device.name).into();
        cx.notify();

        match CaptureSession::start(device.clone(), format) {
            Ok(session) => {
                session.set_volume(self.volume);
                session.set_muted(self.muted);
                self.audio_ok = session.audio_available();
                self.fps = 0.0;
                self.frame_size = None;
                self.video_stage.update(cx, |stage, cx| {
                    stage.start(&session, cx);
                });
                self.status = if self.audio_ok {
                    format!("Live · {}", device.name).into()
                } else {
                    format!(
                        "Live · {} · video only (no matching audio input)",
                        device.name
                    )
                    .into()
                };
                self.session = Some(session);
            }
            Err(err) => {
                self.status = err.into();
            }
        }
        cx.notify();
    }

    fn disconnect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.video_stage.update(cx, |stage, cx| {
            stage.stop(window, cx);
        });
        self.session = None;
        self.audio_ok = false;
        self.frame_size = None;
        self.fps = 0.0;
        cx.notify();
    }
}

impl Render for Viewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _ = window;
        let connected = self.session.is_some();
        let theme = cx.theme().clone();

        v_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(self.toolbar(connected, cx))
            .child(div().flex_1().w_full().child(self.video_stage.clone()))
            .child(self.status_bar(connected, cx))
    }
}

impl Viewer {
    fn toolbar(&self, connected: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        h_flex()
            .w_full()
            .px_3()
            .py_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(Icon::new(IconName::Play).small())
                    .child("ShadowCast"),
            )
            .child(
                div().w(px(260.)).child(
                    Select::new(&self.device_select)
                        .placeholder("Capture device")
                        .disabled(connected)
                        .small(),
                ),
            )
            .child(
                Button::new("refresh")
                    .ghost()
                    .icon(IconName::RefreshCw)
                    .small()
                    .tooltip("Refresh devices")
                    .disabled(connected)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.refresh_devices(window, cx);
                    })),
            )
            .child(
                div().w(px(240.)).child(
                    Select::new(&self.format_select)
                        .placeholder("Format")
                        .disabled(connected)
                        .small(),
                ),
            )
            .child(
                Button::new("connect")
                    .when(connected, |this| this.danger().label("Disconnect"))
                    .when(!connected, |this| this.primary().label("Connect"))
                    .small()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.toggle_connection(window, cx);
                    })),
            )
            .child(div().flex_1())
            .child(
                Switch::new("mute")
                    .label(if self.muted { "Muted" } else { "Audio" })
                    .checked(!self.muted)
                    .on_change(cx.listener(|this, checked: &bool, _, cx| {
                        this.muted = !*checked;
                        if let Some(session) = &this.session {
                            session.set_muted(this.muted);
                        }
                        cx.notify();
                    })),
            )
            .child(
                div().w(px(140.)).child(
                    Slider::new(&self.volume_slider)
                        .disabled(self.muted)
                        .accessibility_label("Volume"),
                ),
            )
    }

    fn status_bar(&self, connected: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let size = self
            .frame_size
            .map(|(w, h)| format!("{w}×{h}"))
            .unwrap_or_else(|| "—".into());
        let fps = if connected {
            format!("{:.0} fps", self.fps)
        } else {
            "0 fps".into()
        };
        let audio = if !connected {
            "audio off"
        } else if self.muted {
            "muted"
        } else if self.audio_ok {
            "audio monitor"
        } else {
            "no audio"
        };

        h_flex()
            .w_full()
            .px_3()
            .py_1()
            .gap_3()
            .items_center()
            .text_sm()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .text_color(theme.muted_foreground)
            .child(self.status.clone())
            .child(div().flex_1())
            .child(size)
            .child(fps)
            .child(audio)
    }
}

fn device_items(devices: &[CaptureDevice]) -> Vec<DeviceItem> {
    if devices.is_empty() {
        return vec![DeviceItem {
            id: "none".into(),
            title: "No devices found".into(),
        }];
    }
    devices
        .iter()
        .map(|device| {
            let mut title = device.name.clone();
            if device.is_shadowcast {
                title.push_str(" (ShadowCast)");
            }
            if let Some(path) = &device.video_path {
                title.push_str(" · ");
                title.push_str(path);
            }
            DeviceItem {
                id: device.id().into(),
                title: title.into(),
            }
        })
        .collect()
}

fn format_items(formats: &[CaptureFormat]) -> Vec<FormatItem> {
    let mut items = vec![FormatItem {
        id: AUTO_FORMAT_ID.into(),
        title: "Auto (best available)".into(),
    }];
    items.extend(formats.iter().map(|format| FormatItem {
        id: format.id().into(),
        title: format.label().into(),
    }));
    items
}

fn default_device_index(devices: &[CaptureDevice]) -> Option<usize> {
    devices
        .iter()
        .position(|device| device.is_shadowcast)
        .or_else(|| (!devices.is_empty()).then_some(0))
}

fn frame_to_render(frame: crate::capture::VideoFrame) -> Option<Arc<RenderImage>> {
    let buffer = frame.into_rgba_image()?;
    Some(Arc::new(RenderImage::new([Frame::new(buffer)])))
}
