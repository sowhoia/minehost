use std::time::Duration;

use mine_host_core::{guest, host};
use tokio::net::TcpListener;

#[tokio::test]
async fn guest_diagnostics_show_host() {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let host = host::start(host::HostOptions {
        port: l.local_addr().unwrap().port(),
        world_name: "Diag".into(),
        secret_key: None,
        use_relays: false,
    })
    .await
    .unwrap();
    let guest = guest::join(guest::GuestOptions {
        code: host.invite_code.clone(),
        player_name: "diag".into(),
        use_relays: false,
        preferred_port: None,
    })
    .await
    .unwrap();

    // ждём установления
    let mut diag = None;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(250)).await;
        if let Some(d) = guest.diagnostics().await {
            diag = Some(d);
            break;
        }
    }
    let d = diag.expect("диагностика недоступна");
    assert!(!d.peer_id.is_empty());
    assert!(!d.self_addrs.is_empty(), "должны знать свои адреса");

    guest.close().await;
    host.close().await;
}
