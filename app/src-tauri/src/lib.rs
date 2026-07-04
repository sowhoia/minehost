mod server;

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

#[tauri::command]
async fn kick(state: State<'_, AppState>, id: String) -> Result<bool, String> {
    match &*state.session.lock().await {
        Some(Session::Host(h)) => Ok(h.kick(&id).await),
        _ => Err("нет активной хост-сессии".into()),
    }
}

/// Новый код приглашения = новый ключ хоста (старые коды перестают работать).
/// Фронтенд после этого вызывает start_host заново.
#[tauri::command]
async fn rotate_code(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    stop_inner(&state).await;
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::remove_file(dir.join("host.key")).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn server_start(
    app: AppHandle,
    srv: State<'_, server::ServerState>,
    jar_path: String,
    ram_mb: u32,
    accept_eula: bool,
) -> Result<u16, String> {
    server::start(app, &srv, jar_path, ram_mb, accept_eula).await
}

#[tauri::command]
async fn server_stop(srv: State<'_, server::ServerState>) -> Result<(), String> {
    server::stop(&srv).await;
    Ok(())
}

#[tauri::command]
async fn diagnostics(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    match &*state.session.lock().await {
        Some(Session::Host(h)) => Ok(serde_json::to_value(h.diagnostics().await).unwrap()),
        Some(Session::Guest(g)) => Ok(serde_json::to_value(g.diagnostics().await).unwrap()),
        None => Ok(serde_json::Value::Null),
    }
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
        // single-instance должен регистрироваться первым: вторая копия,
        // запущенная кликом по minehost://-ссылке, пробрасывает argv сюда.
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.set_focus();
            }
            let _ = app.emit("mh-deeplink", argv);
        }))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(AppState { session: Mutex::new(None) })
        .manage(server::ServerState::default())
        .invoke_handler(tauri::generate_handler![
            start_host,
            join,
            stop,
            kick,
            rotate_code,
            diagnostics,
            server_start,
            server_stop
        ])
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
