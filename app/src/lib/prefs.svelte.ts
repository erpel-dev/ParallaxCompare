/** What the user set as their own defaults; kept in the web view's storage. */

export type Theme = "system" | "light" | "dark";

const KEY = "parallax.prefs.v1";
export const ZOOMS = [0.85, 0.9, 1, 1.1, 1.2, 1.35, 1.5];
export const CONTEXTS = [0, 3, 10];

class Prefs {
  theme = $state<Theme>("system");
  /** The scale of everything in the window; 1 is 100 %. */
  zoom = $state(1);
  /** Unchanged lines kept around each difference when a file view is filtered. */
  context = $state(3);

  constructor() {
    try {
      const saved = JSON.parse(localStorage.getItem(KEY) ?? "null");
      if (saved && typeof saved === "object") {
        if (["system", "light", "dark"].includes(saved.theme)) this.theme = saved.theme;
        if (typeof saved.zoom === "number" && saved.zoom >= 0.5 && saved.zoom <= 2) this.zoom = saved.zoom;
        if (typeof saved.context === "number" && saved.context >= 0 && saved.context <= 100) this.context = Math.round(saved.context);
      }
    } catch {
      // Unreadable settings: the defaults.
    }
  }

  save() {
    try {
      localStorage.setItem(KEY, JSON.stringify({ theme: this.theme, zoom: this.zoom, context: this.context }));
    } catch {
      // No storage: the settings last for this run.
    }
  }

  /** The next larger (1) or smaller (-1) text size; 0 resets it. */
  step(dir: 1 | -1 | 0) {
    if (dir === 0) this.zoom = 1;
    else {
      const at = ZOOMS.reduce((best, z, i) => (Math.abs(z - this.zoom) < Math.abs(ZOOMS[best] - this.zoom) ? i : best), 0);
      this.zoom = ZOOMS[Math.max(0, Math.min(ZOOMS.length - 1, at + dir))];
    }
    this.save();
  }
}

export const prefs = new Prefs();
