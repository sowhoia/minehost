use std::time::Duration;

use mine_host_core::{guest, host};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Эхо-сервер играет роль Minecraft-сервера.
async fn spawn_tcp_echo() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (mut s, _) = l.accept().await.unwrap();
            tokio::spawn(async move {
                let (mut r, mut w) = s.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    port
}

#[tokio::test]
async fn tcp_roundtrip_through_tunnel() {
    let mc_port = spawn_tcp_echo().await;

    let host = host::start(host::HostOptions {
        port: mc_port,
        world_name: "Test World".into(),
        secret_key: None,
        use_relays: false,
    })
    .await
    .unwrap();

    let guest = guest::join(guest::GuestOptions {
        code: host.invite_code.clone(),
        player_name: "tester".into(),
        use_relays: false,
        preferred_port: None, // эфемерный, чтобы не мешал реальный MC
    })
    .await
    .unwrap();

    // Подключаемся к локальному прокси гостя, как это сделал бы Minecraft.
    let mut s = TcpStream::connect(("127.0.0.1", guest.local_port)).await.unwrap();
    s.write_all(b"ping through the world").await.unwrap();
    let mut buf = [0u8; 22];
    tokio::time::timeout(Duration::from_secs(20), s.read_exact(&mut buf))
        .await
        .expect("echo timeout")
        .unwrap();
    assert_eq!(&buf, b"ping through the world");

    guest.close().await;
    host.close().await;
}
