<script>
  import { invoke } from "@tauri-apps/api/core";

  let status = $state(null);
  let port = $state("");
  let tokens = $state([]);
  let label = $state("Claude Code");
  let cap = $state("");
  let minted = $state(null);
  let error = $state("");
  let usage = $state([]);

  async function refresh() {
    status = await invoke("server_status");
    port = String(status.port);
    tokens = await invoke("tokens_list");
    usage = await invoke("usage_recent");
  }

  async function run(fn) {
    error = "";
    try { await fn(); } catch (e) { error = String(e); }
    await refresh();
  }

  const toggle = () => run(() => invoke(status.running ? "server_stop" : "server_start"));
  const savePort = () => run(() => invoke("server_set_port", { port: Number(port) }));
  const revoke = (id) => run(() => invoke("token_revoke", { id }));
  const mint = () =>
    run(async () => {
      const spendCapUsd = String(cap).trim() === "" ? null : Number(cap);
      minted = await invoke("token_mint", { label, providers: null, tasks: null, spendCapUsd });
    });

  async function copy(text) {
    await navigator.clipboard.writeText(text);
  }

  refresh();
</script>

{#if status}
  <div class="card">
    <h3>Server</h3>
    <div class="row">
      <span class={status.running ? "ok" : "bad"}>{status.running ? "Running" : "Stopped"}</span>
      <span class="muted">{status.url} (localhost only)</span>
      <button onclick={toggle}>{status.running ? "Stop" : "Start"}</button>
    </div>
    <div class="row">
      <label>Port <input type="number" min="1024" max="65535" bind:value={port} /></label>
      <button onclick={savePort}>Apply</button>
    </div>
  </div>
{/if}

<div class="card">
  <h3>Tokens</h3>
  <div class="row">
    <input placeholder="Label" bind:value={label} />
    <input type="number" min="0" step="0.5" placeholder="Spend cap USD (optional)" bind:value={cap} />
    <button onclick={mint}>Mint token</button>
  </div>
  {#if minted}
    <p><strong>Copy this now; it will not be shown again.</strong></p>
    <p>Add to Claude Code's MCP config (the <code>x3</code> CLI must be on your PATH):</p>
    <pre>{minted.mcp_config}</pre>
    <div class="row">
      <button onclick={() => copy(minted.mcp_config)}>Copy MCP config</button>
      <button onclick={() => copy(minted.token)}>Copy token only</button>
      <button onclick={() => (minted = null)}>Done</button>
    </div>
  {/if}
  <table>
    <thead><tr><th>Label</th><th>Token</th><th>Cap</th><th>Last used</th><th></th></tr></thead>
    <tbody>
      {#each tokens as t (t.id)}
        <tr class:muted={t.revoked_at}>
          <td>{t.label}</td>
          <td><code>{t.display_prefix}…</code></td>
          <td>{t.spend_cap_usd ?? "—"}</td>
          <td>{t.last_used_at ?? "never"}</td>
          <td>
            {#if t.revoked_at}revoked{:else}<button onclick={() => revoke(t.id)}>Revoke</button>{/if}
          </td>
        </tr>
      {/each}
    </tbody>
  </table>
</div>

<div class="card">
  <h3>Recent calls</h3>
  <table>
    <thead><tr><th>Time</th><th>Token</th><th>Model</th><th>Units</th><th>Est. cost</th><th>Status</th></tr></thead>
    <tbody>
      {#each usage as u}
        <tr>
          <td>{u.ts.slice(0, 19).replace("T", " ")}</td>
          <td>{u.token_label}</td>
          <td>{u.model}</td>
          <td>{Math.round(u.units)} {u.unit}</td>
          <td>${u.est_cost_usd.toFixed(4)}</td>
          <td>{u.status}</td>
        </tr>
      {/each}
    </tbody>
  </table>
</div>

{#if error}<p class="bad">{error}</p>{/if}
