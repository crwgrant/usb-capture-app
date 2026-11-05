use std::path::{Path, PathBuf};

use nokhwa::query;
use nokhwa::utils::{ApiBackend, CameraIndex};

/// Known USB vendor IDs used by Genki / Human Things capture hardware.
const GENKI_VENDOR_IDS: &[&str] = &["298f", "345f", "32ed"];

/// Known vendor:product pairs for ShadowCast hardware revisions.
const SHADOWCAST_USB_IDS: &[(&str, &str)] = &[
    ("298f", "1996"), // ShadowCast 2
    ("345f", "2013"), // ShadowCast 2 (alternate ID)
    ("32ed", "3701"), // ShadowCast 3
];

/// A UVC capture device that can feed the viewer.
#[derive(Debug, Clone)]
pub struct CaptureDevice {
    pub name: String,
    pub index: CameraIndex,
    pub video_path: Option<String>,
    pub audio_matches: Vec<String>,
    pub is_shadowcast: bool,
}

impl CaptureDevice {
    pub fn id(&self) -> String {
        self.index.as_string()
    }
}

/// Enumerate UVC capture devices, preferring ShadowCast hardware when present.
pub fn enumerate_devices() -> Vec<CaptureDevice> {
    let mut devices = match query(ApiBackend::Auto) {
        Ok(infos) => infos
            .into_iter()
            .filter_map(device_from_nokhwa)
            .collect::<Vec<_>>(),
        Err(err) => {
            log::warn!("Failed to query cameras: {err}");
            Vec::new()
        }
    };

    dedupe_metadata_nodes(&mut devices);
    devices.sort_by(|a, b| {
        b.is_shadowcast
            .cmp(&a.is_shadowcast)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.id().cmp(&b.id()))
    });
    devices
}

fn device_from_nokhwa(info: nokhwa::utils::CameraInfo) -> Option<CaptureDevice> {
    let name = info.human_name();
    if name.to_lowercase().contains("metadata") {
        return None;
    }

    let index = info.index().clone();
    let video_path = linux_video_path(&index);
    let (usb_ids, alsa_card) = video_path
        .as_deref()
        .map(linux_usb_and_alsa)
        .unwrap_or((None, None));

    let is_shadowcast = is_shadowcast_device(&name, usb_ids.as_ref());
    let mut audio_matches = Vec::new();
    if let Some(card) = alsa_card {
        audio_matches.push(format!("CARD={card}"));
    }
    audio_matches.push(name.clone());
    if is_shadowcast {
        audio_matches.push("ShadowCast".into());
        audio_matches.push("GENKI".into());
        audio_matches.push("usb-GENKI".into());
    }

    Some(CaptureDevice {
        name,
        index,
        video_path,
        audio_matches,
        is_shadowcast,
    })
}

fn linux_video_path(index: &CameraIndex) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let n = index.as_index().ok()?;
        let path = format!("/dev/video{n}");
        Path::new(&path).exists().then_some(path)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = index;
        None
    }
}

fn linux_usb_and_alsa(video_path: &str) -> (Option<(String, String)>, Option<String>) {
    #[cfg(target_os = "linux")]
    {
        let Some(dev_name) = Path::new(video_path).file_name() else {
            return (None, None);
        };
        let sysfs = PathBuf::from("/sys/class/video4linux").join(dev_name);
        let usb = usb_ids_from_sysfs(&sysfs);
        let alsa = find_sibling_alsa_card(&sysfs);
        (usb, alsa)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = video_path;
        (None, None)
    }
}

fn is_shadowcast_device(name: &str, usb_ids: Option<&(String, String)>) -> bool {
    let lower = name.to_lowercase();
    if lower.contains("shadowcast") || lower.contains("genki") {
        return true;
    }
    if let Some((vendor, product)) = usb_ids {
        if SHADOWCAST_USB_IDS
            .iter()
            .any(|(v, p)| vendor.eq_ignore_ascii_case(v) && product.eq_ignore_ascii_case(p))
        {
            return true;
        }
        if GENKI_VENDOR_IDS
            .iter()
            .any(|v| vendor.eq_ignore_ascii_case(v))
            && lower.contains("capture")
        {
            return true;
        }
    }
    false
}

/// UVC devices often expose a capture node and a metadata node with the same name.
/// Keep the lowest-numbered node for each unique name.
fn dedupe_metadata_nodes(devices: &mut Vec<CaptureDevice>) {
    devices.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id().cmp(&b.id())));
    devices.dedup_by(|a, b| a.name == b.name);
}

#[cfg(target_os = "linux")]
fn usb_ids_from_sysfs(sysfs_path: &Path) -> Option<(String, String)> {
    let resolved = std::fs::canonicalize(sysfs_path.join("device")).ok()?;
    let mut current = resolved.as_path();
    for _ in 0..12 {
        let vendor_path = current.join("idVendor");
        let product_path = current.join("idProduct");
        if vendor_path.exists() && product_path.exists() {
            let vendor = std::fs::read_to_string(vendor_path)
                .ok()?
                .trim()
                .to_string();
            let product = std::fs::read_to_string(product_path)
                .ok()?
                .trim()
                .to_string();
            return Some((vendor, product));
        }
        current = current.parent()?;
    }
    None
}

#[cfg(target_os = "linux")]
fn find_sibling_alsa_card(video_sysfs_path: &Path) -> Option<String> {
    let resolved = std::fs::canonicalize(video_sysfs_path.join("device")).ok()?;
    let mut usb_device = resolved.as_path();
    for _ in 0..12 {
        if usb_device.join("idVendor").exists() {
            break;
        }
        usb_device = usb_device.parent()?;
    }
    if !usb_device.join("idVendor").exists() {
        return None;
    }
    find_card_id_recursive(usb_device)
}

#[cfg(target_os = "linux")]
fn find_card_id_recursive(path: &Path) -> Option<String> {
    let entries = std::fs::read_dir(path).ok()?;
    for entry in entries.flatten() {
        let entry_path = entry.path();
        let file_name = entry.file_name();
        if file_name == "sound"
            && let Ok(sound_entries) = std::fs::read_dir(&entry_path)
        {
            for sound_entry in sound_entries.flatten() {
                let card_name = sound_entry.file_name();
                if card_name.to_string_lossy().starts_with("card") {
                    let id_path = sound_entry.path().join("id");
                    if let Ok(id) = std::fs::read_to_string(id_path) {
                        let id = id.trim().to_string();
                        if !id.is_empty() {
                            return Some(id);
                        }
                    }
                }
            }
        }
        if entry_path.is_dir()
            && !entry_path.is_symlink()
            && let Some(id) = find_card_id_recursive(&entry_path)
        {
            return Some(id);
        }
    }
    None
}
