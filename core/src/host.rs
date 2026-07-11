//! Хост-сессия: принимает гостей и проксирует их в локальный Minecraft.
use std::collections::{HashMap, HashSet};
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use iroh::endpoint::{Connection, SendDatagramError};
use iroh::{Endpoint, SecretKey};
use tokio::io::BufReader;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};

use crate::events::Event;
use crate::{invite, net, protocol};

pub struct HostOptions {
    /// Локальный порт Minecraft (LAN-мир или выделенный сервер).
    pub port: u16,
    /// Имя мира — уйдёт гостям и попадёт в их LAN-список.
    pub world_name: String,
    /// Постоянный ключ хоста (стабильный NodeId между запусками).
    pub secret_key: Option<SecretKey>,
    /// false — герметичный режим для тестов.
    pub use_relays: bool,
}

type ConnMap = Arc<tokio::sync::Mutex<HashMap<String, Connection>>>;

pub struct HostSession {
    pub invite_code: String,
    events: Option<mpsc::Receiver<Event>>,
    endpoint: Endpoint,
    accept_task: tokio::task::JoinHandle<()>,
    conns: ConnMap,
    blocked: Arc<tokio::sync::Mutex<HashSet<String>>>,
}

impl HostSession {
    /// Receiver событий (забирается один раз — для UI).
    pub fn take_events(&mut self) -> Option<mpsc::Receiver<Event>> {
        self.events.take()
    }

    /// Выгоняет гостя и заносит в блок-лист (до конца сессии).
    pub async fn kick(&self, id: &str) -> bool {
        self.blocked.lock().await.insert(id.to_string());
        match self.conns.lock().await.remove(id) {
            Some(c) => {
                c.close(0u32.into(), b"kicked");
                true
            }
            None => false,
        }
    }

    /// Диагностика всех активных гостевых соединений.
    pub async fn diagnostics(&self) -> Vec<crate::net::Diagnostics> {
        let conns: Vec<Connection> = self.conns.lock().await.values().cloned().collect();
        let mut out = Vec::new();
        for c in conns {
            out.push(crate::net::diagnostics(&self.endpoint, &c).await);
        }
        out
    }

    /// Снимок активных гостей: (id, rtt_ms).
    pub async fn guests(&self) -> Vec<(String, u32)> {
        self.conns
            .lock()
            .await
            .iter()
            .map(|(id, c)| (id.clone(), crate::net::conn_rtt_ms(c)))
            .collect()
    }

    pub async fn close(self) {
        self.accept_task.abort();
        self.endpoint.close().await;
    }
}

pub async fn start(opts: HostOptions) -> Result<HostSession> {
    let endpoint = net::make_endpoint(opts.secret_key.clone(), opts.use_relays).await?;
    if opts.use_relays {
        // Дождаться релея, чтобы тикет содержал relay url. С таймаутом:
        // без интернета online() висит вечно, а «хост не стартует» хуже,
        // чем тикет без релея (прямые адреса в нём всё равно есть).
        if tokio::time::timeout(Duration::from_secs(10), endpoint.online()).await.is_err() {
            tracing::warn!("релей недоступен за 10 с — стартуем без него");
        }
    }
    let invite_code = invite::encode(&endpoint.addr());
    let (tx, rx) = mpsc::channel::<Event>(64);
    let _ = tx.try_send(Event::HostReady { invite_code: invite_code.clone() });

    let conns: ConnMap = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let blocked = Arc::new(tokio::sync::Mutex::new(HashSet::new()));

    let ep = endpoint.clone();
    let port = opts.port;
    let world_name = opts.world_name.clone();
    let accept_conns = conns.clone();
    let accept_blocked = blocked.clone();
    // Пробер живёт, пока жив accept_task (он держит receiver).
    let mc_status = spawn_mc_prober(port);
    let accept_task = tokio::spawn(async move {
        while let Some(incoming) = ep.accept().await {
            let world_name = world_name.clone();
            let tx = tx.clone();
            let conns = accept_conns.clone();
            let blocked = accept_blocked.clone();
            let mc_status = mc_status.clone();
            // Хендшейк — в отдельной задаче: медленный или зависший гость
            // не мешает подключаться остальным.
            tokio::spawn(async move {
                let Ok(conn) = incoming.await else { return };
                let id = conn.remote_id().to_string();
                if blocked.lock().await.contains(&id) {
                    conn.close(0u32.into(), b"blocked");
                    return;
                }
                conns.lock().await.insert(id, conn.clone());
                handle_conn(conn, port, world_name, tx, conns, mc_status).await;
            });
        }
    });

    Ok(HostSession { invite_code, events: Some(rx), endpoint, accept_task, conns, blocked })
}

/// Один пробер на сессию: гости читают кэш из watch-канала вместо того, чтобы
/// на каждый Ping каждого гостя открывать своё TCP-соединение к Minecraft
/// (спам подключений на сервер + до 500 мс блокировки контрольного канала).
fn spawn_mc_prober(port: u16) -> watch::Receiver<bool> {
    let (tx, rx) = watch::channel(true);
    tokio::spawn(async move {
        loop {
            let online = tokio::time::timeout(
                Duration::from_millis(750),
                TcpStream::connect((Ipv4Addr::LOCALHOST, port)),
            )
            .await
            .map(|r| r.is_ok())
            .unwrap_or(false);
            if tx.send(online).is_err() {
                break; // сессия закрыта — читателей больше нет
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
    rx
}

async fn handle_conn(
    conn: Connection,
    port: u16,
    world_name: String,
    tx: mpsc::Sender<Event>,
    conns: ConnMap,
    mc_status: watch::Receiver<bool>,
) {
    let id_str = conn.remote_id().to_string();

    // Мост датаграмм для голосовых модов.
    tokio::spawn(datagram_bridge_host(conn.clone(), port));

    // Статус-тикер (rtt + путь) сам завершится по закрытию соединения.
    net::spawn_status_ticker(&conn, tx.clone());

    loop {
        match conn.accept_bi().await {
            Ok((send, recv)) => {
                let tx = tx.clone();
                let world = world_name.clone();
                let id = id_str.clone();
                let mc_status = mc_status.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_stream(send, recv, port, world, id, tx, mc_status).await
                    {
                        tracing::debug!("stream ended: {e:#}");
                    }
                });
            }
            Err(_) => {
                // Гость мог успеть переподключиться: запись в карте тогда уже
                // принадлежит новому соединению. Удалять её и слать GuestLeft
                // по смерти старого — значит «выкинуть» живого гостя из UI
                // и потерять его для kick/diagnostics.
                let still_ours = {
                    let mut map = conns.lock().await;
                    let ours = map.get(&id_str).is_some_and(|c| c.stable_id() == conn.stable_id());
                    if ours {
                        map.remove(&id_str);
                    }
                    ours
                };
                if still_ours {
                    let _ = tx.send(Event::GuestLeft { id: id_str.clone() }).await;
                }
                break;
            }
        }
    }
}

async fn handle_stream(
    mut send: iroh::endpoint::SendStream,
    mut recv: iroh::endpoint::RecvStream,
    port: u16,
    world_name: String,
    guest_id: String,
    tx: mpsc::Sender<Event>,
    mc_status: watch::Receiver<bool>,
) -> Result<()> {
    let mut tag = [0u8; 1];
    recv.read_exact(&mut tag).await?;
    match tag[0] {
        protocol::STREAM_TCP => {
            let tcp = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await?;
            // Nagle + delayed ACK добавляют до ~40-200 мс мелким игровым пакетам.
            let _ = tcp.set_nodelay(true);
            net::splice_tcp_quic(tcp, send, recv).await;
        }
        protocol::STREAM_CTRL => {
            // Контрольные сообщения не должны стоять в очереди за чанками.
            let _ = send.set_priority(1);
            let mut reader = BufReader::new(recv);
            while let Some(msg) = protocol::read_msg(&mut reader).await? {
                match msg {
                    protocol::CtrlMsg::Hello { name } => {
                        let _ = tx.send(Event::GuestJoined { id: guest_id.clone(), name }).await;
                        protocol::write_msg(
                            &mut send,
                            &protocol::CtrlMsg::Info { world_name: world_name.clone() },
                        )
                        .await?;
                    }
                    protocol::CtrlMsg::Ping { seq } => {
                        // Кэш общего пробера: гость покажет «хост офлайн» вместо
                        // молчаливо мёртвого мира, а Pong уходит мгновенно.
                        let mc_online = *mc_status.borrow();
                        protocol::write_msg(&mut send, &protocol::CtrlMsg::Pong { seq, mc_online })
                            .await?;
                    }
                    _ => {}
                }
            }
        }
        other => tracing::warn!("unknown stream tag {other}"),
    }
    Ok(())
}

/// Мост QUIC-датаграммы ↔ локальный UDP (голосовые моды).
/// На каждое соединение — свой сокет: ответы сервера уходят нужному гостю.
/// Один select-цикл: выходит вместе с соединением, сирот не оставляет.
async fn datagram_bridge_host(conn: Connection, port: u16) {
    let Ok(sock) = tokio::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await else {
        return;
    };
    if sock.connect((Ipv4Addr::LOCALHOST, port)).await.is_err() {
        return;
    }
    let mut buf = vec![0u8; 65535];
    loop {
        tokio::select! {
            dgram = conn.read_datagram() => match dgram {
                Ok(d) => { let _ = sock.send(&d).await; }
                Err(_) => break, // соединение закрыто
            },
            incoming = sock.recv(&mut buf) => match incoming {
                Ok(n) => match conn.send_datagram(bytes::Bytes::copy_from_slice(&buf[..n])) {
                    // Пакет больше MTU пути — дропаем кадр, мост живёт дальше.
                    Ok(()) | Err(SendDatagramError::TooLarge) => {}
                    Err(_) => break,
                },
                // ECONNREFUSED от ICMP «порт недоступен» (голосовой мод ещё не
                // запущен) — не повод навсегда убивать мост.
                Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
            },
        }
    }
}
