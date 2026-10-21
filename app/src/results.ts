export type Action = { id: string; label: string };
export type Item = {
  id: string;
  title: string;
  subtitle: string;
  preview: string;
  icon: string | null;
  actions: Action[];
};
export type Batch = { id: number; items: Item[] };
export type Shown = { id: number; items: Item[] };

/**
 * Applies batches in arrival order. The first batch of a newer query replaces the list;
 * later batches of the same query append, so rows above the highlight never move.
 * Batches from older queries are dropped.
 */
export function applyBatches(shown: Shown, batches: Batch[]): Shown {
  let { id, items } = shown;
  for (const b of batches) {
    if (b.id < id) continue;
    items = b.id > id ? b.items : [...items, ...b.items];
    id = b.id;
  }
  return { id, items };
}

/** Rows to render for a fixed row height: [start, end), with a few spare rows either side. */
export function visibleRange(
  scrollTop: number,
  viewport: number,
  rowHeight: number,
  count: number,
  overscan = 4,
): [number, number] {
  const start = Math.max(0, Math.floor(scrollTop / rowHeight) - overscan);
  const end = Math.min(count, Math.ceil((scrollTop + viewport) / rowHeight) + overscan);
  return [start, Math.max(start, end)];
}

/** The scrollTop that brings row `index` fully into view, moving as little as possible. */
export function scrollFor(
  index: number,
  scrollTop: number,
  viewport: number,
  rowHeight: number,
): number {
  const top = index * rowHeight;
  if (top < scrollTop) return top;
  if (top + rowHeight > scrollTop + viewport) return top + rowHeight - viewport;
  return scrollTop;
}

/** Index of the first fully visible row, which Alt+1 activates. */
export function firstVisible(scrollTop: number, rowHeight: number): number {
  return Math.ceil(scrollTop / rowHeight);
}
