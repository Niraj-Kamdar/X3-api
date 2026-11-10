<script>
  // Thin view: all logic lives in Rust commands. Keys are sent once and never
  // read back; only the last-4 hint is displayed.
  import { invoke } from "@tauri-apps/api/core";

  const NAMES = { elevenlabs: "ElevenLabs", cartesia: "Cartesia" };
  let providers = $state([]);
  let drafts = $state({});
  let budgets = $state({});
  let messages = $state({});

  async function refresh() {
    providers = await invoke("providers_status");
    for (const p of providers) budgets[p.provider] = p.monthly_budget_usd ?? "";
  }

  async function saveKey(provider) {
    const key = (drafts[provider] || "").trim();
    if (!key) return;
    try {
      await invoke("provider_set_key", { provider, key });
      drafts[provider] = "";
      messages[provider] = { ok: true, text: "Saved to Keychain." };
    } catch (e) {
      messages[provider] = { ok: false, text: String(e) };
    }
    await refresh();
  }

  async function removeKey(provider) {
    await invoke("provider_delete_key", { provider });
    messages[provider] = { ok: true, text: "Key removed." };
    await refresh();
  }

  async function test(provider) {
    messages[provider] = { ok: true, text: "Testing…" };
    const r = await invoke("provider_test", { provider });
    messages[provider] = { ok: r.ok, text: r.message };
  }

  async function saveBudget(provider) {
    const raw = String(budgets[provider] ?? "").trim();
    const monthlyUsd = raw === "" ? null : Number(raw);
    try {
      await invoke("provider_set_budget", { provider, monthlyUsd });
      messages[provider] = { ok: true, text: monthlyUsd === null ? "Budget removed." : "Budget saved." };
    } catch (e) {
      messages[provider] = { ok: false, text: String(e) };
    }
    await refresh();
  }

  refresh();
</script>

<p class="muted">
  Keys are stored in the macOS Keychain and never shown again. Agents use local tokens
  (Server tab) and never see these keys.
</p>

{#each providers as p (p.provider)}
  <div class="card">
    <h3>{NAMES[p.provider] ?? p.provider}</h3>
    <div class="row">
      {#if p.has_key}
        <span class="ok">Key saved {p.hint}</span>
        <button onclick={() => test(p.provider)}>Test</button>
        <button onclick={() => removeKey(p.provider)}>Remove</button>
      {:else}
        <span class="muted">No key</span>
      {/if}
    </div>
    <div class="row">
      <input
        type="password"
        autocomplete="off"
        placeholder={p.has_key ? "Replace key" : "Paste API key"}
        bind:value={drafts[p.provider]}
      />
      <button onclick={() => saveKey(p.provider)}>{p.has_key ? "Replace" : "Add"}</button>
    </div>
    <div class="row">
      <label>Monthly budget (USD)
        <input type="number" min="0" step="0.5" placeholder="no limit" bind:value={budgets[p.provider]} />
      </label>
      <button onclick={() => saveBudget(p.provider)}>Set</button>
    </div>
    {#if messages[p.provider]}
      <div class={messages[p.provider].ok ? "ok" : "bad"}>{messages[p.provider].text}</div>
    {/if}
  </div>
{/each}
