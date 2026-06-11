use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let world = mine_host_core::lan::discover_lan_world(Duration::from_secs(60)).await?;
    println!("Нашёл мир: «{}» на порту {}", world.motd, world.port);
    let mut session = mine_host_core::host::start(mine_host_core::host::HostOptions {
        port: world.port,
        world_name: world.motd,
        secret_key: None,
        use_relays: true,
    })
    .await?;
    println!("\n=== Код приглашения ===\n{}\n", session.invite_code);
    let mut events = session.take_events().unwrap();
    while let Some(ev) = events.recv().await {
        println!("{}", serde_json::to_string(&ev)?);
    }
    Ok(())
}
