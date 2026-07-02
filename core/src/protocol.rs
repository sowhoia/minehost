//! Прикладной протокол туннеля поверх QUIC-потоков.
use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

/// Первый байт каждого bidi-потока.
pub const STREAM_CTRL: u8 = 0x00;
pub const STREAM_TCP: u8 = 0x01;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum CtrlMsg {
    /// Гость представляется (первое сообщение гостя).
    Hello {
        name: String,
    },
    /// Хост сообщает имя мира (ответ на Hello).
    Info {
        world_name: String,
    },
    Ping {
        seq: u64,
    },
    /// mc_online=false — Minecraft хоста не отвечает на локальном порту.
    Pong {
        seq: u64,
        mc_online: bool,
    },
}

/// JSON-строка с \n — простой самосинхронизирующийся фрейминг.
pub async fn write_msg<W: AsyncWrite + Unpin>(w: &mut W, msg: &CtrlMsg) -> Result<()> {
    let mut line = serde_json::to_vec(msg)?;
    line.push(b'\n');
    w.write_all(&line).await?;
    w.flush().await?;
    Ok(())
}

/// None — поток закрыт пиром.
pub async fn read_msg<R: AsyncBufRead + Unpin>(r: &mut R) -> Result<Option<CtrlMsg>> {
    let mut line = String::new();
    if r.read_line(&mut line).await? == 0 {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str(&line)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn msg_roundtrip_over_duplex() {
        let (client, mut server) = tokio::io::duplex(1024);
        write_msg(&mut server, &CtrlMsg::Hello { name: "Стив".into() }).await.unwrap();
        write_msg(&mut server, &CtrlMsg::Ping { seq: 7 }).await.unwrap();
        // drop закрывает сторону сервера → у клиента после сообщений будет EOF
        drop(server);
        let mut reader = tokio::io::BufReader::new(client);
        assert_eq!(
            read_msg(&mut reader).await.unwrap(),
            Some(CtrlMsg::Hello { name: "Стив".into() })
        );
        assert_eq!(read_msg(&mut reader).await.unwrap(), Some(CtrlMsg::Ping { seq: 7 }));
        assert_eq!(read_msg(&mut reader).await.unwrap(), None);
    }
}
