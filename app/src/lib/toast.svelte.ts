// Short notices in the corner that go away by themselves: errors after a while,
// information sooner. A new notice with the same text replaces the old one.

export interface Toast {
  id: number;
  text: string;
  kind: "error" | "info";
}

export const toasts = $state<Toast[]>([]);
let nextId = 1;
const timers = new Map<number, ReturnType<typeof setTimeout>>();

export function dismiss(id: number) {
  clearTimeout(timers.get(id));
  timers.delete(id);
  const index = toasts.findIndex((t) => t.id === id);
  if (index >= 0) toasts.splice(index, 1);
}

export function notify(text: string, kind: Toast["kind"] = "error") {
  for (const t of toasts.filter((t) => t.text === text)) dismiss(t.id);
  const id = nextId++;
  toasts.push({ id, text, kind });
  if (toasts.length > 4) dismiss(toasts[0].id);
  hold(id, false);
}

/** Starts (or, while the pointer is on the notice, stops) its countdown. */
export function hold(id: number, paused: boolean) {
  clearTimeout(timers.get(id));
  if (paused) return;
  const toast = toasts.find((t) => t.id === id);
  if (toast) timers.set(id, setTimeout(() => dismiss(id), toast.kind === "error" ? 10000 : 3500));
}

/** Errors about something that worked after all, e.g. a retried path, go away. */
export function clearErrors() {
  for (const t of toasts.filter((t) => t.kind === "error")) dismiss(t.id);
}
