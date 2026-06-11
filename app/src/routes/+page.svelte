<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";

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

  async function startHost() {
    busy = true;
    error = "";
    try {
      inviteCode = await invoke<string>("start_host", { manualPort: null });
      mode = "host";
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
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
    peers = {};
    inviteCode = "";
    statusLine = "";
    mode = "home";
  }

  async function copyCode() {
    await navigator.clipboard.writeText(inviteCode);
    copied = true;
    setTimeout(() => (copied = false), 1500);
  }
</script>

<main>
  <h1>⛏ MineHost</h1>

  {#if error}<p class="error">{error}</p>{/if}

  {#if mode === "home"}
    <div class="col">
      <button class="big" disabled={busy} onclick={startHost}>
        Хостить мир
        <small>Сначала открой мир: Esc → Open to LAN</small>
      </button>
      <div class="join-box">
        <input placeholder="Твой ник" bind:value={playerName} />
        <input placeholder="Код приглашения (mh:…)" bind:value={joinCode} />
        <button class="big" disabled={busy || !joinCode.trim()} onclick={joinHost}>
          Подключиться к другу
        </button>
      </div>
      {#if busy}<p class="muted">Подключаемся…</p>{/if}
    </div>
  {:else if mode === "host"}
    <div class="col">
      <p>Отправь друзьям код приглашения:</p>
      <code class="invite">{inviteCode}</code>
      <button onclick={copyCode}>{copied ? "Скопировано ✓" : "Копировать код"}</button>
      <h2>Игроки</h2>
      {#if Object.keys(peers).length === 0}
        <p class="muted">Пока никого — жди друзей</p>
      {/if}
      <ul>
        {#each Object.entries(peers) as [id, p] (id)}
          <li>{pathIcon(p.path)} {p.name} — {p.rtt_ms} ms</li>
        {/each}
      </ul>
      <button class="danger" onclick={stopSession}>Остановить</button>
    </div>
  {:else}
    <div class="col">
      <p>{statusLine}</p>
      {#each Object.entries(peers) as [id, p] (id)}
        <p>{pathIcon(p.path)} до хоста: {p.rtt_ms} ms ({p.path})</p>
      {/each}
      <button class="danger" onclick={stopSession}>Отключиться</button>
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
  ul {
    list-style: none;
    padding: 0;
  }
</style>
