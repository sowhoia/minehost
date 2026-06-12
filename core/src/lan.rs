//! Обнаружение LAN-мира Minecraft и LAN-маяк для гостя.

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use anyhow::{Context, Result};
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;

pub const MC_MULTICAST_ADDR: Ipv4Addr = Ipv4Addr::new(224, 0, 2, 60);
pub const MC_MULTICAST_PORT: u16 = 4445;
pub const BEACON_INTERVAL: Duration = Duration::from_millis(1500);

/// Сокет для прослушивания анонсов. SO_REUSEADDR — Minecraft-клиент на этой же
/// машине тоже слушает 4445. Вступаем в группу и на default-, и на loopback-
/// интерфейсе, чтобы слышать и игру в локалке, и собственный маяк.
fn discovery_socket() -> io::Result<UdpSocket> {
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    s.set_reuse_address(true)?;
    #[cfg(unix)]
    s.set_reuse_port(true)?;
    s.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, MC_MULTICAST_PORT)).into())?;
    s.set_nonblocking(true)?;
    let sock = UdpSocket::from_std(s.into())?;
    sock.join_multicast_v4(MC_MULTICAST_ADDR, Ipv4Addr::UNSPECIFIED)?;
    // loopback-вступление может быть недоступно на некоторых системах — не фатально
    let _ = sock.join_multicast_v4(MC_MULTICAST_ADDR, Ipv4Addr::LOCALHOST);
    Ok(sock)
}

/// Сокет маяка: шлём мультикаст через loopback, чтобы Minecraft-клиент на этой
/// машине увидел источник 127.0.0.1 и подключился к локальному прокси.
/// Настройки мультикаста — best-effort: на некоторых системах (Linux без
/// MULTICAST-флага на lo) они падают, но отправка всё равно может работать.
fn beacon_socket() -> io::Result<UdpSocket> {
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    if let Err(e) = s.set_multicast_if_v4(&Ipv4Addr::LOCALHOST) {
        tracing::warn!("set_multicast_if_v4(loopback): {e}");
    }
    if let Err(e) = s.set_multicast_loop_v4(true) {
        tracing::warn!("set_multicast_loop_v4: {e}");
    }
    s.bind(&SocketAddr::from((Ipv4Addr::LOCALHOST, 0)).into())?;
    s.set_nonblocking(true)?;
    UdpSocket::from_std(s.into())
}

/// Ждёт ближайший LAN-анонс Minecraft (мир, открытый через Open to LAN).
pub async fn discover_lan_world(timeout: Duration) -> Result<LanAnnounce> {
    let sock = discovery_socket().context("bind multicast 4445")?;
    let mut buf = [0u8; 1024];
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let (len, _src) = tokio::time::timeout_at(deadline, sock.recv_from(&mut buf))
            .await
            .context("не вижу открытый в LAN мир — открой мир через Esc → Open to LAN")??;
        if let Some(a) = parse_announce(&buf[..len]) {
            return Ok(a);
        }
    }
}

/// Фоновый маяк: анонсирует туннелированный мир в LAN-списке гостя.
/// Останавливается при drop.
pub struct LanBeacon {
    task: tokio::task::JoinHandle<()>,
}

impl LanBeacon {
    pub fn start(announce: LanAnnounce) -> Result<Self> {
        let sock = beacon_socket().context("bind beacon socket")?;
        let payload = build_announce(&announce);
        let task = tokio::spawn(async move {
            let dst = SocketAddr::from((MC_MULTICAST_ADDR, MC_MULTICAST_PORT));
            loop {
                let _ = sock.send_to(&payload, dst).await;
                tokio::time::sleep(BEACON_INTERVAL).await;
            }
        });
        Ok(Self { task })
    }
}

impl Drop for LanBeacon {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanAnnounce {
    pub motd: String,
    pub port: u16,
}

fn between<'a>(s: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = s.find(open)? + open.len();
    let end = s[start..].find(close)? + start;
    Some(&s[start..end])
}

pub fn parse_announce(payload: &[u8]) -> Option<LanAnnounce> {
    let s = std::str::from_utf8(payload).ok()?;
    let motd = between(s, "[MOTD]", "[/MOTD]")?.to_string();
    let ad = between(s, "[AD]", "[/AD]")?;
    // современный формат — только порт; legacy — ip:port
    let port_str = ad.rsplit(':').next()?;
    let port: u16 = port_str.parse().ok()?;
    Some(LanAnnounce { motd, port })
}

pub fn build_announce(a: &LanAnnounce) -> Vec<u8> {
    format!("[MOTD]{}[/MOTD][AD]{}[/AD]", a.motd, a.port).into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modern_announce() {
        let a = parse_announce(b"[MOTD]My World[/MOTD][AD]25565[/AD]").unwrap();
        assert_eq!(a.motd, "My World");
        assert_eq!(a.port, 25565);
    }

    #[test]
    fn parses_legacy_ip_port_announce() {
        let a = parse_announce(b"[MOTD]Old[/MOTD][AD]192.168.0.5:54321[/AD]").unwrap();
        assert_eq!(a.port, 54321);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_announce(b"hello").is_none());
        assert!(parse_announce(b"[MOTD]x[/MOTD][AD]notaport[/AD]").is_none());
    }

    #[test]
    fn build_then_parse_roundtrip() {
        let src = LanAnnounce { motd: "Мир Никиты ⚡".into(), port: 61234 };
        let parsed = parse_announce(&build_announce(&src)).unwrap();
        assert_eq!(parsed.motd, src.motd);
        assert_eq!(parsed.port, src.port);
    }
}
