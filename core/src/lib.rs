pub mod events;
pub mod lan;

/// ALPN протокола туннеля. Версия в суффиксе — несовместимые изменения = новый суффикс.
pub const ALPN: &[u8] = b"mine-host/0";
