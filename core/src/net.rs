//! Адаптер над iroh::Endpoint. Вся специфика версии iroh — здесь.
use anyhow::Result;
use iroh::endpoint::{
    presets, Connection, PathId, QuicTransportConfig, TransportAddrUsage, VarInt,
};
use iroh::{Endpoint, SecretKey};

use crate::ALPN;

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
        .build();
    builder = builder.transport_config(transport);
    if let Some(key) = secret_key {
        builder = builder.secret_key(key);
    }
    Ok(builder.bind().await?)
}

/// Текущая оценка RTT соединения в миллисекундах (0 — пока неизвестно).
pub fn conn_rtt_ms(conn: &Connection) -> u32 {
    conn.rtt(PathId::ZERO).map(|d| d.as_millis() as u32).unwrap_or(0)
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
