import { api, sourceText, type SourceSpec, type SourcePreview } from "./api.ts";

export interface PickerValue {
  path: string;
  /** Preserve explicit revision/worktree intent, even if its text names a path. */
  spec?: SourceSpec;
}

export interface PickerPreview extends SourcePreview {
  input: string;
}

export function pickerFor(spec: SourceSpec): PickerValue {
  return { path: sourceText(spec), ...(spec.type !== "local" ? { spec } : {}) };
}

/** Also accepts the four-tab picker values stored by earlier versions. */
export function restorePicker(saved: unknown): PickerValue {
  if (!saved || typeof saved !== "object" || !("path" in saved) || typeof saved.path !== "string") return { path: "" };
  const old = saved as { path: string; kind?: string; rev?: string; subpath?: string; spec?: SourceSpec };
  if (old.kind === "gitRev")
    return pickerFor({ type: "gitRev", repo: old.path, rev: old.rev || "HEAD", subpath: old.subpath || "" });
  if (old.kind === "gitWorktree") return pickerFor({ type: "gitWorktree", repo: old.path });
  const spec = old.spec;
  if (
    spec &&
    ((spec.type === "gitRev" &&
      typeof spec.repo === "string" &&
      typeof spec.rev === "string" &&
      (spec.subpath == null || typeof spec.subpath === "string")) ||
      (spec.type === "gitWorktree" && typeof spec.repo === "string")) &&
    sourceText(spec) === old.path.trim()
  )
    return pickerFor(spec);
  return { path: old.path };
}

export function toSpec(value: PickerValue): Promise<SourceSpec> {
  return value.spec && sourceText(value.spec) === value.path.trim()
    ? Promise.resolve(value.spec)
    : api.parseSource(value.path.trim());
}

export function usable(preview: PickerPreview | null, value: PickerValue): boolean {
  return (
    !!preview && preview.input === value.path.trim() && !["empty", "error", "needsRepository"].includes(preview.kind)
  );
}

export function shapeMismatch(left: PickerPreview | null, right: PickerPreview | null): boolean {
  return left?.isFile != null && right?.isFile != null && left.isFile !== right.isFile;
}
