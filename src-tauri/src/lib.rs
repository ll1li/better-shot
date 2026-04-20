//! BetterShot - A screenshot capture and editing application
//!
//! This crate provides the Tauri backend for capturing, editing,
//! and saving screenshots with various features like region selection
//! and background customization.

mod clipboard;
mod commands;
mod image;
mod ocr;
mod screenshot;
mod utils;

use commands::{
    capture_all_monitors, capture_once, capture_region, copy_image_file_to_clipboard,
    get_desktop_directory, get_mouse_position, get_temp_directory, move_window_to_active_space,
    native_capture_fullscreen, native_capture_interactive, native_capture_window,
    native_capture_ocr_region, play_screenshot_sound, render_image_with_effects_rust, save_edited_image,
};

use tauri::{Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

/// Sets the app to be an "accessory" application on macOS.
/// This hides the Dock icon and removes the app from Cmd+Tab.
#[cfg(target_os = "macos")]
fn set_macos_accessory_mode() {
    use objc2::{MainThreadMarker};
    use objc2_app_kit::{NSApp, NSApplicationActivationPolicy};

    let mtm = MainThreadMarker::new().unwrap();
    let app = NSApp(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
}

/// Shows the main application window (creates it if needed, shows if hidden)
fn show_main_window(app: &tauri::AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(window) = app.get_webview_window("main") {
        // Window exists, just show and focus it
        let _ = window.show();
        let _ = window.set_focus();
    } else {
        // Create new window (shouldn't happen normally as we create on startup)
        let window = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
            .title(&format!("Better Shot{}", DISPLAY_SUFFIX))
            .inner_size(1200.0, 800.0)
            .min_inner_size(800.0, 600.0)
            .center()
            .resizable(true)
            .decorations(true)
            .build()?;

        // Handle close request - hide instead of quit
        let window_clone = window.clone();
        window.on_window_event(move |event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if let Err(e) = window_clone.hide() {
                    eprintln!("Failed to hide window: {}", e);
                }
                api.prevent_close();
            }
        });
    }
    Ok(())
}

/// Visual suffix that makes a debug build obvious next to an installed release
/// (same tray icon, same bundle ID, so users couldn't otherwise tell them apart).
#[cfg(debug_assertions)]
const DISPLAY_SUFFIX: &str = " (Dev)";
#[cfg(not(debug_assertions))]
const DISPLAY_SUFFIX: &str = "";

/// Route a `better-shot://capture?mode=<mode>` URL to the matching frontend event.
/// Kept in sync with the tray menu event names in `setup()`.
fn handle_deep_link(app: &tauri::AppHandle, url: &url::Url) {
    eprintln!("[deep-link] received: {}", url);
    if url.host_str() != Some("capture") {
        eprintln!("[deep-link] ignoring non-capture host: {:?}", url.host_str());
        return;
    }
    let mode = url
        .query_pairs()
        .find(|(k, _)| k == "mode")
        .map(|(_, v)| v.into_owned())
        .unwrap_or_else(|| "region".to_string());

    let event = match mode.as_str() {
        "region" => "capture-triggered",
        "screen" | "fullscreen" => "capture-fullscreen",
        "window" => "capture-window",
        "ocr" => "capture-ocr",
        other => {
            eprintln!("[deep-link] unknown capture mode '{}', ignoring", other);
            return;
        }
    };
    eprintln!("[deep-link] emitting event: {}", event);
    let _ = app.emit(event, ());
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let mut builder = tauri::Builder::default();

    // Single-instance guard: second launches (e.g. from `open better-shot://...`)
    // forward argv to the running instance instead of starting a new process.
    // The `deep-link` feature on tauri-plugin-single-instance makes it re-emit
    // the URL event to the already-running app via the deep-link plugin.
    #[cfg(desktop)]
    {
        use tauri::Manager;
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }));
    }

    builder
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_screenshots::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--hidden"]),
        ))
        .plugin(tauri_plugin_deep_link::init())
        .setup(|app| {
            use tauri::menu::{ MenuBuilder, MenuItemBuilder, PredefinedMenuItem};

            // Set accessory mode on macOS to hide Dock icon and Cmd+Tab entry
            #[cfg(target_os = "macos")]
            set_macos_accessory_mode();

            // Enable autostart by default (user can disable in settings)
            #[cfg(target_os = "macos")]
            {
                use tauri_plugin_autostart::ManagerExt;
                let autostart_manager = app.autolaunch();
                // Only enable if not already enabled (don't override user preference)
                if !autostart_manager.is_enabled().unwrap_or(false) {
                    let _ = autostart_manager.enable();
                }
            }

            // Create the main window but keep it hidden initially
            // This allows the React frontend to run and set up event listeners
            let window =
                WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                    .title(&format!("Better Shot{}", DISPLAY_SUFFIX))
                    .inner_size(1200.0, 800.0)
                    .min_inner_size(800.0, 600.0)
                    .center()
                    .resizable(true)
                    .decorations(true)
                    .visible(false) // Start hidden
                    .build()?;

            // Handle close request - hide instead of quit
            let window_clone = window.clone();
            window.on_window_event(move |event| {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    if let Err(e) = window_clone.hide() {
                        eprintln!("Failed to hide window: {}", e);
                    }
                    api.prevent_close();
                }
            });

            let overlay = WebviewWindowBuilder::new(
                app,
                "quick-overlay",
                WebviewUrl::App("index.html?overlay=1".into()),
            )
            .title("Better Shot – Quick Overlay")
            .inner_size(360.0, 240.0)
            .resizable(true)
            .decorations(true)
            .visible(false)
            .build()?;

            let overlay_clone = overlay.clone();
            overlay.on_window_event(move |event| {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    if let Err(e) = overlay_clone.hide() {
                        eprintln!("Failed to hide overlay window: {}", e);
                    }
                    api.prevent_close();
                }
            });

            #[cfg(target_os = "macos")]
            {
                use objc2::msg_send;
                use objc2_app_kit::NSWindow;

                overlay
                    .with_webview(|webview| {
                        let ns_window = webview.ns_window();
                        if ns_window.is_null() {
                            return;
                        }
                        let ns_window = unsafe { &*ns_window.cast::<NSWindow>() };

                        unsafe {
                            let collection_behavior: usize = 1 << 7;
                            let current: usize = msg_send![ns_window, collectionBehavior];
                            let new_behavior = current | collection_behavior;
                            let _: () = msg_send![ns_window, setCollectionBehavior: new_behavior];

                            let _: () = msg_send![ns_window, setHidesOnDeactivate: false];
                            let _: () = msg_send![ns_window, setCanHide: false];
                        }
                    })
                    .ok();
            }

            let open_item = MenuItemBuilder::with_id("open", "Open Better Shot").build(app)?;

            let capture_region_item =
                MenuItemBuilder::with_id("capture_region", "Capture Region").build(app)?;

            let capture_screen_item =
                MenuItemBuilder::with_id("capture_screen", "Capture Screen").build(app)?;

            let capture_window_item =
                MenuItemBuilder::with_id("capture_window", "Capture Window").build(app)?;

            let capture_ocr_item =
                MenuItemBuilder::with_id("capture_ocr", "OCR Region").build(app)?;

            let preferences_item =
                MenuItemBuilder::with_id("preferences", "Preferences...")
                    .accelerator("CommandOrControl+,")
                    .build(app)?;

            let quit_item = MenuItemBuilder::with_id("quit", "Quit")
                .accelerator("CommandOrControl+Q")
                .build(app)?;

            let menu = MenuBuilder::new(app)
                .items(&[
                    &open_item,
                    &PredefinedMenuItem::separator(app)?,
                    &capture_region_item,
                    &capture_screen_item,
                    &capture_window_item,
                    &capture_ocr_item,
                    &PredefinedMenuItem::separator(app)?,
                    &preferences_item,
                    &PredefinedMenuItem::separator(app)?,
                    &quit_item,
                ])
                .build()?;
            let _tray = tauri::tray::TrayIconBuilder::new()
                .menu(&menu)
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip(&format!("Better Shot{}", DISPLAY_SUFFIX))
                .on_menu_event(move |app, event| {
                    match event.id().as_ref() {
                        "open" => {
                            if let Err(e) = show_main_window(app) {
                                eprintln!("Failed to show window: {}", e);
                            }
                        }
                        "capture_region" => {
                            let _ = app.emit("capture-triggered", ());
                        }
                        "capture_screen" => {
                            let _ = app.emit("capture-fullscreen", ());
                        }
                        "capture_window" => {
                            let _ = app.emit("capture-window", ());
                        }
                        "capture_ocr" => {
                            let _ = app.emit("capture-ocr", ());
                        }
                        "preferences" => {
                            if let Err(e) = show_main_window(app) {
                                eprintln!("Failed to show window: {}", e);
                            } else {
                                let _ = app.emit("open-preferences", ());
                            }
                        }
                        "quit" => {
                            app.exit(0);
                        }
                        _ => {}
                    }
                })
                .build(app)?;

            // Register deep-link handler for better-shot:// URLs (Raycast integration).
            // Dispatches to the same events the tray menu uses.
            use tauri_plugin_deep_link::DeepLinkExt;
            let handle = app.handle().clone();
            app.deep_link().on_open_url(move |event| {
                for url in event.urls() {
                    handle_deep_link(&handle, &url);
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            capture_once,
            capture_all_monitors,
            capture_region,
            save_edited_image,
            render_image_with_effects_rust,
            get_desktop_directory,
            get_temp_directory,
            native_capture_interactive,
            native_capture_fullscreen,
            native_capture_window,
            native_capture_ocr_region,
            play_screenshot_sound,
            get_mouse_position,
            move_window_to_active_space,
            copy_image_file_to_clipboard
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
