use std::time::Duration;

use mine_host_core::{guest, host, lan};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{mpsc, Mutex};

enum Session {
    Host(host::HostSession),
    Guest(guest::GuestSession),
}

struct AppState {
    session: Mutex<Option<Session>>,
}

/// Стабильный ключ хоста: NodeId не меняется между запусками,
/// старые инвайт-коды продолжают работать.
fn load_or_create_key(app: &AppHandle) -> anyhow::Result<iroh::SecretKey> {
    let dir = app.path().app_data_dir()?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("host.key");
    if let Ok(hex_str) = std::fs::read_to_string(&path) {
        let bytes: [u8; 32] = hex::decode(hex_str.trim())?
            .try_into()
            .map_err(|_| anyhow::anyhow!("bad key length"))?;
        return Ok(iroh::SecretKey::from_bytes(&bytes));
    }
    let key = iroh::SecretKey::generate();
    std::fs::write(&path, hex::encode(key.to_bytes()))?;
    Ok(key)
}

fn forward_events(app: AppHandle, mut rx: mpsc::Receiver<mine_host_core::events::Event>) {
    use tauri_plugin_notification::NotificationExt;
    tauri::async_runtime::spawn(async move {
        while let Some(ev) = rx.recv().await {
            if let mine_host_core::events::Event::GuestJoined { name, .. } = &ev {
                let _ = app
                    .notification()
                    .builder()
                    .title("MineHost")
                    .body(format!("{name} подключился к миру"))
                    .show();
            }
            let _ = app.emit("mh-event", &ev);
        }
    });
}

#[tauri::command]
async fn start_host(
    app: AppHandle,
    state: State<'_, AppState>,
    manual_port: Option<u16>,
) -> Result<String, String> {
    stop_inner(&state).await;
    let (port, world_name) = match manual_port {
        Some(p) => (p, "Сервер".to_string()),
        None => {
            let w = lan::discover_lan_world(Duration::from_secs(15))
                .await
                .map_err(|e| format!("{e:#}"))?;
            (w.port, w.motd)
        }
    };
    let key = load_or_create_key(&app).map_err(|e| format!("{e:#}"))?;
    let mut session = host::start(host::HostOptions {
        port,
        world_name,
        secret_key: Some(key),
        use_relays: true,
    })
    .await
    .map_err(|e| format!("{e:#}"))?;
    let code = session.invite_code.clone();
    if let Some(rx) = session.take_events() {
        forward_events(app, rx);
    }
    *state.session.lock().await = Some(Session::Host(session));
    Ok(code)
}

#[tauri::command]
async fn join(
    app: AppHandle,
    state: State<'_, AppState>,
    code: String,
    player_name: String,
) -> Result<u16, String> {
    stop_inner(&state).await;
    let mut session = guest::join(guest::GuestOptions {
        code,
        player_name,
        use_relays: true,
        preferred_port: None,
    })
    .await
    .map_err(|e| format!("{e:#}"))?;
    let port = session.local_port;
    if let Some(rx) = session.take_events() {
        forward_events(app, rx);
    }
    *state.session.lock().await = Some(Session::Guest(session));
    Ok(port)
}

#[tauri::command]
async fn stop(state: State<'_, AppState>) -> Result<(), String> {
    stop_inner(&state).await;
    Ok(())
}

async fn stop_inner(state: &State<'_, AppState>) {
    if let Some(session) = state.session.lock().await.take() {
        match session {
            Session::Host(s) => s.close().await,
            Session::Guest(s) => s.close().await,
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    use tauri::menu::{Menu, MenuItem};
    use tauri::tray::TrayIconBuilder;

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .manage(AppState { session: Mutex::new(None) })
        .invoke_handler(tauri::generate_handler![start_host, join, stop])
        .setup(|app| {
            let show = MenuItem::with_id(app, "show", "Показать", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Выход", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;
            TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
