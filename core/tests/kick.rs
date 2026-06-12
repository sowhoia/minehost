use std::time::Duration;

use mine_host_core::events::Event;
use mine_host_core::{guest, host};
use tokio::net::TcpListener;

async fn next_event(rx: &mut tokio::sync::mpsc::Receiver<Event>, want: &str) -> Event {
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(20), rx.recv())
            .await
            .expect("event timeout")
            .expect("channel closed");
        if serde_json::to_value(&ev).unwrap()["type"] == want {
            return ev;
        }
    }
}

#[tokio::test]
async fn kicked_guest_cannot_rejoin() {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut host = host::start(host::HostOptions {
        port: l.local_addr().unwrap().port(),
        world_name: "Kick World".into(),
        secret_key: None,
        use_relays: false,
    })
    .await
    .unwrap();
    let mut host_events = host.take_events().unwrap();

    let mut guest = guest::join(guest::GuestOptions {
        code: host.invite_code.clone(),
        player_name: "Griefer".into(),
        use_relays: false,
        preferred_port: None,
    })
    .await
    .unwrap();
    let mut guest_events = guest.take_events().unwrap();

    let Event::GuestJoined { id, .. } = next_event(&mut host_events, "guest_joined").await else {
        unreachable!()
    };
    next_event(&mut guest_events, "joined_host").await;

    assert!(host.kick(&id).await, "kick должен найти соединение");
    next_event(&mut guest_events, "disconnected").await;

    // Гость пытается переподключаться, но блок-лист не пускает:
    // joined_host больше не должен появиться.
    let rejoined = tokio::time::timeout(Duration::from_secs(10), async {
        next_event(&mut guest_events, "joined_host").await
    })
    .await;
    assert!(rejoined.is_err(), "кикнутый гость переподключился: {rejoined:?}");

    guest.close().await;
    host.close().await;
}
