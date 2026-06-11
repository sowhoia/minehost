# ⛏ MineHost

Играй с друзьями в Minecraft по сети **без аренды сервера и без VPN**.
Прямое P2P-соединение (hole punching, как у Tailscale): задержка = чистый
пинг между вами. Геораспределённые релеи n0 — только как fallback на время
установления соединения и для строгих CGNAT.

## Как играть

**Хост:** открой мир → Esc → *Open to LAN* → в MineHost нажми **Хостить мир** →
отправь код друзьям.

**Друг:** вставь код → **Подключиться** → открой Minecraft → *Multiplayer* —
мир уже в LAN-списке. Никаких IP и настроек.

Работают и тяжёлые модпаки (400+ модов), и голосовые моды (Simple Voice Chat).
Моды у всех игроков должны совпадать — как и на обычном сервере.

## Сборка

```bash
cargo test                 # ядро
cd app && npm install
npm run tauri dev          # запуск в dev-режиме
npm run tauri build        # установщик под текущую ОС (Win/macOS/Linux)
```

CLI без GUI (для отладки): `cargo run -p mine-host-core --example host`
и `cargo run -p mine-host-core --example join <код>`.

## Архитектура

- `core/` — Rust: Iroh QUIC-туннель (TCP-потоки + UDP-датаграммы для голоса),
  LAN-обнаружение/маяк, инвайт-коды `mh:…`, события для UI.
- `app/` — Tauri 2 + SvelteKit GUI: экраны «Хостить»/«Подключиться»,
  трей, уведомления.

Спека: `docs/superpowers/specs/2026-06-10-mine-host-design.md`.
План: `docs/superpowers/plans/2026-06-12-mine-host.md`.
