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
        let tag = serde_json::to_value(&ev).unwrap()["type"].as_str().unwrap().to_string();
        if tag == want {
            return ev;
        }
    }
}

#[tokio::test]
async fn join_leave_events_flow() {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mc_port = l.local_addr().unwrap().port();

    let mut host = host::start(host::HostOptions {
        port: mc_port,
        world_name: "Event World".into(),
        secret_key: None,
        use_relays: false,
    })
    .await
    .unwrap();
    let mut host_events = host.take_events().unwrap();

    let mut guest = guest::join(guest::GuestOptions {
        code: host.invite_code.clone(),
        player_name: "Alex".into(),
        use_relays: false,
        preferred_port: None,
    })
    .await
    .unwrap();
    let mut guest_events = guest.take_events().unwrap();

    // Гость представился — хост видит его имя.
    let ev = next_event(&mut host_events, "guest_joined").await;
    match ev {
        Event::GuestJoined { name, .. } => assert_eq!(name, "Alex"),
        other => panic!("unexpected {other:?}"),
    }
    // Гость получил имя мира.
    let ev = next_event(&mut guest_events, "joined_host").await;
    match ev {
        Event::JoinedHost { world_name, .. } => assert_eq!(world_name, "Event World"),
        other => panic!("unexpected {other:?}"),
    }

    // Хост умирает — гость замечает и начинает переподключаться.
    host.close().await;
    next_event(&mut guest_events, "disconnected").await;
    next_event(&mut guest_events, "reconnecting").await;

    guest.close().await;
}
