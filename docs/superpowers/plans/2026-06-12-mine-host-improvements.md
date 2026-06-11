# mine-host Improvements Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Восемь улучшений MVP (всё из одобренного списка, кроме синхронизации модпаков): ручной порт, deep-link приглашения, CI + автообновления, недавние миры, тюнинг QUIC, диагностика, кик/ротация кода, запуск server.jar.

**Architecture:** Ядро расширяется без ломки API: реестр соединений в HostSession (кик/блок/диагностика), общий Connection-хэндл в GuestSession (диагностика), тюнинг транспорта и диагностика — в net.rs (единственное место iroh-специфики). Приложение получает новые Tauri-команды + модуль server.rs (процесс java). UI остаётся одним +page.svelte.

**Tech Stack:** существующий (iroh 0.98, Tauri 2, SvelteKit) + плагины tauri-plugin-deep-link, single-instance, updater, dialog; GitHub Actions + tauri-action.

**База:** MVP в master (13 коммитов, 11 тестов). Ветка: `feat/improvements`.

---

### Task 1: Deep-link формат инвайтов (core)

**Files:**
- Modify: `core/src/invite.rs`
- Test: `core/tests/invite.rs`

- [ ] **Step 1: Падающий тест**

Добавить в `core/tests/invite.rs`:
```rust
#[tokio::test]
async fn invite_link_roundtrip() {
    let ep = mine_host_core::net::make_endpoint(None, false).await.unwrap();
    let addr = ep.addr();
    let link = invite::encode_link(&addr);
    assert!(link.starts_with("minehost://join/mh:"), "got {link}");
    assert_eq!(invite::decode(&link).unwrap().id, addr.id);
    ep.close().await;
}
```

- [ ] **Step 2: Убедиться, что падает**

Run: `cargo test -p mine-host-core --test invite`
Expected: ошибка компиляции — нет `encode_link`.

- [ ] **Step 3: Реализация**

В `core/src/invite.rs`:
```rust
const LINK_PREFIX: &str = "minehost://join/";

/// Кликабельная ссылка для мессенджеров (deep link).
pub fn encode_link(addr: &EndpointAddr) -> String {
    format!("{LINK_PREFIX}{}", encode(addr))
}
```
и в `decode` заменить строку выделения raw:
```rust
    let raw = code.trim();
    let raw = raw.strip_prefix(LINK_PREFIX).unwrap_or(raw);
    let raw = raw.strip_prefix(PREFIX).unwrap_or(raw);
```

- [ ] **Step 4: Тесты зелёные** — `cargo test -p mine-host-core --test invite` → 2 passed.
- [ ] **Step 5: Commit** — `git add core && git commit -m "feat(core): minehost:// deep-link invite format"`

---

### Task 2: Тюнинг QUIC под Minecraft (core)

Дефолт noq рассчитан на 100 Мбит/100 мс. Логин в модпак — всплеск в десятки МБ по одному потоку; поднимаем окна.

**Files:**
- Modify: `core/src/net.rs`

- [ ] **Step 1: Реализация**

В `make_endpoint` перед `bind()`:
```rust
use iroh::endpoint::{QuicTransportConfig, VarInt};

let transport = QuicTransportConfig::builder()
    // один TCP-поток Minecraft = один QUIC-поток: даём ему до 16 МБ в полёте
    .stream_receive_window(VarInt::from_u32(16 * 1024 * 1024))
    // суммарное окно соединения: логин-всплеск + несколько игроков за одним гостем
    .receive_window(VarInt::from_u32(64 * 1024 * 1024))
    .send_window(64 * 1024 * 1024)
    .build();
builder = builder.transport_config(transport);
```
Если какой-то из трёх методов не обёрнут в `QuicTransportConfigBuilder` текущей версии iroh — посмотреть `cargo doc -p iroh` (методы-обёртки chainable, принимают VarInt/u64); правится только здесь.

- [ ] **Step 2: Все тесты зелёные** — `cargo test -p mine-host-core` (e2e-туннель подтверждает, что конфиг не ломает hole punching).
- [ ] **Step 3: Commit** — `git add core/src/net.rs && git commit -m "perf(core): raise QUIC flow-control windows for modpack login bursts"`

---

### Task 3: Кик, блок-лист и реестр соединений (core)

**Files:**
- Modify: `core/src/host.rs`
- Test: `core/tests/kick.rs`

- [ ] **Step 1: Падающий тест**

`core/tests/kick.rs`:
```rust
use std::time::Duration;

use mine_host_core::events::Event;
use mine_host_core::{guest, host};
use tokio::net::TcpListener;

async fn next_event(rx: &mut tokio::sync::mpsc::Receiver<Event>, want: &str) -> Event {
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(20), rx.recv())
            .await.expect("event timeout").expect("channel closed");
        if serde_json::to_value(&ev).unwrap()["type"] == want { return ev; }
    }
}

#[tokio::test]
async fn kicked_guest_cannot_rejoin() {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut host = host::start(host::HostOptions {
        port: l.local_addr().unwrap().port(),
        world_name: "Kick World".into(),
        secret_key: None,
        use_relays: false,
    }).await.unwrap();
    let mut host_events = host.take_events().unwrap();

    let mut guest = guest::join(guest::GuestOptions {
        code: host.invite_code.clone(),
        player_name: "Griefer".into(),
        use_relays: false,
        preferred_port: None,
    }).await.unwrap();
    let mut guest_events = guest.take_events().unwrap();

    let Event::GuestJoined { id, .. } = next_event(&mut host_events, "guest_joined").await
        else { unreachable!() };
    next_event(&mut guest_events, "joined_host").await;

    assert!(host.kick(&id).await, "kick должен найти соединение");
    next_event(&mut guest_events, "disconnected").await;

    // Гость пытается переподключаться, но блок-лист не пускает:
    // joined_host больше не должен появиться.
    let rejoined = tokio::time::timeout(Duration::from_secs(10), async {
        next_event(&mut guest_events, "joined_host").await
    }).await;
    assert!(rejoined.is_err(), "кикнутый гость переподключился: {rejoined:?}");

    guest.close().await;
    host.close().await;
}
```

- [ ] **Step 2: Убедиться, что падает** — `cargo test -p mine-host-core --test kick` → нет метода `kick`.

- [ ] **Step 3: Реализация**

В `core/src/host.rs`:

Импорты/поля:
```rust
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
// в struct HostSession добавить:
    conns: Arc<tokio::sync::Mutex<HashMap<String, Connection>>>,
    blocked: Arc<tokio::sync::Mutex<HashSet<String>>>,
```

Методы HostSession:
```rust
    /// Выгоняет гостя и заносит в блок-лист (до конца сессии).
    pub async fn kick(&self, id: &str) -> bool {
        self.blocked.lock().await.insert(id.to_string());
        match self.conns.lock().await.remove(id) {
            Some(c) => { c.close(0u32.into(), b"kicked"); true }
            None => false,
        }
    }

    /// Снимок активных гостей: (id, rtt_ms).
    pub async fn guests(&self) -> Vec<(String, u32)> {
        self.conns.lock().await.iter()
            .map(|(id, c)| (id.clone(), crate::net::conn_rtt_ms(c)))
            .collect()
    }
```

В `start()` создать обе мапы до accept-цикла и передать в него; в accept-цикле после установления соединения:
```rust
        let conns = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let blocked = Arc::new(tokio::sync::Mutex::new(HashSet::new()));
        // ... клоны в accept-task ...
        while let Some(incoming) = ep.accept().await {
            let Ok(conn) = incoming.await else { continue };
            let id = conn.remote_id().to_string();
            if blocked.lock().await.contains(&id) {
                conn.close(0u32.into(), b"blocked");
                continue;
            }
            conns.lock().await.insert(id, conn.clone());
            tokio::spawn(handle_conn(/* как раньше + clones conns */));
        }
```
В `handle_conn` при выходе из цикла accept_bi (гость ушёл) — `conns.lock().await.remove(&id_str);` перед отправкой GuestLeft. Сигнатура `handle_conn` получает `conns: Arc<Mutex<HashMap<String, Connection>>>`.

- [ ] **Step 4: Тесты зелёные** — `cargo test -p mine-host-core --test kick`, затем полный прогон.
- [ ] **Step 5: Commit** — `git commit -m "feat(core): kick guests with session blocklist"`

---

### Task 4: Диагностика соединений (core)

**Files:**
- Modify: `core/src/net.rs`, `core/src/host.rs`, `core/src/guest.rs`
- Test: `core/tests/diagnostics.rs`

- [ ] **Step 1: Падающий тест**

`core/tests/diagnostics.rs`:
```rust
use std::time::Duration;

use mine_host_core::{guest, host};
use tokio::net::TcpListener;

#[tokio::test]
async fn guest_diagnostics_show_host() {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let host = host::start(host::HostOptions {
        port: l.local_addr().unwrap().port(),
        world_name: "Diag".into(),
        secret_key: None,
        use_relays: false,
    }).await.unwrap();
    let guest = guest::join(guest::GuestOptions {
        code: host.invite_code.clone(),
        player_name: "diag".into(),
        use_relays: false,
        preferred_port: None,
    }).await.unwrap();

    // ждём установления
    let mut diag = None;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(250)).await;
        if let Some(d) = guest.diagnostics().await { diag = Some(d); break; }
    }
    let d = diag.expect("диагностика недоступна");
    assert!(!d.peer_id.is_empty());
    assert!(!d.addrs.is_empty(), "должен быть хотя бы один активный адрес");

    guest.close().await;
    host.close().await;
}
```

- [ ] **Step 2: Убедиться, что падает** — нет `Diagnostics`/`diagnostics()`.

- [ ] **Step 3: Реализация**

`core/src/net.rs`:
```rust
/// Снимок состояния соединения для экрана диагностики.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Diagnostics {
    pub peer_id: String,
    pub rtt_ms: u32,
    pub path: crate::events::PathKind,
    /// Активные транспортные адреса пира: "ip:1.2.3.4:567" / "relay:https://…".
    pub addrs: Vec<String>,
    /// Наши собственные адреса (что узнает о нас пир).
    pub self_addrs: Vec<String>,
}

pub async fn diagnostics(ep: &Endpoint, conn: &Connection) -> Diagnostics {
    let id = conn.remote_id();
    let mut addrs = Vec::new();
    if let Some(info) = ep.remote_info(id).await {
        for a in info.addrs() {
            if matches!(a.usage(), TransportAddrUsage::Active) {
                addrs.push(format!("{:?}", a.addr()));
            }
        }
    }
    let self_addrs = ep.addr().addrs.iter().map(|a| format!("{a:?}")).collect();
    Diagnostics {
        peer_id: id.to_string(),
        rtt_ms: conn_rtt_ms(conn),
        path: path_kind(ep, id).await,
        addrs,
        self_addrs,
    }
}
```
(`ep.addr().addrs` — если поле называется иначе, см. `iroh::EndpointAddr`; правится только здесь.)

`core/src/guest.rs` — общий хэндл текущего соединения:
```rust
// поле GuestSession:
    current_conn: Arc<tokio::sync::Mutex<Option<Connection>>>,
// метод:
    pub async fn diagnostics(&self) -> Option<crate::net::Diagnostics> {
        let conn = self.current_conn.lock().await.clone()?;
        Some(crate::net::diagnostics(&self.endpoint, &conn).await)
    }
```
В `join()` создать `current_conn`, передать в `run_guest`; там после успешного `connect` — `*current_conn.lock().await = Some(conn.clone());`, после выхода из `serve_conn` — `None`.

`core/src/host.rs`:
```rust
    pub async fn diagnostics(&self) -> Vec<crate::net::Diagnostics> {
        let conns: Vec<Connection> = self.conns.lock().await.values().cloned().collect();
        let mut out = Vec::new();
        for c in conns { out.push(crate::net::diagnostics(&self.endpoint, &c).await); }
        out
    }
```

- [ ] **Step 4: Тесты зелёные** — `cargo test -p mine-host-core`.
- [ ] **Step 5: Commit** — `git commit -m "feat(core): connection diagnostics snapshot"`

---

### Task 5: Команды kick/rotate/diagnostics + UI (app)

**Files:**
- Modify: `app/src-tauri/src/lib.rs`
- Modify: `app/src/routes/+page.svelte`

- [ ] **Step 1: Команды**

В `app/src-tauri/src/lib.rs`:
```rust
#[tauri::command]
async fn kick(state: State<'_, AppState>, id: String) -> Result<bool, String> {
    match &*state.session.lock().await {
        Some(Session::Host(h)) => Ok(h.kick(&id).await),
        _ => Err("нет активной хост-сессии".into()),
    }
}

/// Новый код приглашения = новый ключ хоста (старые коды перестают работать).
#[tauri::command]
async fn rotate_code(app: AppHandle, state: State<'_, AppState>) -> Result<String, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(dir.join("host.key"));
    // перезапускаем хост-сессию с тем же портом
    let (port, world_name) = match &*state.session.lock().await {
        Some(Session::Host(_)) => { /* порт берём из новой команды ниже */ (None, None) }
        _ => return Err("нет активной хост-сессии".into()),
    };
    let _ = (port, world_name);
    Err("используй stop + start_host — см. фронтенд".into())
}

#[tauri::command]
async fn diagnostics(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    match &*state.session.lock().await {
        Some(Session::Host(h)) => Ok(serde_json::to_value(h.diagnostics().await).unwrap()),
        Some(Session::Guest(g)) => Ok(serde_json::to_value(g.diagnostics().await).unwrap()),
        None => Ok(serde_json::Value::Null),
    }
}
```
Упрощение rotate: вместо сложного перезапуска внутри состояния, фронтенд делает `stop` → удалить ключ → `start_host`. Поэтому команда сводится к удалению ключа:
```rust
#[tauri::command]
async fn rotate_code(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    stop_inner(&state).await;
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::remove_file(dir.join("host.key")).map_err(|e| e.to_string())?;
    Ok(())
}
```
(оставить именно эту версию; первую не писать). Зарегистрировать все три в `generate_handler!`.

- [ ] **Step 2: UI**

В `+page.svelte`:
- у каждого гостя в списке хоста кнопка `✕` → `invoke("kick", { id })`, после — `delete peers[id]`;
- кнопка «Новый код» на экране хоста: `await invoke("rotate_code"); await startHost();`;
- сворачиваемый блок «Диагностика» (на обоих экранах): кнопка «Обновить» → `diag = await invoke("diagnostics")`, вывод `<pre>{JSON.stringify(diag, null, 2)}</pre>`.

```svelte
<!-- внутри экрана host, в each peers: -->
<li>
  {pathIcon(p.path)} {p.name} — {p.rtt_ms} ms
  <button class="mini danger" onclick={() => kickPeer(id)}>✕</button>
</li>
<!-- под списком: -->
<button onclick={rotateCode}>Новый код приглашения</button>

<!-- общий блок внизу обоих экранов host/guest: -->
<details>
  <summary>Диагностика</summary>
  <button class="mini" onclick={refreshDiag}>Обновить</button>
  <pre class="diag">{diagText}</pre>
</details>
```
Скрипт:
```ts
let diagText = $state("");
async function refreshDiag() {
  diagText = JSON.stringify(await invoke("diagnostics"), null, 2);
}
async function kickPeer(id: string) {
  await invoke("kick", { id });
  delete peers[id];
}
async function rotateCode() {
  await invoke("rotate_code");
  await startHost();
}
```
Стили: `.mini { padding: 2px 8px; font-size: 12px; } pre.diag { font-size: 11px; overflow-x: auto; background: #23272f; padding: 8px; border-radius: 8px; }`

- [ ] **Step 3: Проверка** — `cargo check -p mine-host-app` и `cd app && npm run build` зелёные.
- [ ] **Step 4: Commit** — `git commit -m "feat(app): kick, code rotation and diagnostics panel"`

---

### Task 6: Ручной порт + недавние миры (app, только UI)

**Files:**
- Modify: `app/src/routes/+page.svelte`

- [ ] **Step 1: Ручной порт**

На home-экране под кнопкой «Хостить мир»:
```svelte
<details>
  <summary>У меня выделенный сервер</summary>
  <div class="join-box">
    <input placeholder="Порт сервера (например 25565)" bind:value={manualPort} />
    <button disabled={busy || !portValid(manualPort)} onclick={() => startHost(Number(manualPort))}>
      Хостить сервер на порту {manualPort || "…"}
    </button>
  </div>
</details>
```
```ts
let manualPort = $state("");
const portValid = (s: string) => /^\d+$/.test(s) && +s > 0 && +s < 65536;
async function startHost(port?: number) {
  busy = true; error = "";
  try {
    inviteCode = await invoke<string>("start_host", { manualPort: port ?? null });
    mode = "host";
  } catch (e) { error = String(e); } finally { busy = false; }
}
```
(существующий вызов `startHost` с кнопки «Хостить мир» остаётся без аргумента).

- [ ] **Step 2: Недавние миры (localStorage)**

```ts
type Recent = { code: string; world: string; at: number };
let recents = $state<Recent[]>(JSON.parse(localStorage.getItem("mh-recents") ?? "[]"));
function saveRecent(code: string, world: string) {
  recents = [{ code, world, at: Date.now() },
             ...recents.filter(r => r.code !== code)].slice(0, 5);
  localStorage.setItem("mh-recents", JSON.stringify(recents));
}
```
В `handleEvent` в ветке `joined_host` добавить `saveRecent(joinCode.trim(), ev.world_name);`.
На home-экране под join-box:
```svelte
{#if recents.length}
  <h3>Недавние миры</h3>
  {#each recents as r (r.code)}
    <button class="recent" disabled={busy}
      onclick={() => { joinCode = r.code; joinHost(); }}>
      ⟳ {r.world}
    </button>
  {/each}
{/if}
```
Стиль: `.recent { background: #2a4d7a; text-align: left; }`

- [ ] **Step 3: Проверка** — `cd app && npm run build`.
- [ ] **Step 4: Commit** — `git commit -m "feat(app): manual server port and recent worlds"`

---

### Task 7: Deep link minehost:// + single instance (app)

**Files:**
- Modify: `app/src-tauri/Cargo.toml`, `app/src-tauri/src/lib.rs`, `app/src-tauri/tauri.conf.json`, `app/src-tauri/capabilities/default.json`, `app/package.json`, `app/src/routes/+page.svelte`

- [ ] **Step 1: Зависимости**

```bash
cargo add -p mine-host-app tauri-plugin-deep-link tauri-plugin-single-instance
cd app && npm install @tauri-apps/plugin-deep-link
```

- [ ] **Step 2: Конфигурация**

`tauri.conf.json` — добавить на верхний уровень:
```json
"plugins": {
  "deep-link": { "desktop": { "schemes": ["minehost"] } }
}
```
`capabilities/default.json` — в permissions добавить `"deep-link:default"`.

- [ ] **Step 3: Rust-обвязка**

В `run()` (single-instance должен идти ПЕРВЫМ плагином):
```rust
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            // Вторая копия запущена по клику на ссылку: фокус + пробрасываем argv.
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.set_focus();
            }
            let _ = app.emit("mh-deeplink", argv);
        }))
        .plugin(tauri_plugin_deep_link::init())
```

- [ ] **Step 4: Фронтенд**

```ts
import { onOpenUrl } from "@tauri-apps/plugin-deep-link";

function tryJoinFromLink(s: string) {
  if (s.startsWith("minehost://")) { joinCode = s; joinHost(); }
}
$effect(() => {
  onOpenUrl((urls) => urls.forEach(tryJoinFromLink));
  const un = listen<string[]>("mh-deeplink", (e) => e.payload.forEach(tryJoinFromLink));
  return () => { un.then(f => f()); };
});
```
Кнопка копирования на экране хоста копирует ссылку:
```ts
async function copyCode() {
  await navigator.clipboard.writeText(`minehost://join/${inviteCode.replace(/^mh:/, "mh:")}`);
  ...
}
```
Упростить: бэкенд сразу отдаёт и код, и ссылку — изменить `start_host` так, чтобы возвращал код как сейчас, а ссылку строить на фронте: `const inviteLink = $derived("minehost://join/" + inviteCode);` и копировать `inviteLink`; код показывать как раньше.

- [ ] **Step 5: Проверка** — `cargo check -p mine-host-app`, `npm run build`; ручная: после установки релизной сборки клик по `minehost://join/<код>` открывает приложение и подключает.
- [ ] **Step 6: Commit** — `git commit -m "feat(app): minehost:// deep links with single-instance forwarding"`

---

### Task 8: Запуск server.jar из приложения (app)

**Files:**
- Create: `app/src-tauri/src/server.rs`
- Modify: `app/src-tauri/src/lib.rs`, `app/src-tauri/capabilities/default.json`, `app/package.json`, `app/src/routes/+page.svelte`

- [ ] **Step 1: Зависимость диалога выбора файла**

```bash
cargo add -p mine-host-app tauri-plugin-dialog
cd app && npm install @tauri-apps/plugin-dialog
```
capabilities → `"dialog:default"`. В `run()` → `.plugin(tauri_plugin_dialog::init())`.

- [ ] **Step 2: server.rs**

```rust
//! Запуск и остановка выделенного Minecraft-сервера (server.jar).
use std::path::{Path, PathBuf};
use std::time::Duration;

use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

#[derive(Default)]
pub struct ServerState(pub Mutex<Option<Child>>);

/// Порт из server.properties рядом с jar (или 25565).
fn read_port(jar: &Path) -> u16 {
    let props = jar.parent().map(|d| d.join("server.properties"));
    if let Some(p) = props {
        if let Ok(text) = std::fs::read_to_string(p) {
            for line in text.lines() {
                if let Some(v) = line.strip_prefix("server-port=") {
                    if let Ok(port) = v.trim().parse() { return port; }
                }
            }
        }
    }
    25565
}

pub async fn start(
    app: AppHandle,
    state: &ServerState,
    jar_path: String,
    ram_mb: u32,
    accept_eula: bool,
) -> Result<u16, String> {
    let jar = PathBuf::from(&jar_path);
    let dir = jar.parent().ok_or("некорректный путь к jar")?.to_path_buf();
    if accept_eula {
        std::fs::write(dir.join("eula.txt"), "eula=true\n").map_err(|e| e.to_string())?;
    }
    let port = read_port(&jar);

    let mut child = Command::new("java")
        .current_dir(&dir)
        .args([
            &format!("-Xms{}M", ram_mb.min(2048)),
            &format!("-Xmx{ram_mb}M"),
            // флаги Айкара — стандарт для игровых серверов
            "-XX:+UseG1GC", "-XX:+ParallelRefProcEnabled", "-XX:MaxGCPauseMillis=200",
            "-XX:+UnlockExperimentalVMOptions", "-XX:+DisableExplicitGC",
            "-jar",
        ])
        .arg(&jar)
        .arg("nogui")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("не удалось запустить java: {e}"))?;

    // стримим последнюю строку лога в UI
    if let Some(out) = child.stdout.take() {
        let app = app.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(out).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let _ = app.emit("mh-server-log", &line);
            }
            let _ = app.emit("mh-server-log", "[процесс сервера завершился]");
        });
    }
    *state.0.lock().await = Some(child);

    // ждём, пока сервер откроет порт (тяжёлый модпак грузится минутами)
    for _ in 0..600 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            return Ok(port);
        }
        if state.0.lock().await.is_none() { return Err("сервер остановлен".into()); }
    }
    Err("сервер не открыл порт за 10 минут".into())
}

pub async fn stop(state: &ServerState) {
    if let Some(mut child) = state.0.lock().await.take() {
        // вежливая остановка: команда stop в stdin, потом kill
        use tokio::io::AsyncWriteExt;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(b"stop\n").await;
        }
        let _ = tokio::time::timeout(Duration::from_secs(30), child.wait()).await;
        let _ = child.kill().await;
    }
}
```

- [ ] **Step 3: Команды в lib.rs**

```rust
mod server;
// в Builder: .manage(server::ServerState::default())

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
```
Добавить оба в `generate_handler!`. В `stop` (сессии) сервер НЕ трогаем — независимые вещи.

- [ ] **Step 4: UI-панель**

На home-экране, внутри `<details>` «У меня выделенный сервер» добавить вариант «запустить сервер из приложения»:
```svelte
<div class="join-box">
  <button class="mini" onclick={pickJar}>{jarPath ? jarPath.split(/[\\/]/).pop() : "Выбрать server.jar"}</button>
  <input placeholder="RAM, МБ (например 6144)" bind:value={ramMb} />
  <label><input type="checkbox" bind:checked={eula} /> Принимаю Minecraft EULA</label>
  <button disabled={busy || !jarPath || !eula} onclick={startServerAndHost}>
    Запустить сервер и хостить
  </button>
  {#if serverLog}<p class="muted log">{serverLog}</p>{/if}
</div>
```
```ts
import { open } from "@tauri-apps/plugin-dialog";
let jarPath = $state(""); let ramMb = $state("4096"); let eula = $state(false);
let serverLog = $state("");
$effect(() => {
  const un = listen<string>("mh-server-log", (e) => (serverLog = e.payload));
  return () => { un.then(f => f()); };
});
async function pickJar() {
  const p = await open({ filters: [{ name: "Server JAR", extensions: ["jar"] }] });
  if (typeof p === "string") jarPath = p;
}
async function startServerAndHost() {
  busy = true; error = "";
  try {
    const port = await invoke<number>("server_start",
      { jarPath, ramMb: Number(ramMb), acceptEula: eula });
    await startHost(port);
  } catch (e) { error = String(e); } finally { busy = false; }
}
```
И в `stopSession` дополнительно `await invoke("server_stop");`.

- [ ] **Step 5: Проверка** — `cargo check -p mine-host-app`, `npm run build`.
- [ ] **Step 6: Commit** — `git commit -m "feat(app): launch and host a dedicated server.jar"`

---

### Task 9: CI (три ОС), автообновления, README

**Files:**
- Create: `.github/workflows/ci.yml`, `.github/workflows/release.yml`
- Modify: `core/tests/lan_multicast.rs` (CI-guard), `app/src-tauri/Cargo.toml`, `app/src-tauri/src/lib.rs`, `app/src-tauri/tauri.conf.json`, `README.md`

- [ ] **Step 1: CI-guard мультикаст-теста** (на CI-раннерах мультикаст часто запрещён)

В начало `beacon_is_discovered`:
```rust
    if std::env::var("CI").is_ok() {
        eprintln!("skip: multicast недоступен на CI-раннерах");
        return;
    }
```

- [ ] **Step 2: ci.yml**

```yaml
name: CI
on:
  push: { branches: [master, main] }
  pull_request:
jobs:
  test:
    runs-on: ${{ matrix.os }}
    strategy:
      matrix:
        os: [ubuntu-latest, windows-latest, macos-latest]
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - name: Linux deps
        if: runner.os == 'Linux'
        run: |
          sudo apt-get update
          sudo apt-get install -y libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf
      - run: cargo test -p mine-host-core
```

- [ ] **Step 3: release.yml** (tauri-action, по тегу v*)

```yaml
name: Release
on:
  push:
    tags: ["v*"]
jobs:
  build:
    permissions: { contents: write }
    runs-on: ${{ matrix.os }}
    strategy:
      fail-fast: false
      matrix:
        os: [ubuntu-latest, windows-latest, macos-latest]
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-node@v4
        with: { node-version: 22 }
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - name: Linux deps
        if: runner.os == 'Linux'
        run: |
          sudo apt-get update
          sudo apt-get install -y libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf
      - run: cd app && npm install
      - uses: tauri-apps/tauri-action@v0
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
          TAURI_SIGNING_PRIVATE_KEY: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY }}
          TAURI_SIGNING_PRIVATE_KEY_PASSWORD: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY_PASSWORD }}
        with:
          projectPath: app
          tagName: ${{ github.ref_name }}
          releaseName: "MineHost ${{ github.ref_name }}"
          releaseDraft: true
          includeUpdaterJson: true
```

- [ ] **Step 4: Updater**

```bash
cargo add -p mine-host-app tauri-plugin-updater
cd app && npm install @tauri-apps/plugin-updater
npm run tauri signer generate -- -w "$env:USERPROFILE/.tauri/minehost.key"
```
(приватный ключ НЕ коммитить; пароль можно пустой). Публичный ключ из вывода → `tauri.conf.json`:
```json
"plugins": {
  "updater": {
    "pubkey": "<PUBKEY_BASE64_ИЗ_ВЫВОДА>",
    "endpoints": ["https://github.com/OWNER/mine-host/releases/latest/download/latest.json"]
  }
}
```
`capabilities/default.json` → `"updater:default"`. В `run()` → `.plugin(tauri_plugin_updater::Builder::new().build())`.
Фронтенд — кнопка в `<details>` диагностики:
```ts
import { check } from "@tauri-apps/plugin-updater";
let updateMsg = $state("");
async function checkUpdate() {
  try {
    const u = await check();
    if (u) { updateMsg = `Доступна ${u.version}, скачиваю…`; await u.downloadAndInstall(); updateMsg = "Установлено — перезапусти приложение"; }
    else updateMsg = "У тебя последняя версия";
  } catch (e) { updateMsg = `Проверка недоступна: ${e}`; }
}
```
`OWNER` остаётся плейсхолдером до создания GitHub-репозитория — README фиксирует это.

- [ ] **Step 5: README** — дополнить разделами «Релизы и автообновления» (как создать репо, секреты `TAURI_SIGNING_PRIVATE_KEY`, тег v0.2.0 → установщики в Releases) и «Возможности» (кик, deep links, выделенный сервер, диагностика).

- [ ] **Step 6: Полный прогон** — `cargo test && cd app && npm run build && cargo check -p mine-host-app`.
- [ ] **Step 7: Commit** — `git commit -m "ci: three-OS pipeline, release workflow and auto-updater"`

---

## Самопроверка плана

- Покрытие: №1 порт (T6), №2 короткие коды → deep links (T1, T7), №3 CI (T9), №4 недавние миры (T6), №6 QUIC (T2), №7 диагностика (T4, T5), №8 updater (T9), №9 кик/ротация (T3, T5), №10 сервер (T8). №5 — исключён пользователем.
- Типы сквозные: `HostSession::{kick, guests, diagnostics}`, `GuestSession::diagnostics`, `net::Diagnostics`, команды `kick/rotate_code/diagnostics/server_start/server_stop` — согласованы между задачами.
