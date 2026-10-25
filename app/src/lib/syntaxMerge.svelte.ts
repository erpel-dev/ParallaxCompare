// The syntax-aware merge: built in (weave), mergiraf (installed separately) or
// off. The choice and mergiraf's path are remembered per user; the engine is
// looked up once per choice.

import { api, type SyntaxEngine } from "./api";

const KEY = "parallax.syntaxMerge";
/** Where the setting was kept while mergiraf was the only engine. */
const OLD_KEY = "parallax.mergiraf";

export type EngineChoice = "builtin" | "mergiraf" | "off";

function load(): { engine: EngineChoice; path: string } {
  try {
    const saved = JSON.parse(localStorage.getItem(KEY) ?? "null");
    if (saved && ["builtin", "mergiraf", "off"].includes(saved.engine)) {
      return { engine: saved.engine, path: typeof saved.path === "string" ? saved.path : "" };
    }
    const old = JSON.parse(localStorage.getItem(OLD_KEY) ?? "null");
    return { engine: old?.enabled === false ? "off" : "builtin", path: typeof old?.path === "string" ? old.path : "" };
  } catch {
    return { engine: "builtin", path: "" };
  }
}

class SyntaxMergeSettings {
  engine = $state<EngineChoice>(load().engine);
  /** The mergiraf program; empty: PARALLAX_MERGIRAF, then PATH. */
  path = $state(load().path);
  /** The engine found, or why none was. */
  found = $state<SyntaxEngine | null>(null);
  problem = $state("");
  private checked: Promise<SyntaxEngine | null> | null = null;
  private checkedFor = "";

  get enabled(): boolean {
    return this.engine !== "off";
  }

  /** The mergiraf program to pass to the backend. */
  get program(): string | null {
    return this.path.trim() || null;
  }

  save() {
    try {
      localStorage.setItem(KEY, JSON.stringify({ engine: this.engine, path: this.path }));
    } catch {
      /* A convenience only. */
    }
  }

  /** The engine to use, looked up once per choice; `null` when off or not found. */
  detect(again = false): Promise<SyntaxEngine | null> {
    if (!this.enabled) return Promise.resolve(null);
    const key = `${this.engine}\n${this.path}`;
    if (again || !this.checked || this.checkedFor !== key) {
      this.checkedFor = key;
      this.found = null;
      this.checked = api.syntaxEngine(this.engine, this.program).then(
        (info) => {
          this.found = info;
          this.problem = "";
          return info;
        },
        (e) => {
          this.problem = String(e);
          return null;
        },
      );
    }
    return this.checked;
  }
}

export const syntaxMerge = new SyntaxMergeSettings();
