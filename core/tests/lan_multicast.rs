use std::time::Duration;
use mine_host_core::lan::{discover_lan_world, LanAnnounce, LanBeacon};

#[tokio::test]
async fn beacon_is_discovered() {
    let announce = LanAnnounce { motd: "Tunnel Test".into(), port: 53999 };
    let _beacon = LanBeacon::start(announce.clone()).expect("beacon start");
    let found = discover_lan_world(Duration::from_secs(10))
        .await
        .expect("discover");
    assert_eq!(found, announce);
}
