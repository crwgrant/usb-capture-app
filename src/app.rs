use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, IndexPath, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    dialog::{DialogClose, DialogFooter},
    form::{Form, field},
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
    resolve_format,
};
use crate::settings::{
    APP_NAME, APP_REPO, APP_VERSION, AppSettings, CapturePreset, PixelFormatPref,
};

const AUTO_FORMAT_ID: &str = "auto";
const CHROME_HIDE_AFTER: Duration = Duration::from_millis(2500);

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
    vsync: bool,
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
            vsync: true,
            shown_frames: 0,
            fps: 0.0,
            last_fps_at: Instant::now(),
            last_fps_count: 0,
            frame_size: None,
        }
    }

    fn start(&mut self, session: &CaptureSession, vsync: bool, cx: &mut Context<Self>) {
        self.frames = Some(session.frame_slot());
        self.error = Some(session.error_slot());
        self.live = true;
        self.vsync = vsync;
        self.shown_frames = 0;
        self.last_fps_count = 0;
        self.last_fps_at = Instant::now();
        self.fps = 0.0;
        cx.notify();
    }

    fn set_vsync(&mut self, vsync: bool, cx: &mut Context<Self>) {
        self.vsync = vsync;
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
            // GPUI presents on the compositor clock either way; vsync is stored
            // for a future tearing present path.
            let _ = self.vsync;
            window.request_animation_frame();
        }

        let theme = cx.theme().clone();
        div()
            .id("stage")
            .size_full()
            .min_h(px(0.))
            .bg(gpui_kit::black())
            .overflow_hidden()
            .child(match &self.frame {
                Some(frame) => {
                    let frame = frame.clone();
                    canvas(
                        |bounds, _, _| bounds,
                        move |bounds, _, window, _| {
                            let fitted = ObjectFit::Contain.get_bounds(bounds, frame.size(0));
                            window
                                .paint_image(
                                    bounds,
                                    fitted,
                                    Corners::default(),
                                    frame.clone(),
                                    0,
                                    false,
                                )
                                .ok();
                        },
                    )
                    .size_full()
                    .into_any_element()
                }
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

#[derive(Clone)]
struct SettingItem {
    id: SharedString,
    title: SharedString,
}

impl SelectItem for SettingItem {
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
    resolution_select: Entity<SelectState<Vec<FormatItem>>>,
    fps_select: Entity<SelectState<Vec<FormatItem>>>,
    preset_select: Entity<SelectState<Vec<SettingItem>>>,
    pixel_select: Entity<SelectState<Vec<SettingItem>>>,
    buffer_select: Entity<SelectState<Vec<SettingItem>>>,
    audio_buffer_select: Entity<SelectState<Vec<SettingItem>>>,
    volume_slider: Entity<SliderState>,
    settings: AppSettings,
    session: Option<CaptureSession>,
    video_stage: Entity<VideoStage>,
    status: SharedString,
    muted: bool,
    volume: f32,
    audio_ok: bool,
    fps: f32,
    frame_size: Option<(u32, u32)>,
    fullscreen: bool,
    chrome_visible: bool,
    chrome_hovered: bool,
    window_hovered: bool,
    _hide_chrome_task: Option<Task<()>>,
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
        let settings = AppSettings::load();
        let resolution_items = resolution_items(&formats, &settings);
        let resolution_row = item_row(&resolution_items, &settings.resolution_id);
        let resolution_select = cx.new(|cx| {
            SelectState::new(
                resolution_items,
                Some(IndexPath::new(resolution_row)),
                window,
                cx,
            )
        });
        let fps_items = fps_items(&formats, &settings.resolution_id);
        let fps_row = item_row(&fps_items, &settings.fps_id);
        let fps_select =
            cx.new(|cx| SelectState::new(fps_items, Some(IndexPath::new(fps_row)), window, cx));

        let preset_select = cx.new(|cx| {
            SelectState::new(
                preset_items(),
                Some(IndexPath::new(
                    CapturePreset::all()
                        .iter()
                        .position(|preset| *preset == settings.capture_preset)
                        .unwrap_or(1),
                )),
                window,
                cx,
            )
        });
        let pixel_select = cx.new(|cx| {
            SelectState::new(
                pixel_items(),
                Some(IndexPath::new(
                    PixelFormatPref::all()
                        .iter()
                        .position(|format| *format == settings.pixel_format)
                        .unwrap_or(0),
                )),
                window,
                cx,
            )
        });
        let buffer_select = cx.new(|cx| {
            SelectState::new(
                buffer_items(),
                Some(IndexPath::new(
                    AppSettings::BUFFER_CHOICES
                        .iter()
                        .position(|n| *n == settings.capture_buffers)
                        .unwrap_or(0),
                )),
                window,
                cx,
            )
        });
        let audio_buffer_select = cx.new(|cx| {
            SelectState::new(
                audio_buffer_items(),
                Some(IndexPath::new(
                    AppSettings::AUDIO_CHOICES
                        .iter()
                        .position(|n| *n == settings.audio_buffer)
                        .unwrap_or(1),
                )),
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
            resolution_select,
            fps_select,
            preset_select,
            pixel_select,
            buffer_select,
            audio_buffer_select,
            volume_slider,
            settings,
            session: None,
            video_stage,
            status: "Idle".into(),
            muted: false,
            volume: 1.0,
            audio_ok: false,
            fps: 0.0,
            frame_size: None,
            fullscreen: window.is_fullscreen(),
            chrome_visible: true,
            chrome_hovered: false,
            window_hovered: window.is_window_hovered(),
            _hide_chrome_task: None,
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
        viewer._subscriptions.push(cx.subscribe_in(
            &viewer.resolution_select,
            window,
            |this, _, _: &SelectEvent<Vec<FormatItem>>, window, cx| {
                this.on_resolution_changed(window, cx);
            },
        ));
        viewer._subscriptions.push(cx.subscribe(
            &viewer.fps_select,
            |this, _, _: &SelectEvent<Vec<FormatItem>>, cx| {
                this.on_fps_changed(cx);
            },
        ));
        viewer._subscriptions.push(cx.subscribe_in(
            &viewer.preset_select,
            window,
            |this, _, _: &SelectEvent<Vec<SettingItem>>, window, cx| {
                this.on_capture_setting(window, cx);
            },
        ));
        viewer._subscriptions.push(cx.subscribe_in(
            &viewer.pixel_select,
            window,
            |this, _, _: &SelectEvent<Vec<SettingItem>>, window, cx| {
                this.on_capture_setting(window, cx);
            },
        ));
        viewer._subscriptions.push(cx.subscribe_in(
            &viewer.buffer_select,
            window,
            |this, _, _: &SelectEvent<Vec<SettingItem>>, window, cx| {
                this.on_capture_setting(window, cx);
            },
        ));
        viewer._subscriptions.push(cx.subscribe_in(
            &viewer.audio_buffer_select,
            window,
            |this, _, _: &SelectEvent<Vec<SettingItem>>, window, cx| {
                this.on_capture_setting(window, cx);
            },
        ));
        viewer
            ._subscriptions
            .push(cx.observe_window_bounds(window, |this, window, cx| {
                let fullscreen = window.is_fullscreen();
                if this.fullscreen != fullscreen {
                    this.fullscreen = fullscreen;
                    cx.notify();
                }
            }));
        if viewer.settings.auto_hide_chrome {
            viewer.schedule_hide_chrome(cx);
        }

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
        let mut settings = self.settings.clone();
        if let Some(id) = self.pixel_select.read(cx).selected_value()
            && let Some(format) = PixelFormatPref::from_id(id.as_ref())
        {
            settings.pixel_format = format;
        }
        let resolution_id = self
            .resolution_select
            .read(cx)
            .selected_value()
            .map(|id| id.to_string())
            .unwrap_or_else(|| settings.resolution_id.clone());
        let fps_id = self
            .fps_select
            .read(cx)
            .selected_value()
            .map(|id| id.to_string())
            .unwrap_or_else(|| settings.fps_id.clone());
        resolve_format(&self.formats, &resolution_id, &fps_id, &settings)
    }

    fn on_device_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(device) = self.selected_device(cx) else {
            return;
        };
        self.formats = list_formats(&device.index).unwrap_or_else(|err| {
            self.status = err.into();
            Vec::new()
        });
        self.refresh_mode_items(window, cx);
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
            self.refresh_mode_items(window, cx);
        }

        let format = self.selected_format(cx);
        self.status = format!("Opening {}…", device.name).into();
        cx.notify();

        match CaptureSession::start(device.clone(), format, &self.settings) {
            Ok(session) => {
                session.set_volume(self.volume);
                session.set_muted(self.muted);
                self.audio_ok = session.audio_available();
                self.fps = 0.0;
                self.frame_size = None;
                self.video_stage.update(cx, |stage, cx| {
                    stage.start(&session, self.settings.vsync, cx);
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

    fn set_auto_hide_chrome(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.settings.auto_hide_chrome == enabled {
            return;
        }
        self.settings.auto_hide_chrome = enabled;
        self.settings.save();
        self.chrome_visible = true;
        if enabled {
            self.schedule_hide_chrome(cx);
        } else {
            self.chrome_hovered = false;
            self._hide_chrome_task = None;
        }
        cx.notify();
    }

    fn reveal_chrome(&mut self, cx: &mut Context<Self>) {
        if !self.settings.auto_hide_chrome || !self.window_hovered {
            return;
        }
        if !self.chrome_visible {
            self.chrome_visible = true;
            cx.notify();
        }
        self.schedule_hide_chrome(cx);
    }

    fn sync_window_hover(&mut self, window: &Window, cx: &mut Context<Self>) {
        if !self.settings.auto_hide_chrome {
            return;
        }
        let hovered = window.is_window_hovered();
        if hovered == self.window_hovered {
            return;
        }
        self.window_hovered = hovered;
        if hovered {
            self.reveal_chrome(cx);
        } else {
            self.hide_chrome_now(cx);
        }
    }

    fn hide_chrome_now(&mut self, cx: &mut Context<Self>) {
        self.chrome_hovered = false;
        self._hide_chrome_task = None;
        if self.chrome_visible {
            self.chrome_visible = false;
            cx.notify();
        }
    }

    fn set_window_hovered(&mut self, hovered: bool, cx: &mut Context<Self>) {
        if hovered == self.window_hovered {
            if hovered {
                self.reveal_chrome(cx);
            }
            return;
        }
        self.window_hovered = hovered;
        if hovered {
            self.reveal_chrome(cx);
        } else {
            self.hide_chrome_now(cx);
        }
    }

    fn set_chrome_hovered(&mut self, hovered: bool, cx: &mut Context<Self>) {
        self.chrome_hovered = hovered;
        if hovered {
            self.reveal_chrome(cx);
        } else if self.window_hovered {
            self.schedule_hide_chrome(cx);
        } else {
            self.hide_chrome_now(cx);
        }
    }

    fn schedule_hide_chrome(&mut self, cx: &mut Context<Self>) {
        if !self.settings.auto_hide_chrome || self.chrome_hovered || !self.window_hovered {
            self._hide_chrome_task = None;
            return;
        }
        self._hide_chrome_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(CHROME_HIDE_AFTER).await;
            this.update(cx, |this, cx| {
                if this.settings.auto_hide_chrome
                    && this.window_hovered
                    && !this.chrome_hovered
                    && this.chrome_visible
                {
                    this.chrome_visible = false;
                    cx.notify();
                }
            })
            .ok();
        }));
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

    fn on_capture_setting(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut changed = false;
        if let Some(id) = self.preset_select.read(cx).selected_value()
            && let Some(preset) = CapturePreset::from_id(id.as_ref())
            && preset != self.settings.capture_preset
        {
            self.settings.capture_preset = preset;
            changed = true;
        }
        if let Some(id) = self.pixel_select.read(cx).selected_value()
            && let Some(format) = PixelFormatPref::from_id(id.as_ref())
            && format != self.settings.pixel_format
        {
            self.settings.pixel_format = format;
            changed = true;
        }
        if let Some(id) = self.buffer_select.read(cx).selected_value()
            && let Ok(buffers) = id.as_ref().parse::<u32>()
            && buffers != self.settings.capture_buffers
        {
            self.settings.capture_buffers = buffers;
            changed = true;
        }
        if let Some(id) = self.audio_buffer_select.read(cx).selected_value()
            && let Ok(frames) = id.as_ref().parse::<u32>()
            && frames != self.settings.audio_buffer
        {
            self.settings.audio_buffer = frames;
            changed = true;
        }
        if !changed {
            return;
        }
        self.settings.save();
        self.refresh_mode_items(window, cx);
        if self.session.is_some() {
            self.disconnect(window, cx);
            self.connect(window, cx);
        }
        cx.notify();
    }

    fn refresh_mode_items(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let resolution_items = resolution_items(&self.formats, &self.settings);
        let resolution_row = item_row(&resolution_items, &self.settings.resolution_id);
        self.resolution_select.update(cx, |state, cx| {
            state.set_items(resolution_items, window, cx);
            state.set_selected_index(Some(IndexPath::new(resolution_row)), window, cx);
        });
        self.refresh_fps_items(window, cx);
    }

    fn refresh_fps_items(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let items = fps_items(&self.formats, &self.settings.resolution_id);
        let row = item_row(&items, &self.settings.fps_id);
        if row == 0 && self.settings.fps_id != AUTO_FORMAT_ID {
            self.settings.fps_id = AUTO_FORMAT_ID.into();
            self.settings.save();
        }
        self.fps_select.update(cx, |state, cx| {
            state.set_items(items, window, cx);
            state.set_selected_index(Some(IndexPath::new(row)), window, cx);
        });
    }

    fn on_resolution_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.resolution_select.read(cx).selected_value() else {
            return;
        };
        if id.as_ref() == self.settings.resolution_id {
            return;
        }
        self.settings.resolution_id = id.to_string();
        self.settings.save();
        self.refresh_fps_items(window, cx);
    }

    fn on_fps_changed(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.fps_select.read(cx).selected_value() else {
            return;
        };
        if id.as_ref() == self.settings.fps_id {
            return;
        }
        self.settings.fps_id = id.to_string();
        self.settings.save();
    }

    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let viewer = cx.entity();
        let preset_select = self.preset_select.clone();
        let pixel_select = self.pixel_select.clone();
        let buffer_select = self.buffer_select.clone();
        let audio_buffer_select = self.audio_buffer_select.clone();
        window.open_dialog(cx, move |dialog, _, cx| {
            let settings = viewer.read(cx).settings.clone();
            dialog
                .title("Settings")
                .w(px(520.))
                .centered()
                .child(
                    Form::horizontal()
                        .label_width(px(160.))
                        .child(
                            field()
                                .label("Capture mode")
                                .description(
                                    "Applies when Resolution and FPS are Auto. Low latency prefers uncompressed 1080p60.",
                                )
                                .child(
                                    Select::new(&preset_select)
                                        .placeholder("Capture mode")
                                        .small(),
                                ),
                        )
                        .child(
                            field()
                                .label("Pixel format")
                                .description(
                                    "NV12/YUY2 skip JPEG encode and decode. MJPEG is for 1440p and 4K.",
                                )
                                .child(
                                    Select::new(&pixel_select)
                                        .placeholder("Pixel format")
                                        .small(),
                                ),
                        )
                        .child(
                            field()
                                .label("Capture buffers")
                                .description("2 is lowest delay. 3–4 can hide a dropped frame.")
                                .child(
                                    Select::new(&buffer_select)
                                        .placeholder("Buffers")
                                        .small(),
                                ),
                        )
                        .child(
                            field()
                                .label("Audio buffer")
                                .description("Frames per callback. Smaller is snappier.")
                                .child(
                                    Select::new(&audio_buffer_select)
                                        .placeholder("Audio buffer")
                                        .small(),
                                ),
                        )
                        .child(
                            field()
                                .label("Vsync")
                                .description(
                                    "Matches the display refresh. Immediate/tearing present is not available yet.",
                                )
                                .child(
                                    Switch::new("vsync")
                                        .label(if settings.vsync {
                                            "On"
                                        } else {
                                            "Off"
                                        })
                                        .checked(settings.vsync)
                                        .on_change({
                                            let viewer = viewer.clone();
                                            move |checked: &bool, window, cx| {
                                                viewer.update(cx, |this, cx| {
                                                    this.settings.vsync = *checked;
                                                    this.settings.save();
                                                    this.video_stage.update(cx, |stage, cx| {
                                                        stage.set_vsync(*checked, cx);
                                                    });
                                                    cx.notify();
                                                });
                                                let _ = window;
                                            }
                                        }),
                                ),
                        )
                        .child(
                            field()
                                .label("GPU decode")
                                .description(
                                    "VA-API JPEG to a GPU texture is not implemented yet.",
                                )
                                .child(
                                    Switch::new("hw-decode")
                                        .label("Coming soon")
                                        .checked(false)
                                        .disabled(true),
                                ),
                        )
                        .child(
                            field()
                                .label("Auto-hide controls")
                                .description(
                                    "Hide the toolbar and status bar after a few seconds idle. Move the mouse to show them again.",
                                )
                                .child(
                                    Switch::new("auto-hide-settings")
                                        .label(if settings.auto_hide_chrome {
                                            "On"
                                        } else {
                                            "Off"
                                        })
                                        .checked(settings.auto_hide_chrome)
                                        .on_change({
                                            let viewer = viewer.clone();
                                            move |checked: &bool, window, cx| {
                                                viewer.update(cx, |this, cx| {
                                                    this.set_auto_hide_chrome(*checked, cx);
                                                });
                                                let _ = window;
                                            }
                                        }),
                                ),
                        ),
                )
                .footer(
                    DialogFooter::new().child(DialogClose::new().trigger(|button| button.label("Close"))),
                )
        });
    }

    fn open_about(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.open_dialog(cx, |dialog, _, cx| {
            let muted = cx.theme().muted_foreground;
            dialog
                .title("About")
                .w(px(420.))
                .centered()
                .child(
                    v_flex()
                        .w_full()
                        .gap_3()
                        .items_center()
                        .py_2()
                        .child(Icon::new(IconName::Play).large())
                        .child(
                            div()
                                .text_lg()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(APP_NAME),
                        )
                        .child(div().text_sm().child(format!("Version {APP_VERSION}")))
                        .child(
                            div()
                                .text_sm()
                                .text_center()
                                .text_color(muted)
                                .child(
                                    "A low-latency viewer for Genki ShadowCast and other UVC capture devices.",
                                ),
                        )
                        .child(
                            Button::new("repo")
                                .ghost()
                                .icon(IconName::Github)
                                .label("github.com/crwgrant/usb-capture-app")
                                .on_click(|_, _, cx| {
                                    cx.open_url(APP_REPO);
                                }),
                        ),
                )
                .footer(
                    DialogFooter::new()
                        .child(DialogClose::new().trigger(|button| button.label("Close"))),
                )
        });
    }
}

impl Render for Viewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_window_hover(window, cx);
        if self.settings.auto_hide_chrome {
            window.request_animation_frame();
        }
        let connected = self.session.is_some();
        let theme = cx.theme().clone();
        let show_chrome = self.chrome_visible;

        v_flex()
            .id("viewer")
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .hover_listener_mode(HoverListenerMode::InputModalityIndependent)
            .on_hover(cx.listener(|this, hovered: &bool, window, cx| {
                if *hovered {
                    if window.is_window_hovered() {
                        this.set_window_hovered(true, cx);
                    }
                } else if !window.is_window_hovered() {
                    this.set_window_hovered(false, cx);
                }
            }))
            .on_mouse_move(cx.listener(|this, _: &MouseMoveEvent, _, cx| {
                this.set_window_hovered(true, cx);
            }))
            .when(show_chrome, |this| this.child(self.toolbar(connected, cx)))
            .child(
                div()
                    .id("video-pane")
                    .flex_1()
                    .w_full()
                    .min_h(px(0.))
                    .overflow_hidden()
                    .on_mouse_move(cx.listener(|this, _: &MouseMoveEvent, _, cx| {
                        this.set_window_hovered(true, cx);
                    }))
                    .child(self.video_stage.clone()),
            )
            .when(show_chrome, |this| {
                this.child(self.status_bar(connected, cx))
            })
    }
}

impl Viewer {
    fn toolbar(&self, connected: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        h_flex()
            .id("toolbar")
            .w_full()
            .flex_shrink_0()
            .px_3()
            .py_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                this.set_chrome_hovered(*hovered, cx);
            }))
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(Icon::new(IconName::Play).small())
                    .child(APP_NAME),
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
                div().w(px(150.)).child(
                    Select::new(&self.resolution_select)
                        .placeholder("Resolution")
                        .disabled(connected)
                        .small(),
                ),
            )
            .child(
                div().w(px(100.)).child(
                    Select::new(&self.fps_select)
                        .placeholder("FPS")
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
                Button::new("settings")
                    .ghost()
                    .icon(IconName::Settings)
                    .label("Settings")
                    .small()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_settings(window, cx);
                    })),
            )
            .child(
                Button::new("about")
                    .ghost()
                    .icon(IconName::Info)
                    .label("About")
                    .small()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_about(window, cx);
                    })),
            )
            .child(
                Button::new("fullscreen")
                    .ghost()
                    .icon(if self.fullscreen {
                        IconName::Minimize
                    } else {
                        IconName::Maximize
                    })
                    .label(if self.fullscreen {
                        "Exit Fullscreen"
                    } else {
                        "Fullscreen"
                    })
                    .small()
                    .tooltip(if self.fullscreen {
                        "Exit fullscreen"
                    } else {
                        "Enter fullscreen"
                    })
                    .on_click(cx.listener(|this, _, window, cx| {
                        window.toggle_fullscreen();
                        this.fullscreen = !this.fullscreen;
                        cx.notify();
                    })),
            )
            .child(
                Switch::new("auto-hide")
                    .label("Auto-hide")
                    .checked(self.settings.auto_hide_chrome)
                    .on_change(cx.listener(|this, checked: &bool, _, cx| {
                        this.set_auto_hide_chrome(*checked, cx);
                    })),
            )
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
            .id("status-bar")
            .w_full()
            .flex_shrink_0()
            .px_3()
            .py_1()
            .gap_3()
            .items_center()
            .text_sm()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                this.set_chrome_hovered(*hovered, cx);
            }))
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

const COMMON_SIZES: [(u32, u32); 4] = [(3840, 2160), (2560, 1440), (1920, 1080), (1280, 720)];
const COMMON_FPS: [u32; 4] = [120, 60, 50, 30];

fn is_common_mode(format: &CaptureFormat) -> bool {
    COMMON_SIZES.contains(&(format.width, format.height)) && COMMON_FPS.contains(&format.fps)
}

fn resolution_items(formats: &[CaptureFormat], settings: &AppSettings) -> Vec<FormatItem> {
    let mut items = vec![FormatItem {
        id: AUTO_FORMAT_ID.into(),
        title: format!("Auto ({})", settings.capture_preset.label().to_lowercase()).into(),
    }];
    let mut sizes = formats
        .iter()
        .filter(|format| is_common_mode(format))
        .map(|format| {
            (
                format.size_id(),
                format.size_label(),
                format.width,
                format.height,
            )
        })
        .collect::<Vec<_>>();
    sizes.sort_by(|a, b| b.2.cmp(&a.2).then(b.3.cmp(&a.3)));
    sizes.dedup_by(|a, b| a.0 == b.0);
    items.extend(sizes.into_iter().map(|(id, title, ..)| FormatItem {
        id: id.into(),
        title: title.into(),
    }));
    items
}

fn fps_items(formats: &[CaptureFormat], resolution_id: &str) -> Vec<FormatItem> {
    let mut items = vec![FormatItem {
        id: AUTO_FORMAT_ID.into(),
        title: "Auto".into(),
    }];
    let size = parse_size_choice(resolution_id);
    let mut rates = formats
        .iter()
        .filter(|format| is_common_mode(format))
        .filter(|format| {
            size.is_none_or(|(width, height)| format.width == width && format.height == height)
        })
        .map(|format| format.fps)
        .collect::<Vec<_>>();
    rates.sort_unstable_by(|a, b| b.cmp(a));
    rates.dedup();
    items.extend(rates.into_iter().map(|fps| FormatItem {
        id: fps.to_string().into(),
        title: format!("{fps} fps").into(),
    }));
    items
}

fn parse_size_choice(id: &str) -> Option<(u32, u32)> {
    if id.is_empty() || id == AUTO_FORMAT_ID {
        return None;
    }
    let (width, height) = id.split_once('x')?;
    Some((width.parse().ok()?, height.parse().ok()?))
}

fn item_row(items: &[FormatItem], id: &str) -> usize {
    items
        .iter()
        .position(|item| item.id.as_ref() == id)
        .unwrap_or(0)
}

fn preset_items() -> Vec<SettingItem> {
    CapturePreset::all()
        .into_iter()
        .map(|preset| SettingItem {
            id: preset.id().into(),
            title: preset.label().into(),
        })
        .collect()
}

fn pixel_items() -> Vec<SettingItem> {
    PixelFormatPref::all()
        .into_iter()
        .map(|format| SettingItem {
            id: format.id().into(),
            title: format.label().into(),
        })
        .collect()
}

fn buffer_items() -> Vec<SettingItem> {
    AppSettings::BUFFER_CHOICES
        .into_iter()
        .map(|n| SettingItem {
            id: n.to_string().into(),
            title: format!("{n} buffers").into(),
        })
        .collect()
}

fn audio_buffer_items() -> Vec<SettingItem> {
    AppSettings::AUDIO_CHOICES
        .into_iter()
        .map(|n| SettingItem {
            id: n.to_string().into(),
            title: format!("{n} frames").into(),
        })
        .collect()
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
