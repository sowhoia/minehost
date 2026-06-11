//! Инвайт-коды: EndpointTicket с префиксом mh:.
use anyhow::{Context, Result};
use iroh::EndpointAddr;
use iroh_tickets::endpoint::EndpointTicket;

const PREFIX: &str = "mh:";

pub fn encode(addr: &EndpointAddr) -> String {
    format!("{PREFIX}{}", EndpointTicket::new(addr.clone()))
}

pub fn decode(code: &str) -> Result<EndpointAddr> {
    let raw = code.trim().trim_start_matches(PREFIX);
    let ticket: EndpointTicket = raw.parse().context("код приглашения недействителен")?;
    Ok(ticket.endpoint_addr().clone())
}
