/** Widths (px) the top-bar breadcrumb needs to decide how much to collapse. */
export interface CrumbMetrics {
  /** Space the breadcrumb nav has. */
  available: number;
  /** Rendered width of each ancestor crumb (everything but the current page). */
  ancestors: number[];
  /** Unclipped width of the current-page crumb. */
  current: number;
  /** Width of one "/" separator including the gaps on both sides. */
  separator: number;
  /** Width of the "…" crumb that stands in for hidden ancestors. */
  ellipsis: number;
}

/** The current page keeps at least this much (or its full width if shorter) before ancestors collapse. */
export const MIN_CURRENT_CRUMB = 120;

/** Below this width the current crumb would only be a sliver, so the breadcrumb is dropped. */
export const MIN_VISIBLE_CRUMB = 40;

/**
 * How many leading ancestors to hide behind "…" so the ancestors that stay
 * visible keep their full text and the current crumb keeps a readable width
 * (it takes the ellipsis, never the ancestors). Hides as few as possible;
 * when every ancestor goes, the "…" goes too and only the current page remains.
 * Returns `ancestors.length + 1` when not even that fits: show nothing.
 */
export function crumbsToHide({ available, ancestors, current, separator, ellipsis }: CrumbMetrics): number {
  const need = Math.min(current, MIN_CURRENT_CRUMB);
  for (let hidden = 0; hidden < ancestors.length; hidden++) {
    let width = need;
    for (const w of ancestors.slice(hidden)) width += w + separator;
    if (hidden > 0) width += ellipsis + separator;
    if (width <= available) return hidden;
  }
  return available >= Math.min(current, MIN_VISIBLE_CRUMB) ? ancestors.length : ancestors.length + 1;
}
