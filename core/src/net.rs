//! Адаптер над iroh::Endpoint. Вся специфика версии iroh — здесь.
use anyhow::Result;
use iroh::endpoint::{presets, Connection, PathId, TransportAddrUsage};
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
