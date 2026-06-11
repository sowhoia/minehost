//! Гостевая сессия: локальный прокси + LAN-маяк + переподключение.
use std::net::Ipv4Addr;
use std::time::Duration;

use anyhow::{Context, Result};
use iroh::endpoint::Connection;
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

pub struct GuestSession {
    pub local_port: u16,
    events: Option<mpsc::Receiver<Event>>,
    endpoint: Endpoint,
    main_task: tokio::task::JoinHandle<()>,
}

impl GuestSession {
    pub fn take_events(&mut self) -> Option<mpsc::Receiver<Event>> {
        self.events.take()
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

    let ep = endpoint.clone();
    let name = opts.player_name.clone();
    let main_task = tokio::spawn(run_guest(ep, addr, listener, local_port, name, tx));

    Ok(GuestSession { local_port, events: Some(rx), endpoint, main_task })
}

/// Главный цикл: подключение → обслуживание → переподключение с backoff.
async fn run_guest(
    ep: Endpoint,
    addr: EndpointAddr,
    listener: TcpListener,
    local_port: u16,
    player_name: String,
    tx: mpsc::Sender<Event>,
) {
    let mut attempt: u32 = 0;
    loop {
        match ep.connect(addr.clone(), ALPN).await {
            Ok(conn) => {
                attempt = 0;
                let reason = serve_conn(&ep, &conn, &listener, local_port, &player_name, &tx)
                    .await
                    .err()
                    .map(|e| format!("{e:#}"))
                    .unwrap_or_else(|| "connection closed".into());
                let _ = tx.send(Event::Disconnected { reason }).await;
            }
            Err(e) => {
                tracing::debug!("connect failed: {e:#}");
            }
        }
        attempt += 1;
        let _ = tx.send(Event::Reconnecting { attempt }).await;
        tokio::time::sleep(backoff(attempt)).await;
    }
}

/// Обслуживает одно соединение до его закрытия.
async fn serve_conn(
    ep: &Endpoint,
    conn: &Connection,
    listener: &TcpListener,
    local_port: u16,
    player_name: &str,
    tx: &mpsc::Sender<Event>,
) -> Result<()> {
    // Контрольный канал: hello → info, дальше ping-тикер.
    let (mut ctrl_send, ctrl_recv) = conn.open_bi().await?;
    ctrl_send.write_all(&[protocol::STREAM_CTRL]).await?;
    protocol::write_msg(&mut ctrl_send, &protocol::CtrlMsg::Hello { name: player_name.to_string() })
        .await?;
    let mut ctrl_reader = BufReader::new(ctrl_recv);

    // Ждём Info с именем мира, поднимаем LAN-маяк.
    let world_name = match protocol::read_msg(&mut ctrl_reader).await? {
        Some(protocol::CtrlMsg::Info { world_name }) => world_name,
        other => anyhow::bail!("ожидал Info, получил {other:?}"),
    };
    let _beacon = LanBeacon::start(LanAnnounce {
        motd: format!("{world_name} ⚡"),
        port: local_port,
    })?;
    let _ = tx
        .send(Event::JoinedHost { local_port, world_name: world_name.clone() })
        .await;

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

    // Мост датаграмм для голосовых модов (Task 9 наполнит функцию).
    tokio::spawn(datagram_bridge_guest(conn.clone(), local_port));

    // Пинг-тикер поверх контрольного канала (поддерживает связь живой).
    let ping_conn = conn.clone();
    let mut ping_send = ctrl_send;
    let ping_task = tokio::spawn(async move {
        let mut seq = 0u64;
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            seq += 1;
            if protocol::write_msg(&mut ping_send, &protocol::CtrlMsg::Ping { seq }).await.is_err() {
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
                let conn = conn.clone();
                tokio::spawn(async move {
                    let Ok((mut send, recv)) = conn.open_bi().await else { return };
                    if send.write_all(&[protocol::STREAM_TCP]).await.is_err() { return; }
                    let mut quic = tokio::io::join(recv, send);
                    let _ = tokio::io::copy_bidirectional(&mut tcp, &mut quic).await;
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

/// Заглушка до Task 9 (UDP для голосовых модов).
async fn datagram_bridge_guest(_conn: Connection, _local_port: u16) {}

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
