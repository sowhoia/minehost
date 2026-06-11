//! Адаптер над iroh::Endpoint. Вся специфика версии iroh — здесь.
use anyhow::Result;
use iroh::{endpoint::presets, Endpoint, SecretKey};

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

/// Направление пути к пиру: напрямую или через релей.
pub async fn path_kind(ep: &Endpoint, id: iroh::EndpointId) -> crate::events::PathKind {
    use crate::events::PathKind;
    // remote_info даёт сведения о транспортных адресах недавнего пира.
    // Если структура полей отличается в текущей iroh — поправить только здесь;
    // безопасный fallback — Unknown (UI покажет «?»).
    match ep.remote_info(id).await {
        Some(info) => {
            let dbg = format!("{info:?}").to_lowercase();
            let direct = dbg.contains("direct") || dbg.contains("udp");
            let relay = dbg.contains("relay");
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
