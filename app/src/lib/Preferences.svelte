<script lang="ts">
  import { onMount } from "svelte";
  import { api, type AppInfo } from "./api";
  import { prefs, CONTEXTS, ZOOMS, type Theme } from "./prefs.svelte";

  let { onClose }: { onClose: () => void } = $props();
  let el = $state<HTMLDivElement>();
  let info = $state.raw<AppInfo | null>(null);
  let infoError = $state("");
  let copyStatus = $state("");
  $effect(() => el?.focus());
  onMount(() => {
    api.appInfo().then((value) => (info = value)).catch((e) => (infoError = String(e)));
  });

  async function copyDiagnostics() {
    if (!info) return;
    try {
      await navigator.clipboard.writeText(`ParallaxCompare ${info.version}\nBuild: ${info.revision}\nOS: ${info.os} (${info.arch})\nTemporary directory: ${info.tempDir}`);
      copyStatus = "Diagnostics copied.";
    } catch (e) {
      copyStatus = `Could not copy diagnostics: ${String(e)}`;
    }
  }

  const themes: [Theme, string][] = [["system", "System"], ["light", "Light"], ["dark", "Dark"]];
  const set = (change: () => void) => {
    change();
    prefs.save();
  };
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="back" onclick={onClose}>
  <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
  <div class="sheet" role="dialog" aria-label="Preferences" tabindex="-1" bind:this={el} onclick={(e) => e.stopPropagation()}
    onkeydown={(e) => e.key === "Escape" && (e.preventDefault(), e.stopPropagation(), onClose())}>
    <header><h2>Preferences</h2><button onclick={onClose}>Close</button></header>

    <h3>Appearance</h3>
    <div class="f">
      <span>Theme</span>
      <span class="seg" role="group" aria-label="Theme">
        {#each themes as [id, label] (id)}
          <button class:on={prefs.theme === id} aria-pressed={prefs.theme === id} onclick={() => set(() => (prefs.theme = id))}>{label}</button>
        {/each}
      </span>
    </div>
    <div class="f">
      <span>Text size</span>
      <span class="size">
        <button onclick={() => prefs.step(-1)} disabled={prefs.zoom <= ZOOMS[0]} title="Smaller (Ctrl+−)" aria-label="Smaller">−</button>
        <b>{Math.round(prefs.zoom * 100)} %</b>
        <button onclick={() => prefs.step(1)} disabled={prefs.zoom >= ZOOMS[ZOOMS.length - 1]} title="Larger (Ctrl++)" aria-label="Larger">+</button>
        <button class="link" onclick={() => prefs.step(0)} disabled={prefs.zoom === 1} title="Back to 100 % (Ctrl+0)">Reset</button>
        <span class="dim"><kbd>Ctrl++</kbd> <kbd>Ctrl+−</kbd> <kbd>Ctrl+0</kbd></span>
      </span>
    </div>

    <h3>Comparing files</h3>
    <div class="f">
      <span>Context around differences</span>
      <span class="seg" role="group" aria-label="Context lines">
        {#each CONTEXTS as n (n)}
          <button class:on={prefs.context === n} aria-pressed={prefs.context === n} onclick={() => set(() => (prefs.context = n))}>{n === 0 ? "None" : `${n} lines`}</button>
        {/each}
      </span>
    </div>
    <p class="note">How many unchanged lines stay around each difference when a file view is filtered. A view can change it for itself, next to the filters.</p>

    <h3>About ParallaxCompare</h3>
    {#if info}
      <dl class="about">
        <dt>Version</dt><dd>{info.version}</dd>
        <dt>Build</dt><dd><code>{info.revision}</code></dd>
        <dt>System</dt><dd>{info.os} ({info.arch})</dd>
        <dt>Temporary directory</dt><dd>{info.tempDir}</dd>
      </dl>
      <button onclick={copyDiagnostics}>Copy diagnostics</button>
    {:else}
      <p class="note about-status">{infoError ? `Could not load application details: ${infoError}` : "Loading application details…"}</p>
    {/if}
    {#if copyStatus}<p class="about-status" role="status">{copyStatus}</p>{/if}

    <p class="foot">These apply to the whole app and are kept between runs. Compare rules (compare by, line endings, excluded files) are set per comparison under Options; the New comparison page remembers the ones last used.</p>
  </div>
</div>

<style>
  .back { position: fixed; inset: 0; z-index: 150; display: grid; place-items: start center; padding-top: 10vh; background: rgb(0 0 0 / 0.3); }
  .sheet { width: min(560px, 92vw); max-height: 80vh; overflow: auto; background: var(--panel); border: 1px solid var(--line); border-radius: 8px; box-shadow: 0 20px 60px rgb(0 0 0 / 0.35); padding: 14px 18px 16px; outline: none; }
  header { display: flex; justify-content: space-between; align-items: center; margin-bottom: 4px; }
  h2 { margin: 0; font-size: 15px; }
  h3 { margin: 14px 0 8px; font-size: 11px; letter-spacing: 0.06em; text-transform: uppercase; color: var(--dim); }
  .f { display: grid; grid-template-columns: 190px 1fr; align-items: center; gap: 10px; padding: 4px 0; font-size: 12.5px; }
  .f > span:first-child { color: var(--dim); }
  .seg { display: inline-flex; width: max-content; }
  .seg button { border-radius: 0; margin-left: -1px; padding: 3px 12px; }
  .seg button:first-child { border-radius: 4px 0 0 4px; margin-left: 0; }
  .seg button:last-child { border-radius: 0 4px 4px 0; }
  .seg button.on { background: var(--sel); border-color: var(--accent); position: relative; z-index: 1; }
  .size { display: inline-flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .size b { min-width: 48px; text-align: center; font-variant-numeric: tabular-nums; }
  .size button { min-width: 26px; }
  .link { border: none; background: none; color: var(--accent); padding: 0 4px; }
  .link:disabled { color: var(--faint); }
  .dim { color: var(--dim); font-size: 11px; }
  .note { margin: 2px 0 0 200px; font-size: 11.5px; color: var(--dim); max-width: 44ch; }
  .foot { margin: 16px 0 0; padding-top: 10px; border-top: 1px solid var(--line); font-size: 11.5px; color: var(--dim); }
  .about { display: grid; grid-template-columns: 140px minmax(0, 1fr); gap: 5px 10px; font-size: 12.5px; }
  .about dt { color: var(--dim); }
  .about dd { margin: 0; overflow-wrap: anywhere; }
  .about-status { margin: 8px 0; font-size: 12px; }
  @media (max-width: 560px) { .f { grid-template-columns: 1fr; } .note { margin-left: 0; } }
</style>
