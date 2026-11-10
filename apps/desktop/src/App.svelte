<script>
  import Providers from "./lib/Providers.svelte";
  import Server from "./lib/Server.svelte";
  let tab = $state("providers");
</script>

<main>
  <nav>
    <button class:active={tab === "providers"} onclick={() => (tab = "providers")}>Providers</button>
    <button class:active={tab === "server"} onclick={() => (tab = "server")}>Server</button>
  </nav>
  {#if tab === "providers"}
    <Providers />
  {:else}
    <Server />
  {/if}
</main>
