//! Инвайт-коды: EndpointTicket с префиксом mh:.
use anyhow::{Context, Result};
use iroh::EndpointAddr;
use iroh_tickets::endpoint::EndpointTicket;

const PREFIX: &str = "mh:";
const LINK_PREFIX: &str = "minehost://join/";

pub fn encode(addr: &EndpointAddr) -> String {
    format!("{PREFIX}{}", EndpointTicket::new(addr.clone()))
}

/// Кликабельная ссылка для мессенджеров (deep link).
pub fn encode_link(addr: &EndpointAddr) -> String {
    format!("{LINK_PREFIX}{}", encode(addr))
}

pub fn decode(code: &str) -> Result<EndpointAddr> {
    let raw = code.trim();
    let raw = raw.strip_prefix(LINK_PREFIX).unwrap_or(raw);
    let raw = raw.strip_prefix(PREFIX).unwrap_or(raw);
    let ticket: EndpointTicket = raw.parse().context("код приглашения недействителен")?;
    Ok(ticket.endpoint_addr().clone())
}
