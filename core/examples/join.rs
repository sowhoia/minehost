#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let code = std::env::args().nth(1).expect("usage: join <invite-code>");
    let mut session = mine_host_core::guest::join(mine_host_core::guest::GuestOptions {
        code,
        player_name: whoami(),
        use_relays: true,
        preferred_port: None,
    })
    .await?;
    println!("Локальный прокси: 127.0.0.1:{}", session.local_port);
    println!("Открой Minecraft → Multiplayer: мир появится в LAN-списке.");
    let mut events = session.take_events().unwrap();
    while let Some(ev) = events.recv().await {
        println!("{}", serde_json::to_string(&ev)?);
    }
    Ok(())
}

fn whoami() -> String {
    std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_else(|_| "guest".into())
}
