//! Запуск и остановка выделенного Minecraft-сервера (server.jar).
use std::path::{Path, PathBuf};
use std::time::Duration;

use tauri::{AppHandle, Emitter};
use tokio::io::AsyncBufReadExt;
use tokio::io::BufReader;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

#[derive(Default)]
pub struct ServerState(pub Mutex<Option<Child>>);

/// Порт из server.properties рядом с jar (или 25565).
fn read_port(jar: &Path) -> u16 {
    if let Some(props) = jar.parent().map(|d| d.join("server.properties")) {
        if let Ok(text) = std::fs::read_to_string(props) {
            for line in text.lines() {
                if let Some(v) = line.strip_prefix("server-port=") {
                    if let Ok(port) = v.trim().parse() {
                        return port;
                    }
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
    if state.0.lock().await.is_some() {
        return Err("сервер уже запущен".into());
    }
    let jar = PathBuf::from(&jar_path);
    let dir = jar.parent().ok_or("некорректный путь к jar")?.to_path_buf();
    if accept_eula {
        std::fs::write(dir.join("eula.txt"), "eula=true\n").map_err(|e| e.to_string())?;
    }
    let port = read_port(&jar);

    let mut child = Command::new("java")
        .current_dir(&dir)
        .args([
            // Xms=Xmx + AlwaysPreTouch — по Айкару: без ресайзов кучи в рантайме
            &format!("-Xms{ram_mb}M"),
            &format!("-Xmx{ram_mb}M"),
            // полный набор флагов Айкара — меньше GC-пауз (= лагов) на сервере
            "-XX:+UseG1GC",
            "-XX:+ParallelRefProcEnabled",
            "-XX:MaxGCPauseMillis=200",
            "-XX:+UnlockExperimentalVMOptions",
            "-XX:+DisableExplicitGC",
            "-XX:+AlwaysPreTouch",
            "-XX:G1NewSizePercent=30",
            "-XX:G1MaxNewSizePercent=40",
            "-XX:G1HeapRegionSize=8M",
            "-XX:G1ReservePercent=20",
            "-XX:G1HeapWastePercent=5",
            "-XX:G1MixedGCCountTarget=4",
            "-XX:InitiatingHeapOccupancyPercent=15",
            "-XX:G1MixedGCLiveThresholdPercent=90",
            "-XX:G1RSetUpdatingPauseTimePercent=5",
            "-XX:SurvivorRatio=32",
            "-XX:+PerfDisableSharedMem",
            "-XX:MaxTenuringThreshold=1",
            "-jar",
        ])
        .arg(&jar)
        .arg("nogui")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("не удалось запустить java: {e} (установлена ли Java?)"))?;

    // стримим строки лога в UI
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
    // stderr тоже в UI: раньше падения JVM и ошибки флагов уходили в никуда
    if let Some(err) = child.stderr.take() {
        let app = app.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(err).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let _ = app.emit("mh-server-log", &line);
            }
        });
    }
    *state.0.lock().await = Some(child);

    // ждём, пока сервер откроет порт (тяжёлый модпак грузится минутами)
    for _ in 0..600 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            return Ok(port);
        }
        // Упавшая JVM (нет Java нужной версии, битый jar, мало RAM) не должна
        // держать пользователя в «Запускаю…» все 10 минут.
        let mut guard = state.0.lock().await;
        match guard.as_mut() {
            None => return Err("сервер остановлен".into()),
            Some(child) => {
                if let Ok(Some(status)) = child.try_wait() {
                    *guard = None;
                    return Err(format!("сервер завершился ({status}) — смотри лог"));
                }
            }
        }
    }
    Err("сервер не открыл порт за 10 минут — смотри лог".into())
}

pub async fn stop(state: &ServerState) {
    if let Some(mut child) = state.0.lock().await.take() {
        // вежливая остановка: команда stop в stdin (сохранит мир), потом kill
        use tokio::io::AsyncWriteExt;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(b"stop\n").await;
        }
        let _ = tokio::time::timeout(Duration::from_secs(30), child.wait()).await;
        let _ = child.kill().await;
    }
}
