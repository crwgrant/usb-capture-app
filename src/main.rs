mod app;
mod capture;
mod menu;
mod settings;

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;

use crate::app::Viewer;

#[cfg(target_os = "macos")]
embed_plist::embed_info_plist!("../macos/Info.plist");

fn main() {
    #[cfg(target_os = "macos")]
    reexec_into_app_bundle();

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    application().with_assets(assets::Assets).run(|cx| {
        init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        #[cfg(target_os = "macos")]
        request_macos_camera_access();

        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(1280.), px(800.)), cx)),
            window_min_size: Some(size(px(720.), px(480.))),
            titlebar: Some(TitlebarOptions {
                title: Some("USB Capture".into()),
                ..Default::default()
            }),
            app_id: Some("com.crwgrant.usb-capture-app".into()),
            focus: true,
            show: true,
            inactive_frame_interval: None,
            low_power_frame_interval: None,
            ..Default::default()
        };

        let (_, viewer) = open_window(options, cx, |window, cx| {
            cx.new(|cx| Viewer::new(window, cx))
        })
        .expect("Failed to open window");
        crate::menu::init(cx, &viewer);
    });
}

#[cfg(target_os = "macos")]
fn request_macos_camera_access() {
    nokhwa::nokhwa_initialize(|granted| {
        if granted {
            log::info!("Camera access granted");
        } else {
            log::warn!(
                "Camera access denied. Enable it in System Settings → Privacy & Security → Camera."
            );
        }
    });
}

/// TCC only honors camera/mic usage strings from a real `.app` bundle, and
/// only if this process is responsible for itself. `cargo run` from Cursor
/// keeps Ursa as the responsible parent after `exec`, so spawn a new process
/// that disclaims that parentage before touching AVFoundation.
#[cfg(target_os = "macos")]
fn reexec_into_app_bundle() {
    use std::process::Command;

    const MARKER: &str = "SHADOWCAST_APP_BUNDLE";
    if std::env::var_os(MARKER).is_some() {
        return;
    }

    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    if is_inside_app_bundle(&exe) {
        return;
    }
    let Some(exe_name) = exe.file_name() else {
        return;
    };
    let Some(parent) = exe.parent() else {
        return;
    };

    let app = parent.join(format!("{}.app", exe_name.to_string_lossy()));
    let contents = app.join("Contents");
    let macos_dir = contents.join("MacOS");
    let bundled = macos_dir.join(exe_name);

    if let Err(err) = std::fs::create_dir_all(&macos_dir) {
        eprintln!("Could not create app bundle at {}: {err}", app.display());
        return;
    }
    if let Err(err) = std::fs::write(
        contents.join("Info.plist"),
        include_bytes!("../macos/Info.plist"),
    ) {
        eprintln!("Could not write Info.plist: {err}");
        return;
    }
    let _ = std::fs::write(contents.join("PkgInfo"), b"APPL????");
    if let Err(err) = std::fs::copy(&exe, &bundled) {
        eprintln!("Could not copy into app bundle: {err}");
        return;
    }

    let identifier = "com.crwgrant.usb-capture-app";
    let _ = Command::new("codesign")
        .args(["--force", "--sign", "-", "--identifier", identifier])
        .arg(&app)
        .status();

    let mut argv = vec![bundled.clone().into_os_string()];
    argv.extend(std::env::args_os().skip(1));
    match spawn_disclaimed(&bundled, &argv) {
        Ok(status) => std::process::exit(status),
        Err(err) => eprintln!("Could not launch bundled app: {err}"),
    }
}

#[cfg(target_os = "macos")]
fn spawn_disclaimed(
    path: &std::path::Path,
    argv: &[std::ffi::OsString],
) -> Result<i32, String> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    fn c_os(bytes: &[u8]) -> Result<CString, String> {
        CString::new(bytes).map_err(|_| "argument contains interior NUL".into())
    }

    unsafe {
        let c_path = c_os(path.as_os_str().as_bytes())?;
        let c_argv_owned = argv
            .iter()
            .map(|arg| c_os(arg.as_os_str().as_bytes()))
            .collect::<Result<Vec<_>, _>>()?;
        let mut c_argv: Vec<*mut libc::c_char> = c_argv_owned
            .iter()
            .map(|s| s.as_ptr() as *mut libc::c_char)
            .collect();
        c_argv.push(std::ptr::null_mut());

        let mut env_owned = std::env::vars_os()
            .filter(|(key, _)| key != "SHADOWCAST_APP_BUNDLE")
            .map(|(key, value)| {
                let mut pair = key;
                pair.push("=");
                pair.push(&value);
                c_os(pair.as_os_str().as_bytes())
            })
            .collect::<Result<Vec<_>, _>>()?;
        env_owned.push(c_os(b"SHADOWCAST_APP_BUNDLE=1")?);
        let mut c_env: Vec<*mut libc::c_char> = env_owned
            .iter()
            .map(|s| s.as_ptr() as *mut libc::c_char)
            .collect();
        c_env.push(std::ptr::null_mut());

        let mut attr: libc::posix_spawnattr_t = std::mem::zeroed();
        if libc::posix_spawnattr_init(&mut attr) != 0 {
            return Err("posix_spawnattr_init failed".into());
        }
        if responsibility_spawnattrs_setdisclaim(&mut attr, 1) != 0 {
            libc::posix_spawnattr_destroy(&mut attr);
            return Err("could not disclaim parent app responsibility".into());
        }

        let mut pid: libc::pid_t = 0;
        let rc = libc::posix_spawn(
            &mut pid,
            c_path.as_ptr(),
            std::ptr::null(),
            &attr,
            c_argv.as_mut_ptr(),
            c_env.as_mut_ptr(),
        );
        libc::posix_spawnattr_destroy(&mut attr);
        if rc != 0 {
            return Err(format!(
                "posix_spawn failed: {}",
                std::io::Error::from_raw_os_error(rc)
            ));
        }

        let mut status = 0;
        if libc::waitpid(pid, &mut status, 0) < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        if libc::WIFEXITED(status) {
            Ok(libc::WEXITSTATUS(status))
        } else if libc::WIFSIGNALED(status) {
            Ok(128 + libc::WTERMSIG(status))
        } else {
            Ok(1)
        }
    }
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn responsibility_spawnattrs_setdisclaim(
        attr: *mut libc::posix_spawnattr_t,
        disclaim: libc::c_int,
    ) -> libc::c_int;
}

#[cfg(target_os = "macos")]
fn is_inside_app_bundle(exe: &std::path::Path) -> bool {
    let macos = exe.parent();
    let contents = macos.and_then(|p| p.parent());
    let app = contents.and_then(|p| p.parent());
    matches!(macos, Some(path) if path.file_name().is_some_and(|name| name == "MacOS"))
        && matches!(contents, Some(path) if path.file_name().is_some_and(|name| name == "Contents"))
        && matches!(app, Some(path) if path.extension().is_some_and(|ext| ext == "app"))
}
