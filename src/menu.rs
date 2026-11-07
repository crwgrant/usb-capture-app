use gpui_kit::*;

use crate::app::Viewer;
use crate::settings::APP_NAME;

gpui_kit::actions!(
    app,
    [
        OpenAbout,
        OpenSettings,
        Quit,
        Hide,
        HideOthers,
        ShowAll,
        CloseWindow,
        MinimizeWindow,
        ZoomWindow,
        ToggleFullScreen,
        Cut,
        Copy,
        Paste,
        SelectAll
    ]
);

pub fn init(cx: &mut App, viewer: &Entity<Viewer>) {
    cx.bind_keys(key_bindings());
    register_actions(cx, viewer);
    cx.set_menus(app_menus());
    #[cfg(target_os = "macos")]
    cx.set_dock_menu(vec![
        MenuItem::action(&format!("Quit {APP_NAME}"), Quit),
    ]);
}

fn register_actions(cx: &mut App, viewer: &Entity<Viewer>) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &CloseWindow, cx| {
        with_window(cx, |window, _| window.remove_window());
    });
    cx.on_action(|_: &MinimizeWindow, cx| {
        with_window(cx, |window, _| window.minimize_window());
    });
    cx.on_action(|_: &ZoomWindow, cx| {
        with_window(cx, |window, _| window.zoom_window());
    });
    cx.on_action(|_: &ToggleFullScreen, cx| {
        with_window(cx, |window, _| window.toggle_fullscreen());
    });

    let settings_viewer = viewer.clone();
    cx.on_action(move |_: &OpenSettings, cx| {
        let settings_viewer = settings_viewer.clone();
        with_window(cx, move |window, cx| {
            settings_viewer.update(cx, |this, cx| this.open_settings(window, cx));
        });
    });
    let about_viewer = viewer.clone();
    cx.on_action(move |_: &OpenAbout, cx| {
        let about_viewer = about_viewer.clone();
        with_window(cx, move |window, cx| {
            about_viewer.update(cx, |this, cx| this.open_about(window, cx));
        });
    });
}

fn target_window(cx: &App) -> Option<AnyWindowHandle> {
    cx.active_window()
        .or_else(|| cx.windows().into_iter().next())
}

/// Menu actions are dispatched while the window is already on the update stack.
/// Nested `WindowHandle::update` fails, so run after the current cycle.
fn with_window(cx: &mut App, f: impl FnOnce(&mut Window, &mut App) + 'static) {
    let Some(handle) = target_window(cx) else {
        return;
    };
    cx.defer(move |cx| {
        handle.update(cx, |_, window, cx| f(window, cx)).ok();
    });
}

fn key_bindings() -> Vec<KeyBinding> {
    #[cfg(target_os = "macos")]
    {
        vec![
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-h", Hide, None),
            KeyBinding::new("cmd-alt-h", HideOthers, None),
            KeyBinding::new("cmd-w", CloseWindow, None),
            KeyBinding::new("cmd-m", MinimizeWindow, None),
            KeyBinding::new("cmd-ctrl-f", ToggleFullScreen, None),
            KeyBinding::new("cmd-,", OpenSettings, None),
            // Workspace matches the synthetic menu-bar context so ⌘X/C/V/A
            // appear as key equivalents without overriding Input's deeper
            // bindings when a text field is focused.
            KeyBinding::new("cmd-x", Cut, Some("Workspace")),
            KeyBinding::new("cmd-c", Copy, Some("Workspace")),
            KeyBinding::new("cmd-v", Paste, Some("Workspace")),
            KeyBinding::new("cmd-a", SelectAll, Some("Workspace")),
        ]
    }
    #[cfg(not(target_os = "macos"))]
    {
        vec![
            KeyBinding::new("ctrl-q", Quit, None),
            KeyBinding::new("ctrl-w", CloseWindow, None),
            KeyBinding::new("f11", ToggleFullScreen, None),
            KeyBinding::new("ctrl-,", OpenSettings, None),
            KeyBinding::new("ctrl-x", Cut, Some("Workspace")),
            KeyBinding::new("ctrl-c", Copy, Some("Workspace")),
            KeyBinding::new("ctrl-v", Paste, Some("Workspace")),
            KeyBinding::new("ctrl-a", SelectAll, Some("Workspace")),
        ]
    }
}

fn app_menus() -> Vec<Menu> {
    vec![
        Menu::new(APP_NAME).items(vec![
            MenuItem::action(&format!("About {APP_NAME}"), OpenAbout),
            MenuItem::separator(),
            MenuItem::action("Settings…", OpenSettings),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action(&format!("Hide {APP_NAME}"), Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action(&format!("Quit {APP_NAME}"), Quit),
        ]),
        Menu::new("File").items(vec![MenuItem::action("Close Window", CloseWindow)]),
        Menu::new("Edit").items(vec![
            MenuItem::os_action("Cut", Cut, OsAction::Cut),
            MenuItem::os_action("Copy", Copy, OsAction::Copy),
            MenuItem::os_action("Paste", Paste, OsAction::Paste),
            MenuItem::separator(),
            MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
        ]),
        Menu::new("Window").items(vec![
            MenuItem::action("Minimize", MinimizeWindow),
            MenuItem::action("Zoom", ZoomWindow),
            MenuItem::separator(),
            MenuItem::action("Enter Full Screen", ToggleFullScreen),
        ]),
        Menu::new("Help").items(vec![MenuItem::action(&format!("About {APP_NAME}"), OpenAbout)]),
    ]
}
