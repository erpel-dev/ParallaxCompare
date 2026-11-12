<script lang="ts" module>
  /** The keys of each view, grouped the same way in all of them. */
  export type ShortcutView = "folder" | "text" | "merge" | "other";

  const shared: [string, string][] = [
    ["Command palette", "Ctrl+K · F1"],
    ["This list", "?"],
    ["Preferences", "Ctrl+,"],
    ["Larger / smaller text", "Ctrl++ · Ctrl+−"],
    ["New comparison", "Ctrl+T"],
    ["Close tab", "Ctrl+W"],
    ["Next / previous tab", "Ctrl+Tab · Ctrl+Shift+Tab"],
  ];

  export const SHORTCUTS: Record<ShortcutView, { title: string; groups: [string, [string, string][]][] }> = {
    folder: {
      title: "Folder comparison",
      groups: [
        ["Move", [["Previous / next difference", "F7 · F8"], ["Up / down", "↑ · ↓"], ["Open or close folder", "→ · ←"], ["Open comparison", "Enter"], ["Compare again", "F5"]]],
        ["Select", [["Select more", "Ctrl+click · Shift+click"], ["Select all", "Ctrl+A"], ["Clear the selection", "Esc"], ["Menu of the row", "Shift+F10"]]],
        ["Change", [["Delete on the selected side", "Delete"], ["Switch the version of a side", "Alt+V"]]],
      ],
    },
    text: {
      title: "File comparison",
      groups: [
        ["Move", [["Previous / next difference", "F7 · F8"], ["Find", "Ctrl+F"], ["Next / previous match", "F3 · Shift+F3"], ["Next / previous pull request thread", "F9 · Shift+F9"]]],
        ["Change", [["Copy to left / right", "Alt+← · Alt+→"], ["Edit the line", "F2 · Enter · typing"], ["Undo / redo", "Ctrl+Z · Ctrl+Y"], ["Save", "Ctrl+S"]]],
        ["Versions", [["Switch the version of a side", "Alt+V"], ["Compare with HEAD", "Alt+H"]]],
      ],
    },
    merge: {
      title: "Merge",
      groups: [
        ["Move", [["Previous / next conflict", "F7 · F8"]]],
        ["Resolve", [["Take local / remote", "Alt+← · Alt+→"], ["Take both", "Alt+↓"], ["Keep the base", "Alt+↑"], ["Syntax-aware solution", "Alt+S"], ["Edit a result line", "click"]]],
        ["Change", [["Undo / redo", "Ctrl+Z · Ctrl+Y"], ["Save the result", "Ctrl+S"]]],
      ],
    },
    other: { title: "Everywhere", groups: [] },
  };
</script>

<script lang="ts">
  let { view, onClose }: { view: ShortcutView; onClose: () => void } = $props();
  const sheet = $derived(SHORTCUTS[view]);
  let el = $state<HTMLDivElement>();
  $effect(() => el?.focus());
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="back" onclick={onClose}>
  <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
  <div class="sheet" role="dialog" aria-label="Keyboard shortcuts" tabindex="-1" bind:this={el} onclick={(e) => e.stopPropagation()}
    onkeydown={(e) => (e.key === "Escape" || e.key === "?") && (e.preventDefault(), e.stopPropagation(), onClose())}>
    <header><h2>Keys · {sheet.title}</h2><button onclick={onClose}>Close</button></header>
    <div class="groups">
      {#each [...sheet.groups, ["App", shared] as [string, [string, string][]]] as [name, keys] (name)}
        <section>
          <h3>{name}</h3>
          {#each keys as [what, k] (what)}
            <div class="k"><span>{what}</span><span class="keys">{#each k.split(" · ") as one, i (i)}{#if i}<span class="or"> or </span>{/if}<kbd>{one}</kbd>{/each}</span></div>
          {/each}
        </section>
      {/each}
    </div>
  </div>
</div>

<style>
  .back { position: fixed; inset: 0; z-index: 150; display: grid; place-items: start center; padding-top: 10vh; background: rgb(0 0 0 / 0.3); }
  .sheet { width: min(720px, 92vw); max-height: 76vh; overflow: auto; background: var(--panel); border: 1px solid var(--line); border-radius: 8px; box-shadow: 0 20px 60px rgb(0 0 0 / 0.35); padding: 14px 18px 18px; outline: none; }
  header { display: flex; justify-content: space-between; align-items: center; margin-bottom: 8px; }
  h2 { margin: 0; font-size: 15px; }
  .groups { display: grid; grid-template-columns: repeat(auto-fit, minmax(300px, 1fr)); gap: 14px 28px; }
  h3 { margin: 4px 0 6px; font-size: 11px; letter-spacing: 0.06em; text-transform: uppercase; color: var(--dim); }
  .k { display: flex; justify-content: space-between; gap: 12px; padding: 3px 0; font-size: 12.5px; border-bottom: 1px solid color-mix(in srgb, var(--line) 50%, transparent); }
  .keys { white-space: nowrap; }
  .or { color: var(--dim); font-size: 11px; }
  kbd { font-family: var(--mono); font-size: 11px; border: 1px solid var(--line); border-bottom-width: 2px; border-radius: 3px; padding: 0 5px; background: var(--panel-2); }
</style>
