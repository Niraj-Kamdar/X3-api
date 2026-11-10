//! Voice Studio desktop shell: a menu-bar (tray) app that runs the local
//! server in-process and offers two screens (Providers, Server).
//!
//! The UI talks to Rust only through the commands in [`commands`]. Provider
//! keys travel UI → command → Keychain once and are never sent back: the UI
//! only ever receives the last-4 hint.

use std::sync::Arc;

use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::TrayIconBuilder,
    Manager, WindowEvent,
};
use x3_server::{Paths, ServerHandle, VoiceService};
use x3_vault::SecretStore;

mod commands;

pub struct AppState {
    pub service: Arc<VoiceService>,
    pub store: Arc<dyn SecretStore>,
    pub server: tokio::sync::Mutex<Option<ServerHandle>>,
}

impl AppState {
    pub async fn start_server(&self) -> anyhow::Result<u16> {
        let mut guard = self.server.lock().await;
        if let Some(h) = guard.as_ref() {
            if h.is_running() {
                return Ok(h.addr.port());
            }
        }
        let port = self.service.settings().port;
        let handle = x3_server::start(self.service.clone(), port).await?;
        let port = handle.addr.port();
        *guard = Some(handle);
        Ok(port)
    }

    pub async fn stop_server(&self) {
        if let Some(h) = self.server.lock().await.take() {
            h.shutdown().await;
        }
    }
}

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.set_focus();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,x3_server=info,voice_studio_lib=info".into()),
        )
        .init();

    tauri::Builder::default()
        .setup(|app| {
            // Menu-bar app: no Dock icon (LSUIElement is also set in Info.plist).
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let store: Arc<dyn SecretStore> =
                Arc::from(x3_vault::default_store(x3_server::BUNDLE_ID));
            let paths = Paths::new(x3_server::default_data_dir());
            let service = tauri::async_runtime::block_on(x3_server::open_service(
                paths,
                x3_server::default_model_router(store.clone()),
            ))?;
            let state = AppState {
                service,
                store,
                server: tokio::sync::Mutex::new(None),
            };
            match tauri::async_runtime::block_on(state.start_server()) {
                Ok(port) => tracing::info!("server started on 127.0.0.1:{port}"),
                Err(e) => tracing::error!("server failed to start: {e:#}"),
            }
            app.manage(state);

            let open = MenuItem::with_id(app, "open", "Open Voice Studio…", true, None::<&str>)?;
            let toggle = MenuItem::with_id(app, "toggle", "Start/Stop Server", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit Voice Studio", true, None::<&str>)?;
            let sep = PredefinedMenuItem::separator(app)?;
            let menu = Menu::with_items(app, &[&open, &toggle, &sep, &quit])?;

            let mut tray = TrayIconBuilder::with_id("main")
                .tooltip("Voice Studio")
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => show_main_window(app),
                    "toggle" => {
                        let app = app.clone();
                        tauri::async_runtime::spawn(async move {
                            let state = app.state::<AppState>();
                            let running = state
                                .server
                                .lock()
                                .await
                                .as_ref()
                                .is_some_and(|h| h.is_running());
                            if running {
                                state.stop_server().await;
                            } else if let Err(e) = state.start_server().await {
                                tracing::error!("start failed: {e:#}");
                            }
                        });
                    }
                    "quit" => {
                        let app = app.clone();
                        tauri::async_runtime::spawn(async move {
                            app.state::<AppState>().stop_server().await;
                            app.exit(0);
                        });
                    }
                    _ => {}
                });
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.build(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window hides it; the app keeps running in the menu bar.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::providers_status,
            commands::provider_set_key,
            commands::provider_delete_key,
            commands::provider_test,
            commands::provider_set_budget,
            commands::server_status,
            commands::server_start,
            commands::server_stop,
            commands::server_set_port,
            commands::tokens_list,
            commands::token_mint,
            commands::token_revoke,
            commands::usage_recent,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Voice Studio");
}
