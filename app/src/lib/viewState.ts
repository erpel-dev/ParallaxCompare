// How a comparison is shown, handed to the comparison that replaces it when one
// side is switched (another version, another folder), so the view stays put.
import type { CompareOptions } from "./api";
import type { Filter } from "./tree";

export interface FolderViewState {
  /** Open folders. */
  expanded: string[];
  selected: string | null;
  filter: Filter;
  flat: boolean;
  scrollTop: number;
  options: CompareOptions;
}

export interface TextViewState {
  scrollTop: number;
  /** The selected row. */
  selected: number | null;
  /** The chosen view (text, a converted view), when it still applies. */
  view: string | null;
}

export type ViewState = { kind: "folder"; state: FolderViewState } | { kind: "text"; state: TextViewState };
