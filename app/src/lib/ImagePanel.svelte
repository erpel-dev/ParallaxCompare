<script lang="ts">
  import type { ImageView } from "./api";

  let { image }: { image: ImageView } = $props();

  type Mode = "side" | "overlay" | "blink";
  let mode = $state<Mode>("side");
  let showMask = $state(true);
  let fit = $state(true);
  let blinkRight = $state(false);

  $effect(() => {
    if (mode !== "blink") return;
    const timer = setInterval(() => (blinkRight = !blinkRight), 700);
    return () => clearInterval(timer);
  });

  const share = $derived(image.totalPixels ? (image.differentPixels / image.totalPixels) * 100 : 0);
</script>

<div class="panel">
  <div class="bar">
    {#each [["side", "Side by side"], ["overlay", "Difference"], ["blink", "Blink"]] as const as [id, label] (id)}
      <button class:on={mode === id} onclick={() => (mode = id)} disabled={id !== "side" && (!image.left || !image.right)}>{label}</button>
    {/each}
    <span class="sep"></span>
    <label title={mode === "side" ? "Outline the areas with differing pixels" : "Mark each differing pixel"}><input type="checkbox" bind:checked={showMask} disabled={!image.mask} /> {mode === "side" ? "Outline differences" : "Highlight differing pixels"}</label>
    <label><input type="checkbox" bind:checked={fit} /> Fit to window</label>
    <span class="summary">
      {#if image.totalPixels}
        {#if image.differentPixels === 0}
          All {image.totalPixels.toLocaleString()} pixels are identical.
        {:else}
          {image.differentPixels.toLocaleString()} of {image.totalPixels.toLocaleString()} pixels differ ({share < 0.01 ? "<0.01" : share.toFixed(2)} %)
          {#if image.bounds}· area {image.bounds[0]},{image.bounds[1]} {image.bounds[2]}×{image.bounds[3]}{/if}
        {/if}
      {/if}
    </span>
  </div>

  {#if mode === "side"}
    <div class="sides">
      {#each [image.left, image.right] as side, i (i)}
        <div class="cell">
          {#if side}
            <div class="meta">{side.format}{side.width ? ` · ${side.width}×${side.height}` : ""}</div>
            <div class="stack" class:fit>
              <img src={side.url} alt={i === 0 ? "left image" : "right image"} />
              {#if showMask && image.outline !== null && side.width}
                <!-- The differing areas outlined: the image stays visible however much differs. -->
                <svg class="outline" viewBox="0 0 {side.width} {side.height}" preserveAspectRatio="none" aria-hidden="true">
                  <path d={image.area} class="area" />
                  <path d={image.outline} class="edge" />
                </svg>
              {:else if showMask && image.mask}<img class="mask" src={image.mask} alt="" />{/if}
            </div>
          {:else}
            <div class="missing">missing on this side</div>
          {/if}
        </div>
      {/each}
    </div>
  {:else if image.left && image.right}
    <div class="single">
      <div class="meta">
        {mode === "blink" ? (blinkRight ? "Right" : "Left") : "Left, with the differing pixels of the right image highlighted"}
      </div>
      <div class="stack" class:fit>
        <img src={mode === "blink" && blinkRight ? image.right.url : image.left.url} alt="compared" class:dim={mode === "overlay"} />
        {#if mode === "overlay" && image.mask}<img class="mask strong" src={image.mask} alt="" />{/if}
      </div>
    </div>
  {/if}
</div>

<style>
  .panel {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-height: 0;
  }
  .bar {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: center;
    padding: 6px 8px;
    border-bottom: 1px solid var(--line);
    background: var(--panel-2);
  }
  .bar label {
    display: flex;
    gap: 4px;
    align-items: center;
  }
  .summary {
    margin-left: auto;
    color: var(--dim);
  }
  .sides {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 1px;
    background: var(--line);
    flex: 1;
    min-height: 0;
  }
  .cell,
  .single {
    background: var(--bg);
    overflow: auto;
    padding: 8px;
    min-height: 0;
  }
  .single {
    flex: 1;
  }
  .meta {
    color: var(--dim);
    font-size: 12px;
    margin-bottom: 6px;
  }
  .stack {
    position: relative;
    display: inline-block;
    /* A checkerboard shows transparency. */
    background: repeating-conic-gradient(#8884 0% 25%, transparent 0% 50%) 0 0 / 16px 16px;
  }
  .stack img {
    display: block;
    image-rendering: pixelated;
  }
  .stack.fit img {
    max-width: 100%;
    max-height: calc(100vh - 260px);
  }
  .stack .mask {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    opacity: 0.55;
    pointer-events: none;
  }
  .stack .outline {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    pointer-events: none;
    overflow: visible;
  }
  .outline .area {
    fill: #ff005a;
    fill-opacity: 0.1;
  }
  .outline .edge {
    fill: none;
    stroke: #ff005a;
    stroke-width: 2px;
    vector-effect: non-scaling-stroke;
  }
  .stack .mask.strong {
    opacity: 0.9;
  }
  .dim {
    opacity: 0.35;
  }
  .missing {
    color: var(--dim);
    padding: 40px;
    text-align: center;
  }
</style>
