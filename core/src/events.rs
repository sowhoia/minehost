use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PathKind {
    Direct,
    Relay,
    Mixed,
    Unknown,
}

/// События ядра для UI. Сериализуются в JSON для Tauri-фронтенда.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// Хост готов, можно слать код друзьям.
    HostReady { invite_code: String },
    /// Гость подключился к хосту (видно у хоста).
    GuestJoined { id: String, name: String },
    /// Гость отключился (видно у хоста).
    GuestLeft { id: String },
    /// Гость успешно подключён (видно у гостя).
    JoinedHost { local_port: u16, world_name: String },
    /// Периодический статус соединения с пиром.
    PeerStatus { id: String, rtt_ms: u32, path: PathKind },
    /// Соединение потеряно.
    Disconnected { reason: String },
    /// Идёт попытка переподключения.
    Reconnecting { attempt: u32 },
    /// Minecraft хоста упал/закрылся (туннель жив, ждём возвращения).
    HostMinecraftStatus { online: bool },
}
