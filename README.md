# USB Capture

A low-latency Linux viewer for Genki ShadowCast and other UVC/UAC HDMI capture devices. It talks to the dongle as a normal webcam and microphone: V4L2 for video, PipeWire/cpal for audio, GPUI for the window.

**Version:** 0.1.0
**Repo:** [github.com/crwgrant/usb-capture-app](https://github.com/crwgrant/usb-capture-app)

## Run

Close OBS or anything else that has the capture device open, then:

```bash
cargo run --release
```

A debug build will decode and present far below 60 fps. Use `--release`.

Plug the ShadowCast in over USB 3. Auto starts at 1080p60; the Format menu can pick any mode the device advertises.

## Roadmap

Check items off as they land. Settings that already exist in the app are marked done.

### Capture and latency

- [x] **Capture mode presets** — Low latency (uncompressed ~1080p60), Balanced (MJPEG 1080p60), High detail (1440p/4K MJPEG). Applies when Format is Auto.
- [x] **Pixel format preference** — Prefer NV12, YUY2, or MJPEG when Auto picks a mode. Uncompressed skips JPEG encode/decode and usually cuts latency.
- [x] **V4L2 buffer count** — 2 is lowest delay; 3–4 is a bit smoother if a frame hitch happens.
- [x] **Audio buffer size** — 64 / 128 / 256 / 512 frames. Smaller is snappier, more likely to underrun.
- [ ] **Vsync / tearing present** — Toggle is in Settings. GPUI still presents on the compositor clock, so unchecking it does not enable immediate/tearing output yet.
- [ ] **VA-API hardware MJPEG decode** — This GPU can decode JPEG (`VAProfileJPEGBaseline`). Use it to keep NV12 on the GPU instead of CPU-decoding to BGRA.
- [ ] **GPU texture present path** — Sample a persistent video texture instead of uploading a new 1080p `RenderImage` into the sprite atlas every frame.

### App

- [x] Settings menu
- [x] About page (name, version, GitHub repo)
