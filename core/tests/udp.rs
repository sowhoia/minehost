use std::time::Duration;

use mine_host_core::{guest, host};
use tokio::net::{TcpListener, UdpSocket};

#[tokio::test]
async fn udp_roundtrip_through_tunnel() {
    // TCP-листенер задаёт номер порта; UDP-эхо садится на тот же номер.
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mc_port = l.local_addr().unwrap().port();
    let udp_echo = UdpSocket::bind(("127.0.0.1", mc_port)).await.unwrap();
    tokio::spawn(async move {
        let mut buf = [0u8; 1500];
        loop {
            let (n, src) = udp_echo.recv_from(&mut buf).await.unwrap();
            let _ = udp_echo.send_to(&buf[..n], src).await;
        }
    });

    let host = host::start(host::HostOptions {
        port: mc_port,
        world_name: "Voice World".into(),
        secret_key: None,
        use_relays: false,
    })
    .await
    .unwrap();

    let guest = guest::join(guest::GuestOptions {
        code: host.invite_code.clone(),
        player_name: "singer".into(),
        use_relays: false,
        preferred_port: None,
    })
    .await
    .unwrap();

    // Даём контрольному каналу установиться (мост стартует после Info).
    tokio::time::sleep(Duration::from_secs(2)).await;

    // «Голосовой клиент» шлёт UDP в локальный прокси гостя.
    let voice = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let mut buf = [0u8; 1500];
    let mut got = None;
    for _ in 0..10 {
        voice.send_to(b"opus-frame", ("127.0.0.1", guest.local_port)).await.unwrap();
        match tokio::time::timeout(Duration::from_secs(2), voice.recv_from(&mut buf)).await {
            Ok(Ok((n, _))) => {
                got = Some(buf[..n].to_vec());
                break;
            }
            _ => continue,
        }
    }
    assert_eq!(got.as_deref(), Some(b"opus-frame".as_slice()));

    guest.close().await;
    host.close().await;
}
