// Virtual scrolling over very many rows.
//
// The views draw only the visible rows inside a spacer as tall as all rows.
// WebKit stops painting content placed beyond about 16.7 million pixels (2^24,
// where its float coordinates lose precision), which a file of 900,000 lines
// passes. Taller content therefore gets a spacer of at most MAX_SCROLL_HEIGHT,
// and scroll positions are scaled: the views work in "row pixels" (row × row
// height) and convert at the edges with `scrollScale`.

export const MAX_SCROLL_HEIGHT = 8_000_000;

/** Row pixels per scroll pixel: 1 for content that fits, more for taller content. */
export function scrollScale(contentHeight: number, viewHeight: number): number {
  if (contentHeight <= MAX_SCROLL_HEIGHT) return 1;
  return (contentHeight - viewHeight) / Math.max(1, MAX_SCROLL_HEIGHT - viewHeight);
}

/** The spacer's height for content of `contentHeight` row pixels. */
export function spacerHeight(contentHeight: number): number {
  return Math.min(contentHeight, MAX_SCROLL_HEIGHT);
}

/**
 * Where to place the rendered rows, starting at row `first`, inside the spacer
 * so that they appear at their place on screen.
 */
export function windowOffset(scrollTop: number, scale: number, first: number, rowHeight: number): number {
  return scrollTop + first * rowHeight - scrollTop * scale;
}

const wheelRest = new WeakMap<HTMLElement, number>();

/**
 * Scrolls by a wheel event in row pixels when the content is scaled, so the
 * wheel moves as many lines as anywhere else. Returns whether it handled the event.
 */
export function scaledWheel(el: HTMLElement, scale: number, e: WheelEvent, rowHeight: number): boolean {
  if (scale <= 1 || !e.deltaY || e.shiftKey) return false;
  const unit = e.deltaMode === 1 ? rowHeight : e.deltaMode === 2 ? el.clientHeight : 1;
  // Fractions of a scroll pixel add up instead of getting lost.
  const want = (wheelRest.get(el) ?? 0) + (e.deltaY * unit) / scale;
  const step = Math.trunc(want);
  wheelRest.set(el, want - step);
  el.scrollTop += step;
  return true;
}
