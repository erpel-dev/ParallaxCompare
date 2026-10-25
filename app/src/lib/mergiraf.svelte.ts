// The optional syntax-aware merge: mergiraf, installed separately. Whether to use
// it and where it is are remembered per user; finding it is checked once per path.

import { api, type MergirafInfo } from "./api";

const KEY = "parallax.mergiraf";

function load(): { enabled: boolean; path: string } {
  try {
    const saved = JSON.parse(localStorage.getItem(KEY) ?? "null");
    return { enabled: saved?.enabled !== false, path: typeof saved?.path === "string" ? saved.path : "" };
  } catch {
    return { enabled: true, path: "" };
  }
}

class MergirafSettings {
  /** Offer mergiraf's solutions when it is installed. */
  enabled = $state(load().enabled);
  /** The program; empty: PARALLAX_MERGIRAF, then PATH. */
  path = $state(load().path);
  /** The mergiraf found, or why none was. */
  found = $state<MergirafInfo | null>(null);
  problem = $state("");
  private checked: Promise<MergirafInfo | null> | null = null;
  private checkedPath = "";

  save() {
    try {
      localStorage.setItem(KEY, JSON.stringify({ enabled: this.enabled, path: this.path }));
    } catch {
      /* A convenience only. */
    }
  }

  /** The program to pass to the backend. */
  get program(): string | null {
    return this.path.trim() || null;
  }

  /** The installed mergiraf, looked up once per path; `null` when it is off or missing. */
  detect(again = false): Promise<MergirafInfo | null> {
    if (!this.enabled) return Promise.resolve(null);
    if (again || !this.checked || this.checkedPath !== this.path) {
      this.checkedPath = this.path;
      this.checked = api.mergirafInfo(this.program).then(
        (info) => {
          this.found = info;
          this.problem = "";
          return info;
        },
        (e) => {
          this.found = null;
          this.problem = String(e);
          return null;
        },
      );
    }
    return this.checked;
  }
}

export const mergiraf = new MergirafSettings();
