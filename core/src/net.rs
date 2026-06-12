//! Адаптер над iroh::Endpoint. Вся специфика версии iroh — здесь.
use anyhow::Result;
use iroh::endpoint::{presets, Connection, PathId, QuicTransportConfig, TransportAddrUsage, VarInt};
use iroh::{Endpoint, SecretKey};

use crate::ALPN;

/// relays=false — герметичный режим для тестов (без сети n0:
/// presets::Minimal ставит только обязательный crypto provider).
pub async fn make_endpoint(secret_key: Option<SecretKey>, relays: bool) -> Result<Endpoint> {
    let mut builder = if relays {
        Endpoint::builder(presets::N0)
    } else {
        Endpoint::builder(presets::Minimal)
    };
    builder = builder.alpns(vec![ALPN.to_vec()]);
    // Дефолтные окна noq рассчитаны на 100 Мбит/100 мс. Логин в тяжёлый модпак —
    // всплеск в десятки МБ по одному потоку; поднимаем окна, чтобы не душить его.
    let transport = QuicTransportConfig::builder()
        .stream_receive_window(VarInt::from_u32(16 * 1024 * 1024))
        .receive_window(VarInt::from_u32(64 * 1024 * 1024))
        .send_window(64 * 1024 * 1024)
        .build();
    builder = builder.transport_config(transport);
    if let Some(key) = secret_key {
        builder = builder.secret_key(key);
    }
    Ok(builder.bind().await?)
}

/// Текущая оценка RTT соединения в миллисекундах (0 — пока неизвестно).
pub fn conn_rtt_ms(conn: &Connection) -> u32 {
    conn.rtt(PathId::ZERO)
        .map(|d| d.as_millis() as u32)
        .unwrap_or(0)
}

/// Снимок состояния соединения для экрана диагностики.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Diagnostics {
    pub peer_id: String,
    pub rtt_ms: u32,
    pub path: crate::events::PathKind,
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
    let self_addrs = ep.addr().addrs.iter().map(|a| format!("{a:?}")).collect();
    Diagnostics {
        peer_id: id.to_string(),
        rtt_ms: conn_rtt_ms(conn),
        path: path_kind(ep, id).await,
        addrs,
        self_addrs,
    }
}

/// Направление пути к пиру: напрямую или через релей.
/// Смотрим активные транспортные адреса пира из remote_info.
pub async fn path_kind(ep: &Endpoint, id: iroh::EndpointId) -> crate::events::PathKind {
    use crate::events::PathKind;
    match ep.remote_info(id).await {
        Some(info) => {
            let (mut direct, mut relay) = (false, false);
            for a in info.addrs() {
                if matches!(a.usage(), TransportAddrUsage::Active) {
                    if a.addr().is_relay() {
                        relay = true;
                    } else {
                        direct = true;
                    }
                }
            }
            match (direct, relay) {
                (true, false) => PathKind::Direct,
                (false, true) => PathKind::Relay,
                (true, true) => PathKind::Mixed,
                _ => PathKind::Unknown,
            }
        }
        None => PathKind::Unknown,
    }
}
