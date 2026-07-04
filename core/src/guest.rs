//! Гостевая сессия: локальный прокси + LAN-маяк + переподключение.
use std::net::Ipv4Addr;
use std::time::Duration;

use anyhow::{Context, Result};
use iroh::endpoint::{Connection, SendDatagramError};
use iroh::{Endpoint, EndpointAddr};
use tokio::io::BufReader;
use tokio::net::TcpListener;
use tokio::sync::mpsc;

use crate::events::Event;
use crate::lan::{LanAnnounce, LanBeacon};
use crate::{invite, net, protocol, ALPN};

pub struct GuestOptions {
    pub code: String,
    pub player_name: String,
    pub use_relays: bool,
    /// None → пробуем 25565, иначе эфемерный.
    pub preferred_port: Option<u16>,
}

type SharedConn = std::sync::Arc<tokio::sync::Mutex<Option<Connection>>>;

pub struct GuestSession {
    pub local_port: u16,
    events: Option<mpsc::Receiver<Event>>,
    endpoint: Endpoint,
    main_task: tokio::task::JoinHandle<()>,
    current_conn: SharedConn,
}

impl GuestSession {
    pub fn take_events(&mut self) -> Option<mpsc::Receiver<Event>> {
        self.events.take()
    }

    /// Снимок состояния соединения с хостом (None — не подключены).
    pub async fn diagnostics(&self) -> Option<crate::net::Diagnostics> {
        let conn = self.current_conn.lock().await.clone()?;
        Some(crate::net::diagnostics(&self.endpoint, &conn).await)
    }

    pub async fn close(self) {
        self.main_task.abort();
        self.endpoint.close().await;
    }
}

pub fn backoff(attempt: u32) -> Duration {
    Duration::from_secs((1u64 << attempt.min(5)).min(30))
}

async fn bind_local(preferred: Option<u16>) -> Result<TcpListener> {
    if let Some(p) = preferred {
        return Ok(TcpListener::bind((Ipv4Addr::LOCALHOST, p)).await?);
    }
    match TcpListener::bind((Ipv4Addr::LOCALHOST, 25565)).await {
        Ok(l) => Ok(l),
        Err(_) => Ok(TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?),
    }
}

pub async fn join(opts: GuestOptions) -> Result<GuestSession> {
    let addr = invite::decode(&opts.code)?;
    let endpoint = net::make_endpoint(None, opts.use_relays).await?;
    let listener = bind_local(opts.preferred_port).await.context("bind local proxy")?;
    let local_port = listener.local_addr()?.port();
    let (tx, rx) = mpsc::channel::<Event>(64);
    let current_conn: SharedConn = Default::default();

    let ep = endpoint.clone();
    let name = opts.player_name.clone();
    let conn_slot = current_conn.clone();
    let main_task = tokio::spawn(run_guest(ep, addr, listener, local_port, name, tx, conn_slot));

    Ok(GuestSession { local_port, events: Some(rx), endpoint, main_task, current_conn })
}

/// Главный цикл: подключение → обслуживание → переподключение с backoff.
async fn run_guest(
    ep: Endpoint,
    addr: EndpointAddr,
    listener: TcpListener,
    local_port: u16,
    player_name: String,
    tx: mpsc::Sender<Event>,
    conn_slot: SharedConn,
) {
    // Голосовой UDP-сокет и адрес голосового клиента живут через
    // переподключения: перебинд на каждый reconnect гонялся со старой
    // задачей за порт, проигрывал и оставлял голос мёртвым до конца сессии.
    let last_src: LastVoiceSrc = Default::default();
    let mut voice_sock: Option<std::sync::Arc<tokio::net::UdpSocket>> = None;

    let mut attempt: u32 = 0;
    loop {
        if voice_sock.is_none() {
            match tokio::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, local_port)).await {
                Ok(s) => voice_sock = Some(std::sync::Arc::new(s)),
                Err(e) => {
                    tracing::warn!("UDP {local_port} занят — голосовые моды не будут работать: {e}")
                }
            }
        }
        // Таймаут — чтобы зависший хендшейк (смена сети, полумёртвый NAT)
        // не останавливал цикл переподключения навсегда.
        match tokio::time::timeout(Duration::from_secs(15), ep.connect(addr.clone(), ALPN)).await {
            Ok(Ok(conn)) => {
                attempt = 0;
                *conn_slot.lock().await = Some(conn.clone());
                let reason = serve_conn(
                    &ep,
                    &conn,
                    &listener,
                    local_port,
                    &player_name,
                    &tx,
                    voice_sock.clone(),
                    last_src.clone(),
                )
                .await
                .err()
                .map(|e| format!("{e:#}"))
                .unwrap_or_else(|| "connection closed".into());
                *conn_slot.lock().await = None;
                // Явно закрываем: живые клоны Connection (мост датаграмм) не
                // должны держать старое соединение параллельно с новым.
                conn.close(0u32.into(), b"reconnect");
                let _ = tx.send(Event::Disconnected { reason }).await;
            }
            Ok(Err(e)) => tracing::debug!("connect failed: {e:#}"),
            Err(_) => tracing::debug!("connect timed out"),
        }
        attempt += 1;
        let _ = tx.send(Event::Reconnecting { attempt }).await;
        // Первая попытка — быстрая: обрыв чаще всего мгновенно устраним
        // (хост перезапустился, сеть моргнула). Длинный backoff — при повторных.
        let delay = if attempt == 1 { Duration::from_millis(500) } else { backoff(attempt) };
        tokio::time::sleep(delay).await;
    }
}

/// Обслуживает одно соединение до его закрытия.
#[allow(clippy::too_many_arguments)]
async fn serve_conn(
    ep: &Endpoint,
    conn: &Connection,
    listener: &TcpListener,
    local_port: u16,
    player_name: &str,
    tx: &mpsc::Sender<Event>,
    voice_sock: Option<std::sync::Arc<tokio::net::UdpSocket>>,
    last_src: LastVoiceSrc,
) -> Result<()> {
    // Контрольный канал: hello → info, дальше ping-тикер.
    let (mut ctrl_send, ctrl_recv) = conn.open_bi().await?;
    // Пинги не должны стоять в очереди за чанками мира.
    let _ = ctrl_send.set_priority(1);
    ctrl_send.write_all(&[protocol::STREAM_CTRL]).await?;
    protocol::write_msg(
        &mut ctrl_send,
        &protocol::CtrlMsg::Hello { name: player_name.to_string() },
    )
    .await?;
    let mut ctrl_reader = BufReader::new(ctrl_recv);

    // Ждём Info с именем мира, поднимаем LAN-маяк.
    let world_name = match protocol::read_msg(&mut ctrl_reader).await? {
        Some(protocol::CtrlMsg::Info { world_name }) => world_name,
        other => anyhow::bail!("ожидал Info, получил {other:?}"),
    };
    // Маяк — UX-сахар: без него мир не появится в LAN-списке, но туннель
    // обязан жить (можно подключиться вручную по 127.0.0.1:порт).
    let _beacon =
        match LanBeacon::start(LanAnnounce { motd: format!("{world_name} ⚡"), port: local_port })
        {
            Ok(b) => Some(b),
            Err(e) => {
                tracing::warn!("LAN-маяк недоступен: {e:#}");
                None
            }
        };
    let _ = tx.send(Event::JoinedHost { local_port, world_name: world_name.clone() }).await;

    // Читаем ответы хоста: Pong несёт mc_online («хост офлайн» в UI).
    // Читать обязательно — иначе непрочитанные Pong'и со временем
    // упрутся во flow control контрольного потока.
    let pong_task = {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut mc_online = true;
            while let Ok(Some(msg)) = protocol::read_msg(&mut ctrl_reader).await {
                if let protocol::CtrlMsg::Pong { mc_online: ok, .. } = msg {
                    if ok != mc_online {
                        mc_online = ok;
                        let _ = tx.send(Event::HostMinecraftStatus { online: ok }).await;
                    }
                }
            }
        })
    };

    // Мост датаграмм для голосовых модов. Живёт не дольше соединения:
    // run_guest закрывает conn после serve_conn, и select-цикл моста выходит.
    if let Some(sock) = voice_sock {
        tokio::spawn(datagram_bridge_guest(conn.clone(), sock, last_src));
    }

    // Пинг-тикер поверх контрольного канала (поддерживает связь живой).
    let ping_conn = conn.clone();
    let mut ping_send = ctrl_send;
    let ping_task = tokio::spawn(async move {
        let mut seq = 0u64;
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            seq += 1;
            if protocol::write_msg(&mut ping_send, &protocol::CtrlMsg::Ping { seq }).await.is_err()
            {
                break;
            }
            if ping_conn.close_reason().is_some() {
                break;
            }
        }
    });

    // Статус-тикер.
    let status_task = {
        let conn = conn.clone();
        let ep = ep.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            let host_id = conn.remote_id();
            let id_str = host_id.to_string();
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                if conn.close_reason().is_some() {
                    break;
                }
                let path = net::path_kind(&ep, host_id).await;
                let _ = tx
                    .send(Event::PeerStatus {
                        id: id_str.clone(),
                        rtt_ms: net::conn_rtt_ms(&conn),
                        path,
                    })
                    .await;
            }
        })
    };

    // Принимаем TCP от Minecraft, пока соединение живо.
    let result = loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (mut tcp, _) = accepted?;
                // Nagle + delayed ACK добавляют до ~40-200 мс мелким игровым пакетам.
                let _ = tcp.set_nodelay(true);
                let conn = conn.clone();
                tokio::spawn(async move {
                    let Ok((mut send, recv)) = conn.open_bi().await else { return };
                    if send.write_all(&[protocol::STREAM_TCP]).await.is_err() { return; }
                    let mut quic = tokio::io::join(recv, send);
                    let _ = tokio::io::copy_bidirectional_with_sizes(
                        &mut tcp, &mut quic, crate::COPY_BUF, crate::COPY_BUF,
                    ).await;
                });
            }
            _ = conn.closed() => break Ok(()),
        }
    };

    ping_task.abort();
    status_task.abort();
    pong_task.abort();
    result
}

/// Адрес последнего локального отправителя UDP (голосового клиента) —
/// туда возвращаем ответы хоста. Живёт всю сессию, переживая reconnect'ы.
type LastVoiceSrc = std::sync::Arc<tokio::sync::Mutex<Option<std::net::SocketAddr>>>;

/// Мост локальный UDP ↔ QUIC-датаграммы. Один select-цикл вместо пары
/// задач: выходит, как только соединение закрыто, и не оставляет сирот,
/// которые держали бы сокет к следующему переподключению.
async fn datagram_bridge_guest(
    conn: Connection,
    sock: std::sync::Arc<tokio::net::UdpSocket>,
    last_src: LastVoiceSrc,
) {
    let mut buf = vec![0u8; 65535];
    loop {
        tokio::select! {
            dgram = conn.read_datagram() => match dgram {
                Ok(d) => {
                    if let Some(addr) = *last_src.lock().await {
                        let _ = sock.send_to(&d, addr).await;
                    }
                }
                Err(_) => break, // соединение закрыто
            },
            incoming = sock.recv_from(&mut buf) => match incoming {
                Ok((n, src)) => {
                    *last_src.lock().await = Some(src);
                    match conn.send_datagram(bytes::Bytes::copy_from_slice(&buf[..n])) {
                        // Пакет больше MTU пути — дропаем кадр, мост живёт дальше.
                        Ok(()) | Err(SendDatagramError::TooLarge) => {}
                        Err(_) => break,
                    }
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::backoff;
    use std::time::Duration;

    #[test]
    fn backoff_grows_and_caps() {
        assert_eq!(backoff(1), Duration::from_secs(2));
        assert_eq!(backoff(2), Duration::from_secs(4));
        assert_eq!(backoff(10), Duration::from_secs(30));
    }
}
