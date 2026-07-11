//! Адаптер над iroh::Endpoint. Вся специфика версии iroh — здесь.
use std::time::Duration;

use anyhow::Result;
use iroh::endpoint::{
    presets, Connection, PathId, QuicTransportConfig, RecvStream, SendStream, TransportAddrUsage,
    VarInt,
};
use iroh::{Endpoint, SecretKey, Watcher};
use tokio::sync::mpsc;

use crate::events::Event;
use crate::ALPN;

/// Буфер копирования TCP↔QUIC. Больше — меньше сисколлов на всплесках чанков;
/// задержки не добавляет (copy не ждёт заполнения буфера).
const COPY_BUF: usize = 64 * 1024;

/// relays=false — герметичный режим для тестов (без сети n0:
/// presets::Minimal ставит только обязательный crypto provider).
pub async fn make_endpoint(secret_key: Option<SecretKey>, relays: bool) -> Result<Endpoint> {
    let mut builder =
        if relays { Endpoint::builder(presets::N0) } else { Endpoint::builder(presets::Minimal) };
    builder = builder.alpns(vec![ALPN.to_vec()]);
    // Окна — компромисс «пропускная способность ↔ задержка». Гигантские окна
    // (16-64 МБ) — это bufferbloat: когда аплоад хоста медленнее, чем Minecraft
    // отдаёт чанки, туннель копит многосекундную очередь, пинг растёт без
    // предела, keep-alive'ы тонут за чанками и клиента выкидывает по таймауту.
    // 3 МБ на поток ≈ 120 Мбит/с при RTT 200 мс — логину в тяжёлый модпак
    // хватает, а очередь ограничена долями секунды; дальше backpressure по TCP
    // доходит до сервера, и Netty сам притормаживает отправку чанков.
    let transport = QuicTransportConfig::builder()
        .stream_receive_window(VarInt::from_u32(3 * 1024 * 1024))
        .receive_window(VarInt::from_u32(8 * 1024 * 1024))
        .send_window(8 * 1024 * 1024)
        // BBR держит очередь у бутылочного горлышка минимальной. Дефолтный
        // Cubic заполняет буферы домашнего роутера/релея до потерь — именно
        // так выглядит «пинг всё больше и больше» под нагрузкой.
        .congestion_controller_factory(std::sync::Arc::new(
            noq_proto::congestion::BbrConfig::default(),
        ))
        // Голосовые датаграммы: маленький буфер отправки — лучше потерять
        // кадр, чем проиграть его с секундным опозданием.
        .datagram_send_buffer_size(64 * 1024)
        // Дефолтные 30 с — это полминуты «призрака»: гость закрыл крышку
        // ноутбука, а хост всё ещё показывает его онлайн. iroh шлёт keep-alive
        // каждые 5 с, так что 15 с = три пропущенных подряд — мёртв.
        .max_idle_timeout(Some(VarInt::from_u32(15_000).into()))
        .build();
    builder = builder.transport_config(transport);
    if let Some(key) = secret_key {
        builder = builder.secret_key(key);
    }
    Ok(builder.bind().await?)
}

/// Склеивает локальный TCP-поток с парой QUIC-стримов до закрытия любой
/// из сторон. Единственное место проксирования игрового трафика — хост
/// и гость зовут его со своих концов туннеля.
pub(crate) async fn splice_tcp_quic(
    mut tcp: tokio::net::TcpStream,
    send: SendStream,
    recv: RecvStream,
) {
    let mut quic = tokio::io::join(recv, send);
    let _ = tokio::io::copy_bidirectional_with_sizes(&mut tcp, &mut quic, COPY_BUF, COPY_BUF).await;
}

/// Тикер статуса для UI: раз в 2 с шлёт rtt и вид пути, пока живо соединение.
/// Сам завершается по close_reason; handle нужен тем, кто хочет прибить раньше.
pub(crate) fn spawn_status_ticker(
    conn: &Connection,
    tx: mpsc::Sender<Event>,
) -> tokio::task::JoinHandle<()> {
    let conn = conn.clone();
    tokio::spawn(async move {
        let id = conn.remote_id().to_string();
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            if conn.close_reason().is_some() {
                break;
            }
            let _ = tx
                .send(Event::PeerStatus {
                    id: id.clone(),
                    rtt_ms: conn_rtt_ms(&conn),
                    path: path_kind(&conn),
                })
                .await;
        }
    })
}

/// RTT главного пути передачи в миллисекундах (0 — пока неизвестно).
/// PathId::ZERO — только запасной вариант: при multipath нулевой путь может
/// быть релейным, когда трафик давно идёт напрямую.
pub fn conn_rtt_ms(conn: &Connection) -> u32 {
    let selected =
        conn.paths().get().iter().find(|p| p.is_selected() && !p.is_closed()).and_then(|p| p.rtt());
    selected.or_else(|| conn.rtt(PathId::ZERO)).map(|d| d.as_millis() as u32).unwrap_or(0)
}

/// Один сетевой путь соединения — для экрана диагностики.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PathDiag {
    pub addr: String,
    pub relay: bool,
    /// Главный путь передачи (по нему идёт трафик прямо сейчас).
    pub selected: bool,
    pub rtt_ms: u32,
}

/// Снимок состояния соединения для экрана диагностики.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Diagnostics {
    pub peer_id: String,
    pub rtt_ms: u32,
    pub path: crate::events::PathKind,
    /// Открытые QUIC-пути к пиру с их RTT.
    pub paths: Vec<PathDiag>,
    /// Активные транспортные адреса пира: Ip(..) / Relay(..).
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
    let paths = conn
        .paths()
        .get()
        .iter()
        .filter(|p| !p.is_closed())
        .map(|p| PathDiag {
            addr: format!("{:?}", p.remote_addr()),
            relay: p.is_relay(),
            selected: p.is_selected(),
            rtt_ms: p.rtt().map(|d| d.as_millis() as u32).unwrap_or(0),
        })
        .collect();
    let self_addrs = ep.addr().addrs.iter().map(|a| format!("{a:?}")).collect();
    Diagnostics {
        peer_id: id.to_string(),
        rtt_ms: conn_rtt_ms(conn),
        path: path_kind(conn),
        paths,
        addrs,
        self_addrs,
    }
}

/// Направление главного пути передачи: напрямую или через релей.
/// Смотрим selected-путь QUIC-multipath — это тот, по которому реально
/// идёт трафик, а не просто «какие адреса известны».
pub fn path_kind(conn: &Connection) -> crate::events::PathKind {
    use crate::events::PathKind;
    let (mut direct, mut relay) = (false, false);
    for p in conn.paths().get().iter() {
        if p.is_closed() || !p.is_selected() {
            continue;
        }
        if p.is_relay() {
            relay = true;
        } else {
            direct = true;
        }
    }
    match (direct, relay) {
        (true, false) => PathKind::Direct,
        (false, true) => PathKind::Relay,
        (true, true) => PathKind::Mixed,
        _ => PathKind::Unknown,
    }
}
