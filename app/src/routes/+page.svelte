<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { onOpenUrl } from "@tauri-apps/plugin-deep-link";
  import { open } from "@tauri-apps/plugin-dialog";
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

  let mode = $state<"home" | "host" | "guest">("home");
  let busy = $state(false);
  let error = $state("");
  let inviteCode = $state("");
  let joinCode = $state("");
  let playerName = $state("Player");
  let statusLine = $state("");
  let worldName = $state("");
  let copied = $state(false);
  let peers = $state<Record<string, Peer>>({});

  const pathIcon = (p: string) =>
    p === "direct" ? "⚡" : p === "relay" ? "🌐" : p === "mixed" ? "⚡🌐" : "·";

  function handleEvent(ev: MhEvent) {
    switch (ev.type) {
      case "guest_joined":
        peers[ev.id] = { name: ev.name, rtt_ms: 0, path: "unknown" };
        break;
      case "guest_left":
        delete peers[ev.id];
        break;
      case "peer_status":
        peers[ev.id] = { name: peers[ev.id]?.name ?? "хост", rtt_ms: ev.rtt_ms, path: ev.path };
        break;
      case "joined_host":
        worldName = ev.world_name;
        statusLine = `Подключено! Открой Minecraft → Multiplayer: «${ev.world_name}» в LAN-списке`;
        saveRecent(joinCode.trim(), ev.world_name);
        break;
      case "disconnected":
        statusLine = `Связь потеряна: ${ev.reason}`;
        break;
      case "reconnecting":
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

  let manualPort = $state("");
  const portValid = (s: string) => /^\d+$/.test(s) && +s > 0 && +s < 65536;

  async function startHost(port?: number) {
    busy = true;
    error = "";
    try {
      inviteCode = await invoke<string>("start_host", { manualPort: port ?? null });
      mode = "host";
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  let jarPath = $state("");
  let ramMb = $state("4096");
  let eula = $state(false);
  let serverLog = $state("");
  let serverRunning = $state(false);
  $effect(() => {
    const un = listen<string>("mh-server-log", (e) => (serverLog = e.payload));
    return () => {
      un.then((f) => f());
    };
  });
  async function pickJar() {
    const p = await open({ filters: [{ name: "Server JAR", extensions: ["jar"] }] });
    if (typeof p === "string") jarPath = p;
  }
  async function startServerAndHost() {
    busy = true;
    error = "";
    serverLog = "Запускаю сервер…";
    try {
      const port = await invoke<number>("server_start", {
        jarPath,
        ramMb: Number(ramMb),
        acceptEula: eula,
      });
      serverRunning = true;
      await startHost(port);
    } catch (e) {
      error = String(e);
      await invoke("server_stop");
      serverRunning = false;
    } finally {
      busy = false;
    }
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
    inviteCode = "";
    statusLine = "";
    mode = "home";
  }

  async function copyCode() {
    await navigator.clipboard.writeText(inviteLink);
    copied = true;
    setTimeout(() => (copied = false), 1500);
  }

  let diagText = $state("");
  async function refreshDiag() {
    diagText = JSON.stringify(await invoke("diagnostics"), null, 2);
  }
  let updateMsg = $state("");
  async function checkUpdate() {
    updateMsg = "Проверяю…";
    try {
      const u = await check();
      if (u) {
        updateMsg = `Доступна ${u.version}, скачиваю…`;
        await u.downloadAndInstall();
        updateMsg = "Установлено — перезапусти приложение";
      } else {
        updateMsg = "У тебя последняя версия";
      }
    } catch (e) {
      updateMsg = `Проверка недоступна: ${e}`;
    }
  }
  async function kickPeer(id: string) {
    await invoke("kick", { id });
    delete peers[id];
  }
  async function rotateCode() {
    busy = true;
    error = "";
    try {
      await invoke("rotate_code");
      peers = {};
      await startHost();
    } catch (e) {
      error = String(e);
      mode = "home";
    } finally {
      busy = false;
    }
  }
</script>

{#snippet diagPanel()}
  <details>
    <summary>Диагностика</summary>
    <button class="mini" onclick={refreshDiag}>Обновить</button>
    <button class="mini" onclick={checkUpdate}>Проверить обновления</button>
    {#if updateMsg}<p class="muted">{updateMsg}</p>{/if}
    <pre class="diag">{diagText || "нажми «Обновить»"}</pre>
  </details>
{/snippet}

<main>
  <h1>⛏ MineHost</h1>

  {#if error}<p class="error">{error}</p>{/if}

  {#if mode === "home"}
    <div class="col">
      <button class="big" disabled={busy} onclick={() => startHost()}>
        Хостить мир
        <small>Сначала открой мир: Esc → Open to LAN</small>
      </button>
      <details>
        <summary>У меня выделенный сервер</summary>
        <div class="join-box">
          <input placeholder="Порт сервера (например 25565)" bind:value={manualPort} />
          <button disabled={busy || !portValid(manualPort)} onclick={() => startHost(Number(manualPort))}>
            Хостить сервер на порту {manualPort || "…"}
          </button>
        </div>
        <div class="join-box">
          <p class="muted">…или пусть MineHost сам запустит server.jar:</p>
          <button class="mini" onclick={pickJar}>
            {jarPath ? jarPath.split(/[\\/]/).pop() : "Выбрать server.jar"}
          </button>
          <input placeholder="RAM, МБ (например 6144)" bind:value={ramMb} />
          <label class="muted">
            <input type="checkbox" bind:checked={eula} /> Принимаю Minecraft EULA
          </label>
          <button disabled={busy || !jarPath || !eula} onclick={startServerAndHost}>
            Запустить сервер и хостить
          </button>
          {#if serverLog}<p class="muted log">{serverLog}</p>{/if}
        </div>
      </details>
      <div class="join-box">
        <input placeholder="Твой ник" bind:value={playerName} />
        <input placeholder="Код приглашения (mh:…)" bind:value={joinCode} />
        <button class="big" disabled={busy || !joinCode.trim()} onclick={joinHost}>
          Подключиться к другу
        </button>
      </div>
      {#if recents.length}
        <h3>Недавние миры</h3>
        {#each recents as r (r.code)}
          <button
            class="recent"
            disabled={busy}
            onclick={() => {
              joinCode = r.code;
              joinHost();
            }}
          >
            ⟳ {r.world}
          </button>
        {/each}
      {/if}
      {#if busy}<p class="muted">Подключаемся…</p>{/if}
    </div>
  {:else if mode === "host"}
    <div class="col">
      <p>Отправь друзьям ссылку-приглашение:</p>
      <code class="invite">{inviteLink}</code>
      <button onclick={copyCode}>{copied ? "Скопировано ✓" : "Копировать ссылку"}</button>
      <h2>Игроки</h2>
      {#if Object.keys(peers).length === 0}
        <p class="muted">Пока никого — жди друзей</p>
      {/if}
      <ul>
        {#each Object.entries(peers) as [id, p] (id)}
          <li>
            {pathIcon(p.path)} {p.name} — {p.rtt_ms} ms
            <button class="mini danger" title="Выгнать" onclick={() => kickPeer(id)}>✕</button>
          </li>
        {/each}
      </ul>
      <button onclick={rotateCode} disabled={busy}>Новый код приглашения</button>
      <button class="danger" onclick={stopSession}>Остановить</button>
      {@render diagPanel()}
    </div>
  {:else}
    <div class="col">
      <p>{statusLine}</p>
      {#each Object.entries(peers) as [id, p] (id)}
        <p>{pathIcon(p.path)} до хоста: {p.rtt_ms} ms ({p.path})</p>
      {/each}
      <button class="danger" onclick={stopSession}>Отключиться</button>
      {@render diagPanel()}
    </div>
  {/if}
</main>

<style>
  :global(body) {
    margin: 0;
    font-family: system-ui, sans-serif;
    background: #1a1d23;
    color: #e8e8e8;
  }
  main {
    max-width: 440px;
    margin: 0 auto;
    padding: 24px;
  }
  h1 {
    text-align: center;
  }
  .col {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  button {
    padding: 10px 16px;
    border: none;
    border-radius: 8px;
    background: #3a7d44;
    color: white;
    font-size: 15px;
    cursor: pointer;
  }
  button:disabled {
    opacity: 0.5;
  }
  button.big {
    padding: 18px;
    font-size: 17px;
    display: flex;
    flex-direction: column;
    gap: 4px;
    align-items: center;
  }
  button.big small {
    opacity: 0.7;
    font-size: 12px;
  }
  button.danger {
    background: #8d3434;
  }
  .join-box {
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin-top: 16px;
  }
  input {
    padding: 10px;
    border-radius: 8px;
    border: 1px solid #444;
    background: #23272f;
    color: inherit;
    font-size: 14px;
  }
  code.invite {
    word-break: break-all;
    background: #23272f;
    padding: 12px;
    border-radius: 8px;
    font-size: 12px;
    user-select: all;
  }
  .error {
    color: #ff7b7b;
  }
  .muted {
    opacity: 0.6;
  }
  .mini {
    padding: 2px 8px;
    font-size: 12px;
  }
  .recent {
    background: #2a4d7a;
    text-align: left;
  }
  .log {
    font-family: monospace;
    font-size: 11px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  details {
    background: #20242b;
    border-radius: 8px;
    padding: 8px 12px;
  }
  summary {
    cursor: pointer;
    opacity: 0.8;
  }
  pre.diag {
    font-size: 11px;
    overflow-x: auto;
    background: #23272f;
    padding: 8px;
    border-radius: 8px;
    white-space: pre-wrap;
    word-break: break-all;
  }
  ul {
    list-style: none;
    padding: 0;
  }
</style>
