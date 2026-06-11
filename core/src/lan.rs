//! Обнаружение LAN-мира Minecraft и LAN-маяк для гостя.

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
