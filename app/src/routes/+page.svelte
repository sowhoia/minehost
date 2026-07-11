<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { getVersion } from "@tauri-apps/api/app";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { onOpenUrl } from "@tauri-apps/plugin-deep-link";
  import { open } from "@tauri-apps/plugin-dialog";
  import { relaunch } from "@tauri-apps/plugin-process";
  import { check } from "@tauri-apps/plugin-updater";

  type Peer = { name: string; rtt_ms: number; path: string };
  type MhEvent =
    | { type: "host_ready"; invite_code: string }
    | { type: "guest_joined"; id: string; name: string }
    | { type: "guest_left"; id: string }
    | { type: "joined_host"; local_port: number; world_name: string }
    | { type: "peer_status"; id: string; rtt_ms: number; path: string }
    | { type: "disconnected"; reason: string }
    | { type: "reconnecting"; attempt: number }
    | { type: "host_minecraft_status"; online: boolean };

  /// Настройки, которые обидно вводить заново при каждом запуске.
  const stored = (k: string, fallback: string) =>
    typeof localStorage === "undefined" ? fallback : (localStorage.getItem(k) ?? fallback);

  let mode = $state<"home" | "host" | "guest">("home");
  /// Связь с хостом (в режиме гостя): жива или переустанавливается.
  let linkState = $state<"up" | "reconnecting">("up");
  let busy = $state(false);
  let error = $state("");
  let inviteCode = $state("");
  let joinCode = $state("");
  let playerName = $state(stored("mh-name", "Player"));
  let statusLine = $state("");
  let worldName = $state("");
  /// Локальный порт туннеля у гостя — запасной вход через Direct Connect.
  let localPort = $state(0);
  let copied = $state(false);
  let peers = $state<Record<string, Peer>>({});
  /// История rtt по пирам (тик статуса — раз в 2 с) для спарклайна.
  let rttHist = $state<Record<string, number[]>>({});

  type Tab = "host" | "join" | "players" | "diag";
  let tab = $state<Tab>("host");
  const storedSrc = stored("mh-src", "lan");
  let hostSource = $state<"lan" | "port" | "jar">(
    storedSrc === "port" || storedSrc === "jar" ? storedSrc : "lan",
  );
  let elapsed = $state(0);
  let appVersion = $state("");

  $effect(() => {
    getVersion().then((v) => (appVersion = v)).catch(() => {});
  });

  $effect(() => {
    if (mode === "home") {
      elapsed = 0;
      return;
    }
    const t = setInterval(() => (elapsed += 1), 1000);
    return () => clearInterval(t);
  });

  const phase = $derived(
    busy || (mode === "guest" && linkState === "reconnecting")
      ? "connecting"
      : mode === "home"
        ? "idle"
        : "online",
  );
  const peerCount = $derived(Object.keys(peers).length);
  const statusColor = $derived(
    phase === "online" ? "#5fbf4f" : phase === "connecting" ? "#f3c63a" : "#d6504a",
  );
  const statusGlow = $derived(
    phase === "online"
      ? "rgba(95,191,79,.6)"
      : phase === "connecting"
        ? "rgba(243,198,58,.5)"
        : "rgba(214,80,74,.5)",
  );
  const statusShort = $derived(
    phase === "online" ? "ОНЛАЙН" : phase === "connecting" ? ". . ." : "ОФФЛАЙН",
  );
  const statusLabel = $derived(
    phase === "online"
      ? mode === "host"
        ? "Онлайн — хост запущен"
        : "Онлайн — ты в мире друга"
      : phase === "connecting"
        ? mode === "guest" && linkState === "reconnecting"
          ? "Переподключение к хосту…"
          : "Подключение к P2P-сети…"
        : "Оффлайн",
  );
  const tunnelTxt = $derived(
    phase === "online" ? "установлен" : phase === "connecting" ? "установка…" : "—",
  );
  const uptime = $derived.by(() => {
    if (mode === "home") return "—";
    const h = Math.floor(elapsed / 3600);
    const mm = String(Math.floor((elapsed % 3600) / 60)).padStart(2, "0");
    const ss = String(elapsed % 60).padStart(2, "0");
    return h > 0 ? `${h}:${mm}:${ss}` : `${mm}:${ss}`;
  });
  const footerStatus = $derived(
    phase === "connecting"
      ? "подключение…"
      : mode === "host"
        ? "хост запущен"
        : mode === "guest"
          ? "подключён к миру"
          : "хост остановлен",
  );

  const SKINS = [
    { skin: "#a06a44", hair: "#3a2a1a", eye: "#6b4a8f" },
    { skin: "#f0c8a0", hair: "#d2792f", eye: "#4a8f6b" },
    { skin: "#54a04a", hair: "#3a6e33", eye: "#143010" },
    { skin: "#2c2436", hair: "#7d3fb0", eye: "#c060ff" },
    { skin: "#c98e5e", hair: "#1a1a1a", eye: "#3fd0d0" },
    { skin: "#d99a7a", hair: "#9a4f2a", eye: "#5a3a20" },
  ];
  const BIOMES = [
    { sky: "#3b6ea5", skyTop: "#6fa8d8", grass: "#5fa84e", grassHi: "#7ec85f", dirt: "#7a5230", sun: "#fff2b0" },
    { sky: "#2a1a3a", skyTop: "#4a2f63", grass: "#7d3fb0", grassHi: "#a05fd0", dirt: "#3a2150", sun: "#d6a0ff" },
    { sky: "#5a1e14", skyTop: "#8a2e1c", grass: "#c0431f", grassHi: "#e0612f", dirt: "#5a1e14", sun: "#ffb070" },
    { sky: "#7fb0d8", skyTop: "#b8d8ec", grass: "#e8f0f5", grassHi: "#ffffff", dirt: "#8a8f99", sun: "#fffae0" },
  ];
  function hashStr(s: string) {
    let h = 0;
    for (let i = 0; i < s.length; i++) h = (h * 31 + s.charCodeAt(i)) | 0;
    return Math.abs(h);
  }
  const skinFor = (name: string) => SKINS[hashStr(name) % SKINS.length];
  const biomeFor = (code: string) => BIOMES[hashStr(code) % BIOMES.length];

  function pingView(rtt: number) {
    if (!rtt) return { level: 0, col: "#8a8a8a", txt: "…" };
    const level = rtt < 55 ? 4 : rtt < 110 ? 3 : rtt < 185 ? 2 : 1;
    const col = rtt < 110 ? "#5fbf4f" : rtt < 185 ? "#f3c63a" : "#d6504a";
    return { level, col, txt: `${rtt} ms` };
  }
  const pathLabel = (p: string) =>
    p === "direct" ? "⚡ напрямую" : p === "relay" ? "🌐 через релей" : p === "mixed" ? "⚡🌐 смешанный" : "путь ищется…";

  function ago(at: number) {
    const s = Math.floor((Date.now() - at) / 1000);
    if (s < 90) return "только что";
    if (s < 3600) return `${Math.floor(s / 60)} мин назад`;
    if (s < 86400) return `${Math.floor(s / 3600)} ч назад`;
    const d = Math.floor(s / 86400);
    return d === 1 ? "вчера" : `${d} дн назад`;
  }

  function handleEvent(ev: MhEvent) {
    switch (ev.type) {
      case "guest_joined":
        peers[ev.id] = { name: ev.name, rtt_ms: 0, path: "unknown" };
        break;
      case "guest_left":
        delete peers[ev.id];
        delete rttHist[ev.id];
        break;
      case "peer_status":
        // У хоста только обновляем существующих: запоздавший тикер после
        // guest_left не должен воскрешать строку-призрак.
        if (mode === "guest") {
          peers[ev.id] = { name: peers[ev.id]?.name ?? "хост", rtt_ms: ev.rtt_ms, path: ev.path };
        } else if (peers[ev.id]) {
          peers[ev.id] = { ...peers[ev.id], rtt_ms: ev.rtt_ms, path: ev.path };
        } else {
          break;
        }
        rttHist[ev.id] = [...(rttHist[ev.id] ?? []).slice(-59), ev.rtt_ms];
        break;
      case "joined_host":
        worldName = ev.world_name;
        localPort = ev.local_port;
        linkState = "up";
        statusLine = `Подключено! Открой Minecraft → Multiplayer: «${ev.world_name}» в LAN-списке`;
        saveRecent(joinCode.trim(), ev.world_name);
        break;
      case "disconnected":
        linkState = "reconnecting";
        statusLine = `Связь потеряна: ${ev.reason}`;
        break;
      case "reconnecting":
        linkState = "reconnecting";
        statusLine = `Переподключение (попытка ${ev.attempt})…`;
        break;
      case "host_minecraft_status":
        statusLine = ev.online
          ? `Хост снова в игре! «${worldName}» доступен в LAN-списке`
          : "Хост офлайн (мир закрыт?) — туннель ждёт его возвращения";
        break;
    }
  }

  $effect(() => {
    const un: Promise<UnlistenFn> = listen<MhEvent>("mh-event", (e) => handleEvent(e.payload));
    return () => {
      un.then((f) => f());
    };
  });

  function tryJoinFromLink(s: string) {
    if (s.startsWith("minehost://") && mode === "home" && !busy) {
      joinCode = s;
      tab = "join";
      joinHost();
    }
  }
  $effect(() => {
    onOpenUrl((urls) => urls.forEach(tryJoinFromLink));
    const un = listen<string[]>("mh-deeplink", (e) => e.payload.forEach(tryJoinFromLink));
    return () => {
      un.then((f) => f());
    };
  });

  const inviteLink = $derived(inviteCode ? `minehost://join/${inviteCode}` : "");

  let manualPort = $state(stored("mh-port", ""));
  const portValid = (s: string) => /^\d+$/.test(s) && +s > 0 && +s < 65536;

  /// Порт, с которым хост стартовал в этот раз: ротация кода перезапускает
  /// хост с тем же портом, а не уходит в LAN-поиск (который для port/jar
  /// просто упадёт по таймауту).
  let lastHostPort: number | null = null;

  async function startHost(port?: number): Promise<boolean> {
    busy = true;
    error = "";
    try {
      inviteCode = await invoke<string>("start_host", { manualPort: port ?? null });
      lastHostPort = port ?? null;
      mode = "host";
      return true;
    } catch (e) {
      error = String(e);
      return false;
    } finally {
      busy = false;
    }
  }

  let jarPath = $state(stored("mh-jar", ""));
  let ramMb = $state(stored("mh-ram", "4096"));
  let eula = $state(false);

  // Персистим настройки: эффект перезапускается при изменении любой из них.
  $effect(() => {
    localStorage.setItem("mh-name", playerName);
    localStorage.setItem("mh-src", hostSource);
    localStorage.setItem("mh-port", manualPort);
    localStorage.setItem("mh-jar", jarPath);
    localStorage.setItem("mh-ram", ramMb);
  });
  /// Скользящее окно лога сервера: видно контекст, а не одну последнюю строку.
  let serverLines = $state<string[]>([]);
  let serverRunning = $state(false);
  let logBox = $state<HTMLPreElement | null>(null);
  $effect(() => {
    const un = listen<string>("mh-server-log", (e) => {
      serverLines = [...serverLines.slice(-249), e.payload];
    });
    return () => {
      un.then((f) => f());
    };
  });
  // автопрокрутка вниз при новых строках
  $effect(() => {
    void serverLines.length;
    if (logBox) logBox.scrollTop = logBox.scrollHeight;
  });
  async function pickJar() {
    const p = await open({ filters: [{ name: "Server JAR", extensions: ["jar"] }] });
    if (typeof p === "string") jarPath = p;
  }
  async function startServerAndHost() {
    busy = true;
    error = "";
    serverLines = ["Запускаю сервер…"];
    try {
      const port = await invoke<number>("server_start", {
        jarPath,
        ramMb: Number(ramMb),
        acceptEula: eula,
      });
      serverRunning = true;
      // startHost ловит свои ошибки сам — но java-сироту после неудачи
      // оставлять нельзя: иначе повторный запуск упрётся в «уже запущен».
      if (!(await startHost(port))) {
        await invoke("server_stop");
        serverRunning = false;
      }
    } catch (e) {
      error = String(e);
      await invoke("server_stop");
      serverRunning = false;
    } finally {
      busy = false;
    }
  }

  const canStart = $derived(
    hostSource === "lan"
      ? true
      : hostSource === "port"
        ? portValid(manualPort)
        : !!jarPath && eula,
  );
  function startFromSource() {
    if (hostSource === "lan") startHost();
    else if (hostSource === "port") startHost(Number(manualPort));
    else startServerAndHost();
  }

  type Recent = { code: string; world: string; at: number };
  let recents = $state<Recent[]>(
    typeof localStorage === "undefined" ? [] : JSON.parse(localStorage.getItem("mh-recents") ?? "[]"),
  );
  function saveRecent(code: string, world: string) {
    recents = [{ code, world, at: Date.now() }, ...recents.filter((r) => r.code !== code)].slice(0, 5);
    localStorage.setItem("mh-recents", JSON.stringify(recents));
  }

  async function joinHost() {
    busy = true;
    error = "";
    try {
      const port = await invoke<number>("join", { code: joinCode.trim(), playerName });
      localPort = port;
      statusLine = `Туннель готов (127.0.0.1:${port}), устанавливаем связь…`;
      mode = "guest";
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  async function stopSession() {
    await invoke("stop");
    if (serverRunning) {
      await invoke("server_stop");
      serverRunning = false;
    }
    peers = {};
    rttHist = {};
    inviteCode = "";
    statusLine = "";
    localPort = 0;
    lastHostPort = null;
    linkState = "up";
    mode = "home";
  }

  async function copyCode() {
    await navigator.clipboard.writeText(inviteLink);
    copied = true;
    setTimeout(() => (copied = false), 1500);
  }

  type DiagPath = { addr: string; relay: boolean; selected: boolean; rtt_ms: number };
  type Diag = {
    peer_id: string;
    rtt_ms: number;
    path: string;
    paths: DiagPath[];
    addrs: string[];
    self_addrs: string[];
  };
  let diags = $state<Diag[]>([]);
  let diagData = $state<Diag | Diag[] | null>(null);
  let showRaw = $state(false);
  // Строку JSON считаем только когда блок реально открыт.
  const diagRaw = $derived(showRaw ? JSON.stringify(diagData, null, 2) : "");
  async function refreshDiag() {
    try {
      const d = await invoke<Diag | Diag[] | null>("diagnostics");
      diagData = d;
      diags = d == null ? [] : Array.isArray(d) ? d : [d];
    } catch {
      /* сессия могла закрыться между тиками */
    }
  }
  // живая диагностика, пока открыта вкладка
  $effect(() => {
    if (tab !== "diag") return;
    refreshDiag();
    const t = setInterval(refreshDiag, 2000);
    return () => clearInterval(t);
  });
  const shortId = (id: string) => (id.length > 12 ? `${id.slice(0, 12)}…` : id);

  let updateMsg = $state("");
  let updateReady = $state(false);
  /// Версия, найденная тихой проверкой при старте (бейдж на вкладке).
  let updateAvail = $state("");
  async function checkUpdate() {
    updateMsg = "Проверяю…";
    try {
      const u = await check();
      if (u) {
        updateMsg = `Доступна ${u.version}, скачиваю…`;
        await u.downloadAndInstall();
        updateMsg = "Обновление установлено";
        updateReady = true;
        updateAvail = "";
      } else {
        updateMsg = "У тебя последняя версия";
      }
    } catch (e) {
      updateMsg = `Проверка недоступна: ${e}`;
    }
  }
  // Тихая проверка при старте: не качаем ничего сами, только показываем бейдж.
  $effect(() => {
    check()
      .then((u) => {
        if (u) {
          updateAvail = u.version;
          updateMsg = `Доступна версия ${u.version}`;
        }
      })
      .catch(() => {});
  });

  /// Кик в два клика: первый «взводит», второй подтверждает.
  let kickArm = $state<string | null>(null);
  let kickTimer: ReturnType<typeof setTimeout> | undefined;
  async function kickPeer(id: string) {
    if (kickArm !== id) {
      kickArm = id;
      clearTimeout(kickTimer);
      kickTimer = setTimeout(() => (kickArm = null), 2500);
      return;
    }
    kickArm = null;
    await invoke("kick", { id });
    delete peers[id];
  }
  async function rotateCode() {
    busy = true;
    error = "";
    const port = lastHostPort;
    try {
      await invoke("rotate_code");
      peers = {};
      rttHist = {};
      // Тот же источник, что и был: для port/jar LAN-поиск не нужен и упал бы.
      await startHost(port ?? undefined);
    } catch (e) {
      error = String(e);
      mode = "home";
    } finally {
      busy = false;
    }
  }
</script>

{#snippet sigBars(pv: { level: number; col: string })}
  <div class="sig">
    {#each [1, 2, 3, 4] as n (n)}
      <span
        class="bar"
        style:height="{4 + n * 5}px"
        style:background={n <= pv.level ? pv.col : "#3a3a3a"}
      ></span>
    {/each}
  </div>
{/snippet}

{#snippet playerRow(id: string, p: Peer)}
  {@const sk = skinFor(p.name)}
  {@const pv = pingView(p.rtt_ms)}
  <div class="player-row joined">
    <div class="p-head" style:background={sk.skin}>
      <span class="ph-hair" style:background={sk.hair}></span>
      <span class="ph-eye l"></span>
      <span class="ph-pupil l" style:box-shadow="inset -3px 0 0 {sk.eye}"></span>
      <span class="ph-eye r"></span>
      <span class="ph-pupil r" style:box-shadow="inset 3px 0 0 {sk.eye}"></span>
      <span class="ph-mouth"></span>
    </div>
    <div class="p-info">
      <div class="p-name">{p.name}</div>
      <div class="p-status">
        <span class="p-dot"></span>
        <span>{mode === "guest" ? "хост" : "подключён"} · {pathLabel(p.path)}</span>
      </div>
    </div>
    {@render sigBars(pv)}
    <div class="ping" style:color={pv.col}>{pv.txt}</div>
    {#if mode === "host"}
      <button
        class="kick-btn"
        class:armed={kickArm === id}
        title="Выгнать до конца сессии"
        onclick={() => kickPeer(id)}>{kickArm === id ? "ТОЧНО?" : "КИК"}</button
      >
    {/if}
  </div>
{/snippet}

<div class="page">
  <div class="shell">
    <!-- ===== HEADER ===== -->
    <header class="header">
      <div class="logo">
        <div class="logo-grass"></div>
        <div class="logo-dirt"></div>
      </div>
      <div class="title-wrap">
        <div class="app-title">MineHost</div>
        <div class="app-sub">P2P-хостинг миров · прямое подключение без сервера</div>
      </div>
      <div class="status-chip">
        <div
          class="chip-dot"
          class:blink={phase === "connecting"}
          style:background={statusColor}
          style:box-shadow="inset 1px 1px 0 rgba(255,255,255,.5), inset -1px -1px 0 rgba(0,0,0,.4), 0 0 8px {statusGlow}"
        ></div>
        <span style:color={statusColor}>{statusShort}</span>
      </div>
    </header>

    <!-- ===== NAV ===== -->
    <nav class="nav">
      <button class="nav-btn" class:active={tab === "host"} onclick={() => (tab = "host")}>
        Хостинг
        <span class="nav-bar"></span>
      </button>
      <button class="nav-btn" class:active={tab === "join"} onclick={() => (tab = "join")}>
        Подключение
        <span class="nav-bar"></span>
      </button>
      <button class="nav-btn" class:active={tab === "players"} onclick={() => (tab = "players")}>
        Игроки
        <span class="badge" class:on={peerCount > 0}>{peerCount}</span>
        <span class="nav-bar"></span>
      </button>
      <button class="nav-btn" class:active={tab === "diag"} onclick={() => (tab = "diag")}>
        Диагностика
        {#if updateAvail && !updateReady}<span class="upd-dot" title="Доступно обновление"></span>{/if}
        <span class="nav-bar"></span>
      </button>
    </nav>

    <!-- ===== CONTENT ===== -->
    <div class="content">
      {#if error}
        <div class="error-banner">
          <span class="eb-text">⚠ {error}</span>
          <button class="eb-x" onclick={() => (error = "")}>✕</button>
        </div>
      {/if}

      <!-- ====== TAB: HOST ====== -->
      {#if tab === "host"}
        {#if mode === "guest"}
          <div class="empty-block">
            <div class="empty-icon pulse">🧭</div>
            <div class="empty-title">Ты подключён к чужому миру</div>
            <div class="empty-sub">Чтобы хостить свой мир, сначала отключись от текущего.</div>
            <button class="btn-px green" onclick={() => (tab = "join")}>К подключению →</button>
          </div>
        {:else}
          <div class="host-grid">
            <!-- left: source + host control -->
            <div>
              <div class="label green">ИСТОЧНИК МИРА</div>
              <div class="seg3">
                <button
                  class="seg"
                  class:on={hostSource === "lan"}
                  disabled={mode !== "home" || busy}
                  onclick={() => (hostSource = "lan")}>LAN-мир</button
                >
                <button
                  class="seg"
                  class:on={hostSource === "port"}
                  disabled={mode !== "home" || busy}
                  onclick={() => (hostSource = "port")}>Свой порт</button
                >
                <button
                  class="seg"
                  class:on={hostSource === "jar"}
                  disabled={mode !== "home" || busy}
                  onclick={() => (hostSource = "jar")}>server.jar</button
                >
              </div>

              <div class="panel src-panel">
                {#if hostSource === "lan"}
                  <p class="hint-text">
                    Открой мир в игре: <b>Esc → Open to LAN</b>. MineHost сам найдёт его и проведёт
                    друзей напрямую к тебе.
                  </p>
                {:else if hostSource === "port"}
                  <div class="field-label">Порт запущенного сервера</div>
                  <input
                    class="px-input"
                    placeholder="25565"
                    bind:value={manualPort}
                    disabled={mode !== "home" || busy}
                  />
                {:else}
                  <button class="btn-px dark file-btn" disabled={mode !== "home" || busy} onclick={pickJar}>
                    {jarPath ? jarPath.split(/[\\/]/).pop() : "📦 Выбрать server.jar"}
                  </button>
                  <div class="field-label">RAM, МБ</div>
                  <input class="px-input" placeholder="4096" bind:value={ramMb} disabled={mode !== "home" || busy} />
                  <button class="toggle-row" disabled={mode !== "home" || busy} onclick={() => (eula = !eula)}>
                    <span class="toggle-label">Принимаю Minecraft EULA</span>
                    <span class="track" class:on={eula}><span class="knob"></span></span>
                  </button>
                  {#if serverLines.length}
                    <pre class="server-logbox" bind:this={logBox}>{serverLines.join("\n")}</pre>
                  {/if}
                {/if}
              </div>

              {#if phase === "connecting"}
                <button class="host-toggle yellow" disabled>
                  <span class="glyph spin">◌</span>
                  <span class="t-label">Подключение…</span>
                  <span class="t-hint">Устанавливаем P2P-туннель…</span>
                </button>
              {:else if mode === "host"}
                <button class="host-toggle red" onclick={stopSession}>
                  <span class="glyph">■</span>
                  <span class="t-label">Остановить хост</span>
                  <span class="t-hint">Хост активен — друзья могут заходить</span>
                </button>
              {:else}
                <button class="host-toggle green" disabled={!canStart} onclick={startFromSource}>
                  <span class="glyph">▶</span>
                  <span class="t-label">Запустить хост</span>
                  <span class="t-hint">Запусти, чтобы открыть мир для друзей</span>
                </button>
              {/if}
            </div>

            <!-- right: status + invite -->
            <div>
              <div class="label green">СТАТУС СОЕДИНЕНИЯ</div>
              <div class="panel status-panel">
                <div class="status-head">
                  <div
                    class="status-dot"
                    class:blink={phase === "connecting"}
                    style:background={statusColor}
                    style:box-shadow="inset 2px 2px 0 rgba(255,255,255,.45), inset -2px -2px 0 rgba(0,0,0,.4), 0 0 10px {statusGlow}"
                  ></div>
                  <span class="status-label" style:color={statusColor}>{statusLabel}</span>
                </div>
                <div class="srow">
                  <span class="sk">P2P-туннель</span>
                  <span class="sv" style:color={statusColor}>{tunnelTxt}</span>
                </div>
                <div class="srow">
                  <span class="sk">Время работы</span>
                  <span class="sv">{uptime}</span>
                </div>
                <div class="srow">
                  <span class="sk">Игроков онлайн</span>
                  <span class="sv">{peerCount}</span>
                </div>
              </div>

              <div class="label yellow">ССЫЛКА-ПРИГЛАШЕНИЕ</div>
              {#if mode === "host" && inviteLink}
                <div class="invite-row">
                  <div class="invite-box">{inviteLink}</div>
                  <button class="btn-px copy-btn" class:green={copied} class:yellow={!copied} onclick={copyCode}>
                    {copied ? "✓ скопировано" : "Скопировать"}
                  </button>
                </div>
                <div class="hint-text">
                  Отправь её друзьям — они подключатся напрямую к твоему ПК. Ссылка живёт, пока
                  запущен хост.
                </div>
                <button class="btn-px dark rotate-btn" disabled={busy} onclick={rotateCode}
                  >⟳ Новый код приглашения</button
                >
                <div class="hint-text dim">Старые ссылки перестанут работать, игроки будут отключены.</div>
              {:else}
                <div class="dashed-box">— запусти хост, чтобы получить ссылку —</div>
              {/if}
            </div>
          </div>
        {/if}
      {/if}

      <!-- ====== TAB: JOIN ====== -->
      {#if tab === "join"}
        {#if mode === "guest"}
          <div class="join-session">
            <div class="label green">ТЕКУЩЕЕ ПОДКЛЮЧЕНИЕ</div>
            <h2 class="h-px">{worldName || "Мир друга"}</h2>
            {#if statusLine}<p class="hint-text">{statusLine}</p>{/if}
            {#if localPort}
              <p class="hint-text dim">
                Если мира нет в LAN-списке: Multiplayer → Direct Connect →
                <span class="inline-addr">127.0.0.1:{localPort}</span>
              </p>
            {/if}
            <div class="players-list">
              {#each Object.entries(peers) as [id, p] (id)}
                {@render playerRow(id, p)}
              {/each}
            </div>
            <button class="btn-px red wide" onclick={stopSession}>■ Отключиться</button>
          </div>
        {:else}
          <div class="join-head">
            <div class="label green">ПОДКЛЮЧИТЬСЯ К ДРУГУ</div>
            <h2 class="h-px">Вставь код приглашения</h2>
          </div>
          <div class="panel join-form">
            <div class="jf-grid">
              <div>
                <div class="field-label">Твой ник</div>
                <input class="px-input" placeholder="Player" bind:value={playerName} disabled={busy} />
              </div>
              <div>
                <div class="field-label">Код приглашения</div>
                <input
                  class="px-input"
                  placeholder="mh:… или minehost://join/…"
                  bind:value={joinCode}
                  disabled={busy}
                  onkeydown={(e) => {
                    if (e.key === "Enter" && !busy && joinCode.trim()) joinHost();
                  }}
                />
              </div>
            </div>
            <button class="btn-px green wide tall" disabled={busy || !joinCode.trim()} onclick={joinHost}>
              {busy ? "Подключение…" : "▶ Подключиться"}
            </button>
          </div>

          {#if recents.length}
            <div class="label yellow recents-label">НЕДАВНИЕ МИРЫ</div>
            <div class="worlds-grid">
              {#each recents as r (r.code)}
                {@const b = biomeFor(r.code)}
                <button
                  class="world-card"
                  disabled={busy}
                  onclick={() => {
                    joinCode = r.code;
                    joinHost();
                  }}
                >
                  <div class="wc-preview" style:background="linear-gradient(180deg, {b.skyTop}, {b.sky})">
                    <span class="wc-sun" style:background={b.sun} style:box-shadow="inset 2px 2px 0 rgba(255,255,255,.4), 0 0 12px {b.sun}"></span>
                    <span class="wc-cloud c1"></span>
                    <span class="wc-cloud c2"></span>
                    <span
                      class="wc-ground"
                      style:background={b.dirt}
                      style:box-shadow="inset 0 7px 0 {b.grass}, inset 0 11px 0 {b.grassHi}"
                    ></span>
                    <span class="wc-tex"></span>
                  </div>
                  <div class="wc-body">
                    <div class="wc-name">{r.world}</div>
                    <div class="wc-meta">
                      <span>⟳ {ago(r.at)}</span>
                      <span class="wc-pick">⛏</span>
                    </div>
                  </div>
                  <div class="wc-footer">▶ Подключиться →</div>
                </button>
              {/each}
            </div>
          {/if}
        {/if}
      {/if}

      <!-- ====== TAB: PLAYERS ====== -->
      {#if tab === "players"}
        <div class="players-head">
          <div>
            <div class="label green">
              {mode === "guest" ? "ТВОЁ ПОДКЛЮЧЕНИЕ" : "ПОДКЛЮЧЁННЫЕ ИГРОКИ"}
            </div>
            <h2 class="h-px">{mode === "guest" ? worldName || "Мир друга" : "Твой мир"}</h2>
          </div>
          <div class="players-count">
            <div class="pc-num" style:color={statusColor}>{peerCount}</div>
            <div class="pc-sub">в сети</div>
          </div>
        </div>

        {#if mode === "home"}
          <div class="empty-block">
            <div class="off-dot"></div>
            <div class="empty-title">Хост не запущен</div>
            <button class="btn-px green" onclick={() => (tab = "host")}>К запуску хоста →</button>
          </div>
        {:else if peerCount === 0}
          <div class="empty-block">
            <div class="empty-icon pulse">⛏</div>
            <div class="empty-title">Ожидание подключений…</div>
            <div class="empty-sub">
              {mode === "host"
                ? "Хост запущен. Отправь ссылку-приглашение друзьям."
                : "Туннель готов — ждём данные от хоста."}
            </div>
          </div>
        {:else}
          <div class="players-list">
            {#each Object.entries(peers) as [id, p] (id)}
              {@render playerRow(id, p)}
            {/each}
          </div>
        {/if}
      {/if}

      <!-- ====== TAB: DIAG ====== -->
      {#if tab === "diag"}
        <div class="diag-wrap">
          <div class="label green">ДИАГНОСТИКА</div>
          <h2 class="h-px">Состояние туннеля</h2>

          {#if diags.length === 0}
            <div class="dashed-box">
              {mode === "home"
                ? "— сессии нет: запусти хост или подключись к другу —"
                : "— ждём соединения с пиром… —"}
            </div>
          {:else}
            {#each diags as d (d.peer_id)}
              {@const pv = pingView(d.rtt_ms)}
              <div class="panel diag-card">
                <div class="dc-head">
                  <span class="dc-id" title={d.peer_id}>⛓ {shortId(d.peer_id)}</span>
                  <span
                    class="dc-path"
                    class:direct={d.path === "direct"}
                    class:relay={d.path === "relay"}>{pathLabel(d.path)}</span
                  >
                  <span class="dc-fill"></span>
                  {@render sigBars(pv)}
                  <span class="ping" style:color={pv.col}>{pv.txt}</span>
                </div>
                <div class="dc-sub">Пути к пиру</div>
                <div class="addr-list">
                  {#each d.paths as p (p.addr)}
                    <div class="path-row" class:sel={p.selected}>
                      <span class="path-tag" class:relay={p.relay}>{p.relay ? "🌐 релей" : "⚡ прямой"}</span>
                      <span class="path-addr">{p.addr}</span>
                      {#if p.selected}<span class="path-sel">▶ трафик здесь</span>{/if}
                      <span class="path-rtt">{p.rtt_ms ? `${p.rtt_ms} ms` : "…"}</span>
                    </div>
                  {:else}
                    {#each d.addrs as a (a)}<span class="addr">{a}</span>{:else}<span class="addr dim"
                        >пути ещё не открыты</span
                      >{/each}
                  {/each}
                </div>
                {#if (rttHist[d.peer_id] ?? []).length > 1}
                  <div class="dc-sub">Пинг за последние ~2 минуты</div>
                  <div class="spark">
                    {#each rttHist[d.peer_id] ?? [] as v, i (i)}
                      <span
                        class="spark-bar"
                        title="{v} ms"
                        style:height="{Math.max(6, Math.min(100, (v / 250) * 100))}%"
                        style:background={!v ? "#3a3a3a" : v < 110 ? "#5fbf4f" : v < 185 ? "#f3c63a" : "#d6504a"}
                      ></span>
                    {/each}
                  </div>
                {/if}
              </div>
            {/each}
            <div class="panel diag-card">
              <div class="dc-sub">Наши адреса (что видит пир)</div>
              <div class="addr-list">
                {#each diags[0].self_addrs as a (a)}<span class="addr">{a}</span>{/each}
              </div>
            </div>
          {/if}

          <div class="diag-btns">
            {#if updateReady}
              <button class="btn-px yellow" onclick={() => relaunch()}>⟳ Перезапустить сейчас</button>
            {:else if updateAvail}
              <button class="btn-px yellow" onclick={checkUpdate}>⬇ Установить {updateAvail}</button>
            {:else}
              <button class="btn-px green" onclick={checkUpdate}>Проверить обновления</button>
            {/if}
            <button class="btn-px dark" onclick={() => (showRaw = !showRaw)}>
              {showRaw ? "− скрыть JSON" : "+ сырой JSON"}
            </button>
          </div>
          {#if updateMsg}<p class="hint-text">{updateMsg}</p>{/if}
          {#if showRaw}<pre class="diag-pre">{diagRaw || "нет данных"}</pre>{/if}

          <p class="hint-text dim">
            Обновляется каждые 2 секунды. rtt и путь (напрямую / релей) помогают разобраться,
            почему 🌐 вместо ⚡.
          </p>
        </div>
      {/if}
    </div>

    <!-- ===== FOOTER ===== -->
    <footer class="footer">
      <span>MineHost · одноранговый хостинг</span>
      <span>v{appVersion || "?"} · {footerStatus}</span>
    </footer>
  </div>
</div>

<style>
  @keyframes mh-join {
    0% {
      opacity: 0;
      transform: translateX(-14px);
    }
    100% {
      opacity: 1;
      transform: translateX(0);
    }
  }
  @keyframes mh-blink {
    0%,
    100% {
      opacity: 1;
    }
    50% {
      opacity: 0.25;
    }
  }
  @keyframes mh-spin {
    0% {
      transform: rotate(0);
    }
    100% {
      transform: rotate(360deg);
    }
  }
  @keyframes mh-pulse {
    0%,
    100% {
      opacity: 0.4;
    }
    50% {
      opacity: 1;
    }
  }

  :global(html, body) {
    margin: 0;
    padding: 0;
    box-sizing: border-box;
  }
  :global(body) {
    background: #101010;
    color: #d6d6d6;
    font-family: "VT323", "Courier New", monospace;
    -webkit-font-smoothing: none;
  }
  :global(*),
  :global(*::before),
  :global(*::after) {
    box-sizing: border-box;
  }
  :global(::-webkit-scrollbar) {
    width: 12px;
    height: 12px;
    background: #101010;
  }
  :global(::-webkit-scrollbar-thumb) {
    background: #2e2e2e;
    border: 2px solid #000;
  }
  :global(button) {
    font-family: inherit;
  }
  /* клавиатурная навигация: рамка только от фокуса с клавиатуры */
  :global(button:focus-visible),
  :global(input:focus-visible) {
    outline: 2px solid #7ec8ff;
    outline-offset: 2px;
  }
  @media (prefers-reduced-motion: reduce) {
    :global(*),
    :global(*::before),
    :global(*::after) {
      animation-duration: 0.01ms !important;
      animation-iteration-count: 1 !important;
      transition-duration: 0.01ms !important;
    }
  }

  .page {
    min-height: 100vh;
    padding: 28px 20px;
    display: flex;
    justify-content: center;
    align-items: flex-start;
    background: #101010;
    background-image:
      linear-gradient(rgba(255, 255, 255, 0.02) 1px, transparent 1px),
      linear-gradient(90deg, rgba(255, 255, 255, 0.02) 1px, transparent 1px);
    background-size: 24px 24px;
  }
  .shell {
    width: 100%;
    max-width: 1080px;
    background: #191919;
    border: 2px solid #000;
    box-shadow:
      inset 2px 2px 0 #313131,
      inset -2px -2px 0 #060606,
      0 16px 50px rgba(0, 0, 0, 0.65);
  }

  /* ===== header ===== */
  .header {
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 16px 22px;
    border-bottom: 2px solid #000;
    background: #141414;
    box-shadow: inset 0 2px 0 #2b2b2b;
  }
  .logo {
    width: 44px;
    height: 44px;
    flex: 0 0 auto;
    border: 2px solid #000;
    box-shadow:
      inset 2px 2px 0 rgba(255, 255, 255, 0.18),
      inset -2px -2px 0 rgba(0, 0, 0, 0.5);
    image-rendering: pixelated;
    background: #6b4a2c;
  }
  .logo-grass {
    height: 14px;
    background: #5fa84e;
    box-shadow:
      inset 0 3px 0 #7ec85f,
      inset 0 -2px 0 #3f7a36;
  }
  .logo-dirt {
    height: 6px;
    background: #7a5230;
  }
  .title-wrap {
    flex: 1 1 auto;
    min-width: 0;
  }
  .app-title {
    font-family: "Press Start 2P", monospace;
    font-size: 18px;
    color: #f4f4f4;
    text-shadow: 3px 3px 0 #000;
    letter-spacing: 1px;
  }
  .app-sub {
    font-size: 18px;
    color: #7d7d7d;
    margin-top: 4px;
    letter-spacing: 1px;
  }
  .status-chip {
    display: flex;
    align-items: center;
    gap: 9px;
    padding: 8px 12px;
    border: 2px solid #000;
    background: #101010;
    box-shadow:
      inset 2px 2px 0 #2a2a2a,
      inset -2px -2px 0 #000;
  }
  .status-chip span {
    font-family: "Press Start 2P", monospace;
    font-size: 9px;
    text-shadow: 2px 2px 0 #000;
  }
  .chip-dot {
    width: 13px;
    height: 13px;
  }
  .blink {
    animation: mh-blink 1s infinite;
  }

  /* ===== nav ===== */
  .nav {
    display: flex;
    gap: 0;
    background: #101010;
    border-bottom: 2px solid #000;
    padding: 0 14px;
  }
  .nav-btn {
    position: relative;
    flex: 0 0 auto;
    background: transparent;
    border: none;
    cursor: pointer;
    padding: 15px 20px 12px;
    font-family: "Press Start 2P", monospace;
    font-size: 10px;
    color: #7d7d7d;
    text-shadow: 2px 2px 0 #000;
  }
  .nav-btn:hover {
    color: #fff;
  }
  .nav-btn.active {
    color: #fff;
  }
  .nav-bar {
    position: absolute;
    left: 14px;
    right: 14px;
    bottom: 0;
    height: 3px;
    background: transparent;
  }
  .nav-btn.active .nav-bar {
    background: #5fbf4f;
  }
  .badge {
    font-family: "VT323", monospace;
    font-size: 16px;
    margin-left: 7px;
    padding: 1px 7px;
    background: #2a2a2a;
    color: #fff;
    border: 2px solid #000;
  }
  .badge.on {
    background: #3f8a30;
  }
  .upd-dot {
    display: inline-block;
    width: 8px;
    height: 8px;
    margin-left: 7px;
    background: #f3c63a;
    border: 1px solid #000;
    box-shadow: 0 0 6px rgba(243, 198, 58, 0.6);
    animation: mh-blink 1.2s infinite;
  }

  /* ===== content ===== */
  .content {
    padding: 30px 30px 36px;
    min-height: 520px;
  }

  .error-banner {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 14px;
    margin-bottom: 22px;
    padding: 12px 16px;
    border: 2px solid #000;
    background: #2a1414;
    box-shadow:
      inset 2px 2px 0 #4a2020,
      inset -2px -2px 0 #000;
  }
  .eb-text {
    font-size: 19px;
    color: #ff8c86;
    word-break: break-word;
  }
  .eb-x {
    flex: 0 0 auto;
    cursor: pointer;
    background: transparent;
    border: none;
    color: #d6504a;
    font-family: "Press Start 2P", monospace;
    font-size: 10px;
    text-shadow: 1px 1px 0 #000;
  }

  .label {
    font-family: "Press Start 2P", monospace;
    font-size: 10px;
    text-shadow: 2px 2px 0 #000;
    letter-spacing: 2px;
    margin-bottom: 12px;
  }
  .label.green {
    color: #5fbf4f;
  }
  .label.yellow {
    color: #f3c63a;
  }
  .h-px {
    font-family: "Press Start 2P", monospace;
    font-size: 20px;
    font-weight: normal;
    color: #f4f4f4;
    text-shadow: 3px 3px 0 #000;
    margin: 0 0 8px;
    line-height: 1.5;
  }

  .panel {
    padding: 18px;
    border: 2px solid #000;
    background: #141414;
    box-shadow:
      inset 2px 2px 0 #2e2e2e,
      inset -2px -2px 0 #050505;
  }

  .hint-text {
    font-size: 17px;
    color: #7d7d7d;
    line-height: 1.35;
    margin: 12px 0 0;
  }
  .hint-text b {
    color: #b8b8b8;
  }
  .hint-text.dim {
    color: #5f5f5f;
  }
  .field-label {
    font-family: "Press Start 2P", monospace;
    font-size: 9px;
    color: #e4e4e4;
    text-shadow: 2px 2px 0 #000;
    margin: 14px 0 10px;
  }
  .field-label:first-child {
    margin-top: 0;
  }

  .px-input {
    width: 100%;
    padding: 10px 12px;
    border: 2px solid #000;
    background: #0c0c0c;
    box-shadow:
      inset 2px 2px 0 #222,
      inset -2px -2px 0 #000;
    font-family: "VT323", "Courier New", monospace;
    font-size: 21px;
    color: #7ec8ff;
  }
  .px-input::placeholder {
    color: #4a4a4a;
  }
  .px-input:focus {
    outline: none;
    box-shadow:
      inset 2px 2px 0 #222,
      inset -2px -2px 0 #000,
      0 0 0 2px #5fbf4f;
  }
  .px-input:disabled {
    opacity: 0.45;
  }

  /* pixel buttons */
  .btn-px {
    cursor: pointer;
    padding: 13px 22px;
    border: 2px solid #000;
    font-family: "Press Start 2P", monospace;
    font-size: 10px;
    color: #fff;
    text-shadow: 2px 2px 0 rgba(0, 0, 0, 0.5);
    box-shadow:
      inset 2px 2px 0 rgba(255, 255, 255, 0.3),
      inset -3px -3px 0 rgba(0, 0, 0, 0.4);
  }
  .btn-px:active:not(:disabled) {
    transform: translateY(2px);
    filter: brightness(0.92);
  }
  .btn-px:disabled {
    opacity: 0.45;
    cursor: default;
  }
  .btn-px.green {
    background: linear-gradient(#62c24f, #3f8a30);
  }
  .btn-px.green:hover:not(:disabled) {
    background: linear-gradient(#6fd25b, #47993a);
  }
  .btn-px.yellow {
    background: linear-gradient(#e0b93a, #b08a18);
  }
  .btn-px.yellow:hover:not(:disabled) {
    background: linear-gradient(#ecc94e, #bd961f);
  }
  .btn-px.red {
    background: linear-gradient(#c0463a, #8c2a22);
  }
  .btn-px.red:hover:not(:disabled) {
    background: linear-gradient(#cf5448, #9a322a);
  }
  .btn-px.dark {
    background: #222;
    color: #c8c8c8;
    box-shadow:
      inset 1px 1px 0 #383838,
      inset -1px -1px 0 #000;
  }
  .btn-px.dark:hover:not(:disabled) {
    background: #2c2c2c;
    color: #fff;
  }
  .btn-px.wide {
    width: 100%;
  }
  .btn-px.tall {
    padding: 17px 22px;
    font-size: 12px;
  }

  /* ===== host tab ===== */
  .host-grid {
    display: grid;
    grid-template-columns: 1.2fr 1fr;
    gap: 24px;
    align-items: start;
  }
  .seg3 {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 8px;
    margin-bottom: 14px;
  }
  .seg {
    cursor: pointer;
    padding: 13px 6px;
    border: 2px solid #000;
    background: #222;
    box-shadow:
      inset 1px 1px 0 #383838,
      inset -1px -1px 0 #000;
    font-family: "Press Start 2P", monospace;
    font-size: 9px;
    color: #8a8a8a;
    text-shadow: 1px 1px 0 #000;
  }
  .seg:hover:not(:disabled) {
    color: #c8c8c8;
  }
  .seg.on {
    background: linear-gradient(#62c24f, #3f8a30);
    color: #fff;
    box-shadow:
      inset 2px 2px 0 rgba(255, 255, 255, 0.3),
      inset -2px -2px 0 rgba(0, 0, 0, 0.4);
  }
  .seg:disabled {
    cursor: default;
    opacity: 0.6;
  }
  .src-panel {
    margin-bottom: 22px;
  }
  .src-panel .hint-text {
    margin: 0;
    font-size: 18px;
  }
  .file-btn {
    width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .toggle-row {
    width: 100%;
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    background: transparent;
    border: none;
    padding: 14px 0 0;
  }
  .toggle-row:disabled {
    opacity: 0.45;
    cursor: default;
  }
  .toggle-label {
    font-family: "Press Start 2P", monospace;
    font-size: 9px;
    color: #e4e4e4;
    text-shadow: 2px 2px 0 #000;
  }
  .track {
    flex: 0 0 auto;
    width: 50px;
    height: 24px;
    border: 2px solid #000;
    background: #2a2a2a;
    position: relative;
    box-shadow: inset 1px 1px 0 rgba(0, 0, 0, 0.3);
  }
  .track.on {
    background: #3f8a30;
  }
  .knob {
    position: absolute;
    top: 0;
    bottom: 0;
    left: 0;
    width: 20px;
    background: #e0e0e0;
    border: 1px solid #000;
    box-shadow:
      inset 1px 1px 0 #fff,
      inset -1px -1px 0 #888;
  }
  .track.on .knob {
    left: auto;
    right: 0;
  }
  .server-logbox {
    margin: 14px 0 0;
    max-height: 150px;
    overflow-y: auto;
    font-family: "VT323", "Courier New", monospace;
    font-size: 16px;
    line-height: 1.25;
    color: #7d9d72;
    background: #0c0c0c;
    border: 2px solid #000;
    box-shadow:
      inset 2px 2px 0 #222,
      inset -2px -2px 0 #000;
    padding: 10px 12px;
    white-space: pre-wrap;
    word-break: break-word;
  }

  .host-toggle {
    width: 100%;
    cursor: pointer;
    padding: 30px 24px;
    border: 2px solid #000;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 14px;
    box-shadow:
      inset 3px 3px 0 rgba(255, 255, 255, 0.32),
      inset -4px -4px 0 rgba(0, 0, 0, 0.4);
  }
  .host-toggle:active:not(:disabled) {
    transform: translateY(2px);
    filter: brightness(0.92);
  }
  .host-toggle.green {
    background: linear-gradient(#62c24f, #3f8a30);
  }
  .host-toggle.green:hover:not(:disabled) {
    background: linear-gradient(#6fd25b, #47993a);
  }
  .host-toggle.yellow {
    background: linear-gradient(#e0b93a, #b08a18);
    cursor: default;
  }
  .host-toggle.red {
    background: linear-gradient(#c0463a, #8c2a22);
  }
  .host-toggle.red:hover {
    background: linear-gradient(#cf5448, #9a322a);
  }
  .host-toggle:disabled {
    opacity: 0.55;
    cursor: default;
  }
  .glyph {
    font-size: 46px;
    line-height: 1;
    color: #fff;
    text-shadow: 3px 3px 0 rgba(0, 0, 0, 0.5);
  }
  .glyph.spin {
    animation: mh-spin 1.4s linear infinite;
    text-shadow: none;
  }
  .t-label {
    font-family: "Press Start 2P", monospace;
    font-size: 15px;
    color: #fff;
    text-shadow: 2px 2px 0 rgba(0, 0, 0, 0.55);
  }
  .t-hint {
    font-size: 17px;
    color: rgba(255, 255, 255, 0.78);
  }

  .status-panel {
    margin-bottom: 22px;
  }
  .status-head {
    display: flex;
    align-items: center;
    gap: 13px;
    margin-bottom: 16px;
  }
  .status-dot {
    width: 18px;
    height: 18px;
    flex: 0 0 auto;
  }
  .status-label {
    font-family: "Press Start 2P", monospace;
    font-size: 11px;
    text-shadow: 2px 2px 0 #000;
    line-height: 1.5;
  }
  .srow {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    white-space: nowrap;
    padding: 10px 0;
    border-top: 1px solid #000;
  }
  .srow .sk {
    font-size: 18px;
    color: #8a8a8a;
  }
  .srow .sv {
    font-size: 18px;
    color: #d6d6d6;
  }

  .invite-row {
    display: flex;
    gap: 10px;
  }
  .invite-box {
    flex: 1 1 auto;
    min-width: 0;
    padding: 12px 14px;
    border: 2px solid #000;
    background: #0c0c0c;
    box-shadow:
      inset 2px 2px 0 #222,
      inset -2px -2px 0 #000;
    font-size: 20px;
    color: #7ec8ff;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    user-select: all;
  }
  .copy-btn {
    flex: 0 0 auto;
    padding: 0 18px;
  }
  .rotate-btn {
    margin-top: 16px;
  }
  .dashed-box {
    padding: 22px 18px;
    border: 2px dashed #3a3a3a;
    background: #0c0c0c;
    text-align: center;
    font-size: 19px;
    color: #6f6f6f;
  }

  /* ===== join tab ===== */
  .join-head {
    margin-bottom: 20px;
  }
  .join-form {
    margin-bottom: 28px;
  }
  .jf-grid {
    display: grid;
    grid-template-columns: 1fr 2fr;
    gap: 14px;
    margin-bottom: 18px;
  }
  .jf-grid .field-label {
    margin-top: 0;
  }
  .recents-label {
    margin-top: 4px;
  }
  .worlds-grid {
    display: grid;
    grid-template-columns: repeat(2, 1fr);
    gap: 18px;
  }
  .world-card {
    text-align: left;
    cursor: pointer;
    padding: 0;
    border: 2px solid #000;
    background: #141414;
    box-shadow:
      inset 2px 2px 0 #2e2e2e,
      inset -2px -2px 0 #050505;
    overflow: hidden;
    display: flex;
    flex-direction: column;
    font-family: inherit;
    color: inherit;
  }
  .world-card:hover:not(:disabled) {
    box-shadow:
      inset 2px 2px 0 #3f3f3f,
      inset -2px -2px 0 #050505,
      0 0 0 2px #5fbf4f;
  }
  .world-card:disabled {
    opacity: 0.6;
    cursor: default;
  }
  .wc-preview {
    position: relative;
    height: 104px;
    overflow: hidden;
  }
  .wc-sun {
    position: absolute;
    top: 14px;
    right: 18px;
    width: 20px;
    height: 20px;
    image-rendering: pixelated;
  }
  .wc-cloud {
    position: absolute;
    background: rgba(255, 255, 255, 0.18);
  }
  .wc-cloud.c1 {
    top: 30px;
    left: 24px;
    width: 26px;
    height: 7px;
  }
  .wc-cloud.c2 {
    top: 40px;
    left: 40px;
    width: 18px;
    height: 7px;
    background: rgba(255, 255, 255, 0.12);
  }
  .wc-ground {
    position: absolute;
    left: 0;
    right: 0;
    bottom: 0;
    height: 34px;
  }
  .wc-tex {
    position: absolute;
    left: 0;
    right: 0;
    bottom: 0;
    height: 7px;
    background: repeating-linear-gradient(90deg, rgba(0, 0, 0, 0.18) 0 8px, transparent 8px 16px);
  }
  .wc-body {
    padding: 14px 16px 16px;
  }
  .wc-name {
    font-family: "Press Start 2P", monospace;
    font-size: 12px;
    color: #f0f0f0;
    text-shadow: 2px 2px 0 #000;
    line-height: 1.5;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .wc-meta {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-top: 12px;
    font-size: 17px;
    color: #8a8a8a;
  }
  .wc-pick {
    color: #6f6f6f;
  }
  .wc-footer {
    padding: 11px 16px;
    border-top: 2px solid #000;
    background: #101010;
    font-family: "Press Start 2P", monospace;
    font-size: 9px;
    color: #5fbf4f;
    text-shadow: 2px 2px 0 #000;
  }

  .join-session {
    max-width: 680px;
  }
  .join-session .players-list {
    margin: 20px 0;
  }

  /* ===== players tab ===== */
  .players-head {
    display: flex;
    align-items: flex-end;
    justify-content: space-between;
    margin-bottom: 24px;
  }
  .players-count {
    text-align: right;
  }
  .pc-num {
    font-family: "Press Start 2P", monospace;
    font-size: 22px;
    text-shadow: 2px 2px 0 #000;
  }
  .pc-sub {
    font-size: 17px;
    color: #7d7d7d;
    margin-top: 6px;
  }
  .players-list {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .player-row {
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 12px 16px;
    border: 2px solid #000;
    background: #141414;
    box-shadow:
      inset 2px 2px 0 #2a2a2a,
      inset -2px -2px 0 #050505;
  }
  .player-row.joined {
    animation: mh-join 0.25s both;
  }
  .p-head {
    width: 46px;
    height: 46px;
    flex: 0 0 auto;
    position: relative;
    image-rendering: pixelated;
    border: 2px solid #000;
    box-shadow: inset -3px -3px 0 rgba(0, 0, 0, 0.22);
  }
  .ph-hair {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    height: 15px;
  }
  .ph-eye,
  .ph-pupil {
    position: absolute;
    top: 18px;
    width: 7px;
    height: 7px;
  }
  .ph-eye {
    background: #fff;
  }
  .ph-eye.l,
  .ph-pupil.l {
    left: 8px;
  }
  .ph-eye.r,
  .ph-pupil.r {
    right: 8px;
  }
  .ph-mouth {
    position: absolute;
    bottom: 7px;
    left: 14px;
    right: 14px;
    height: 4px;
    background: rgba(0, 0, 0, 0.3);
  }
  .p-info {
    flex: 1 1 auto;
    min-width: 0;
  }
  .p-name {
    font-family: "Press Start 2P", monospace;
    font-size: 12px;
    color: #f0f0f0;
    text-shadow: 2px 2px 0 #000;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .p-status {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 8px;
    font-size: 17px;
    color: #7d9d72;
  }
  .p-dot {
    width: 9px;
    height: 9px;
    background: #5fbf4f;
    box-shadow: 0 0 6px #5fbf4f;
  }
  .sig {
    display: flex;
    align-items: flex-end;
    gap: 3px;
    height: 22px;
  }
  .bar {
    width: 5px;
  }
  .ping {
    width: 78px;
    text-align: right;
    font-size: 20px;
  }
  .kick-btn {
    flex: 0 0 auto;
    cursor: pointer;
    padding: 8px 11px;
    border: 2px solid #000;
    background: #2a1414;
    box-shadow:
      inset 1px 1px 0 #4a2020,
      inset -1px -1px 0 #000;
    font-family: "Press Start 2P", monospace;
    font-size: 8px;
    color: #d6504a;
    text-shadow: 1px 1px 0 #000;
  }
  .kick-btn:hover {
    background: #3a1a1a;
  }
  .kick-btn.armed {
    background: #8c2a22;
    color: #fff;
    animation: mh-blink 0.6s infinite;
  }

  .empty-block {
    padding: 60px 20px;
    text-align: center;
    border: 2px dashed #2e2e2e;
    background: #0e0e0e;
  }
  .empty-icon {
    font-size: 34px;
    margin-bottom: 16px;
  }
  .empty-icon.pulse {
    animation: mh-pulse 1.4s infinite;
  }
  .off-dot {
    width: 18px;
    height: 18px;
    background: #d6504a;
    margin: 0 auto 18px;
    box-shadow:
      inset 2px 2px 0 rgba(255, 255, 255, 0.35),
      0 0 10px rgba(214, 80, 74, 0.5);
  }
  .empty-title {
    font-family: "Press Start 2P", monospace;
    font-size: 13px;
    color: #9a9a9a;
    text-shadow: 2px 2px 0 #000;
    line-height: 1.7;
  }
  .empty-sub {
    font-size: 18px;
    color: #6f6f6f;
    margin-top: 14px;
  }
  .empty-block .btn-px {
    margin-top: 20px;
  }

  /* ===== diag tab ===== */
  .diag-wrap {
    max-width: 680px;
  }
  .diag-btns {
    display: flex;
    gap: 10px;
    flex-wrap: wrap;
    margin-top: 18px;
  }
  .diag-card {
    margin-top: 12px;
  }
  .dc-head {
    display: flex;
    align-items: center;
    gap: 12px;
    flex-wrap: wrap;
  }
  .dc-id {
    font-family: "Press Start 2P", monospace;
    font-size: 10px;
    color: #e4e4e4;
    text-shadow: 2px 2px 0 #000;
  }
  .dc-path {
    font-size: 17px;
    padding: 2px 10px;
    border: 2px solid #000;
    background: #2a2a2a;
    color: #b8b8b8;
    box-shadow: inset 1px 1px 0 #383838;
  }
  .dc-path.direct {
    background: #1d3318;
    color: #7ec85f;
  }
  .dc-path.relay {
    background: #33290f;
    color: #f3c63a;
  }
  .dc-fill {
    flex: 1 1 auto;
  }
  .dc-sub {
    font-family: "Press Start 2P", monospace;
    font-size: 8px;
    color: #7d7d7d;
    text-shadow: 1px 1px 0 #000;
    letter-spacing: 1px;
    margin: 14px 0 8px;
  }
  .dc-sub:first-child {
    margin-top: 0;
  }
  .addr-list {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .addr {
    font-size: 16px;
    line-height: 1.2;
    color: #7ec8ff;
    background: #0c0c0c;
    border: 2px solid #000;
    box-shadow: inset 1px 1px 0 #222;
    padding: 4px 10px;
    word-break: break-all;
  }
  .addr.dim {
    color: #5f5f5f;
  }
  .inline-addr {
    color: #7ec8ff;
    user-select: all;
  }
  .path-row {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: 16px;
    line-height: 1.2;
    background: #0c0c0c;
    border: 2px solid #000;
    box-shadow: inset 1px 1px 0 #222;
    padding: 5px 10px;
  }
  .path-row.sel {
    border-color: #2f5a26;
    box-shadow:
      inset 1px 1px 0 #1d3318,
      0 0 0 1px #2f5a26;
  }
  .path-tag {
    flex: 0 0 auto;
    padding: 1px 8px;
    border: 2px solid #000;
    background: #1d3318;
    color: #7ec85f;
    box-shadow: inset 1px 1px 0 rgba(255, 255, 255, 0.08);
  }
  .path-tag.relay {
    background: #33290f;
    color: #f3c63a;
  }
  .path-addr {
    flex: 1 1 auto;
    min-width: 0;
    color: #7ec8ff;
    word-break: break-all;
  }
  .path-sel {
    flex: 0 0 auto;
    color: #7ec85f;
    font-size: 15px;
  }
  .path-rtt {
    flex: 0 0 auto;
    color: #d6d6d6;
    font-size: 17px;
  }
  .spark {
    display: flex;
    align-items: flex-end;
    gap: 2px;
    height: 46px;
    padding: 6px 8px;
    background: #0c0c0c;
    border: 2px solid #000;
    box-shadow:
      inset 2px 2px 0 #222,
      inset -2px -2px 0 #000;
  }
  .spark-bar {
    flex: 1 1 auto;
    max-width: 9px;
    min-width: 2px;
    image-rendering: pixelated;
  }
  .diag-pre {
    margin: 16px 0 0;
    font-family: "VT323", "Courier New", monospace;
    font-size: 17px;
    line-height: 1.25;
    overflow-x: auto;
    background: #0c0c0c;
    border: 2px solid #000;
    box-shadow:
      inset 2px 2px 0 #222,
      inset -2px -2px 0 #000;
    color: #7ec8ff;
    padding: 12px 14px;
    white-space: pre-wrap;
    word-break: break-all;
  }

  /* ===== footer ===== */
  .footer {
    padding: 13px 22px;
    border-top: 2px solid #000;
    background: #101010;
    display: flex;
    justify-content: space-between;
    align-items: center;
    font-size: 16px;
    color: #5a5a5a;
    letter-spacing: 0.5px;
  }

  @media (max-width: 860px) {
    .host-grid {
      grid-template-columns: 1fr;
    }
    .worlds-grid {
      grid-template-columns: 1fr;
    }
    .jf-grid {
      grid-template-columns: 1fr;
    }
  }
</style>
