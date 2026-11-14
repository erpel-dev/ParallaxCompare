export function formatSize(bytes: number): string {
  if (bytes < 10_000) return bytes.toLocaleString();
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit++;
  }
  return `${value.toFixed(value < 10 ? 1 : 0)} ${units[unit]}`;
}

export function formatTime(seconds: number | null): string {
  if (seconds === null) return "";
  const d = new Date(seconds * 1000);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

export function baseName(path: string): string {
  const parts = path.split(/[\\/]/).filter(Boolean);
  return parts[parts.length - 1] ?? path;
}

/** Control characters in a name as their visible symbols (␊ for a line break, ␉ for a tab), so different names never look alike. */
export function shown(name: string): string {
  return name.replace(/[\u0000-\u001f\u007f]/g, (c) => String.fromCharCode(c === "\u007f" ? 0x2421 : 0x2400 + c.charCodeAt(0)));
}
