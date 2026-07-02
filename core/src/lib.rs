pub mod events;
pub mod guest;
pub mod host;
pub mod invite;
pub mod lan;
pub mod net;
pub mod protocol;

/// ALPN протокола туннеля. Версия в суффиксе — несовместимые изменения = новый суффикс.
pub const ALPN: &[u8] = b"mine-host/0";

/// Буфер копирования TCP↔QUIC. Больше — меньше сисколлов на всплесках чанков;
/// задержки не добавляет (copy не ждёт заполнения буфера).
pub(crate) const COPY_BUF: usize = 64 * 1024;
