use mine_host_core::invite;

#[tokio::test]
async fn invite_code_roundtrip() {
    // Реальный endpoint, чтобы получить валидный EndpointAddr (bind локальный).
    let ep = mine_host_core::net::make_endpoint(None, false).await.unwrap();
    let addr = ep.addr();
    let code = invite::encode(&addr);
    assert!(code.starts_with("mh:"), "код должен иметь префикс mh:, получили {code}");
    let decoded = invite::decode(&code).unwrap();
    assert_eq!(decoded.id, addr.id);
    // decode должен прощать пробелы и отсутствие префикса
    let decoded2 = invite::decode(&format!("  {}  ", code.trim_start_matches("mh:"))).unwrap();
    assert_eq!(decoded2.id, addr.id);
    ep.close().await;
}
