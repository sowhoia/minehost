//! Хост-сессия: принимает гостей и проксирует их в локальный Minecraft.
use std::net::Ipv4Addr;
use std::time::Duration;

use anyhow::Result;
use iroh::endpoint::Connection;
use iroh::{Endpoint, SecretKey};
use tokio::io::BufReader;
use tokio::net::TcpStream;
use tokio::sync::mpsc;

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

pub struct HostSession {
    pub invite_code: String,
    events: Option<mpsc::Receiver<Event>>,
    endpoint: Endpoint,
    accept_task: tokio::task::JoinHandle<()>,
}

impl HostSession {
    /// Receiver событий (забирается один раз — для UI).
    pub fn take_events(&mut self) -> Option<mpsc::Receiver<Event>> {
        self.events.take()
    }

    pub async fn close(self) {
        self.accept_task.abort();
        self.endpoint.close().await;
    }
}

pub async fn start(opts: HostOptions) -> Result<HostSession> {
    let endpoint = net::make_endpoint(opts.secret_key.clone(), opts.use_relays).await?;
    if opts.use_relays {
        endpoint.online().await; // дождаться релея, чтобы тикет содержал relay url
    }
    let invite_code = invite::encode(&endpoint.addr());
    let (tx, rx) = mpsc::channel::<Event>(64);
    let _ = tx.try_send(Event::HostReady { invite_code: invite_code.clone() });

    let ep = endpoint.clone();
    let port = opts.port;
    let world_name = opts.world_name.clone();
    let accept_task = tokio::spawn(async move {
        while let Some(incoming) = ep.accept().await {
            let Ok(conn) = incoming.await else { continue };
            tokio::spawn(handle_conn(ep.clone(), conn, port, world_name.clone(), tx.clone()));
        }
    });

    Ok(HostSession { invite_code, events: Some(rx), endpoint, accept_task })
}

async fn handle_conn(
    ep: Endpoint,
    conn: Connection,
    port: u16,
    world_name: String,
    tx: mpsc::Sender<Event>,
) {
    let remote_id = conn.remote_id();
    let id_str = remote_id.to_string();

    // Мост датаграмм для голосовых модов (Task 9 наполнит функцию).
    tokio::spawn(datagram_bridge_host(conn.clone(), port));

    // Статус-тикер: rtt + путь (direct/relay).
    {
        let conn = conn.clone();
        let ep = ep.clone();
        let tx = tx.clone();
        let id_str = id_str.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                if conn.close_reason().is_some() {
                    break;
                }
                let path = net::path_kind(&ep, remote_id).await;
                let _ = tx
                    .send(Event::PeerStatus {
                        id: id_str.clone(),
                        rtt_ms: net::conn_rtt_ms(&conn),
                        path,
                    })
                    .await;
            }
        });
    }

    loop {
        match conn.accept_bi().await {
            Ok((send, recv)) => {
                let tx = tx.clone();
                let world = world_name.clone();
                let id = id_str.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_stream(send, recv, port, world, id, tx).await {
                        tracing::debug!("stream ended: {e:#}");
                    }
                });
            }
            Err(_) => {
                let _ = tx.send(Event::GuestLeft { id: id_str.clone() }).await;
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
) -> Result<()> {
    let mut tag = [0u8; 1];
    recv.read_exact(&mut tag).await?;
    match tag[0] {
        protocol::STREAM_TCP => {
            let mut tcp = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await?;
            let mut quic = tokio::io::join(recv, send);
            let _ = tokio::io::copy_bidirectional(&mut tcp, &mut quic).await;
        }
        protocol::STREAM_CTRL => {
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
                        // Заодно проверяем, жив ли Minecraft хоста: гость покажет
                        // «хост офлайн» вместо молчаливо мёртвого мира.
                        let mc_online = tokio::time::timeout(
                            Duration::from_millis(500),
                            TcpStream::connect((Ipv4Addr::LOCALHOST, port)),
                        )
                        .await
                        .map(|r| r.is_ok())
                        .unwrap_or(false);
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

/// Заглушка до Task 9 (UDP для голосовых модов).
async fn datagram_bridge_host(_conn: Connection, _port: u16) {}
