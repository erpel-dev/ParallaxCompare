<script lang="ts">
  import type { SourceInfo } from "./api";

  let { info, suffix = "" }: { info: SourceInfo; suffix?: string } = $props();

  const badge = $derived(
    { folder: ["DIR", "fs"], archive: ["ARC", "arc"], gitRev: ["GIT", "git"], gitWorktree: ["WT", "wt"] }[info.kind],
  );
</script>

<div class="bar" title={info.commitSummary ?? info.display}>
  <span class="badge {badge[1]}">{badge[0]}</span>
  <span class="path mono"><span dir="ltr">{info.display}{suffix ? ` › ${suffix}` : ""}</span></span>
  {#if info.rev}
    <span class="ref mono">⎇ {info.rev}{info.commit && info.commit !== info.rev ? ` · ${info.commit}` : ""}</span>
  {/if}
  {#if !info.writable}<span class="ro">read-only</span>{/if}
</div>

<style>
  .bar {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 5px 8px;
    background: var(--panel-2);
    min-width: 0;
  }
  .badge {
    font-size: 10.5px;
    font-weight: 600;
    padding: 0 6px;
    border-radius: 3px;
    color: #fff;
    background: #3d6e9e;
  }
  .badge.git {
    background: #b2502d;
  }
  .badge.arc {
    background: #6b5ab8;
  }
  .badge.wt {
    background: #3f7d4f;
  }
  .path {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    background: var(--input);
    border: 1px solid var(--line);
    padding: 1px 6px;
    border-radius: 3px;
    /* Long paths lose their beginning, not the interesting end. */
    direction: rtl;
    text-align: left;
  }
  .ref {
    color: #e6a974;
    white-space: nowrap;
  }
  .ro {
    color: var(--dim);
    font-size: 11px;
  }
</style>
