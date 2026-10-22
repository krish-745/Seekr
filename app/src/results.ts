export type Action = { id: string; label: string };
export type Item = {
  id: string;
  title: string;
  subtitle: string;
  preview: string;
  icon: string | null;
  actions: Action[];
  /** For a content match: the line its passage starts on. */
  line?: number;
};
export type Chip = { label: string; valid: boolean };
/** The content-search folder picker: suggestions and chosen folders, plus the notes folder. */
export type Setup = { folders: { path: string; checked: boolean }[]; notes: string | null };
/** What the preview pane shows for a file or folder; `code` lines are [color, text] runs. */
export type Preview =
  | { kind: "image"; url: string }
  | { kind: "code"; lines: [string, string][][]; truncated: boolean; first: number; focus?: number }
  | { kind: "markdown"; html: string }
  | { kind: "folder"; entries: { name: string; dir: boolean }[]; total: number }
  | { kind: "document"; text: string; truncated: boolean }
  | { kind: "nothing" };
export type Batch = { id: number; source: "apps" | "files" | "content" | "web"; items: Item[]; chips: Chip[] };
export type Shown = { id: number; items: Item[]; chips: Chip[] };

/**
 * Applies batches in arrival order. The first batch of a newer query replaces the list;
 * later batches of the same query append, so rows above the highlight never move.
 * Batches from older queries are dropped. Filter chips come with a query's first batch.
 */
export function applyBatches(shown: Shown, batches: Batch[]): Shown {
  let { id, items, chips } = shown;
  for (const b of batches) {
    if (b.id < id) continue;
    if (b.id > id) {
      items = b.items;
      chips = b.chips;
    } else items = [...items, ...b.items];
    id = b.id;
  }
  return { id, items, chips };
}

/**
 * Splits `text` around case-insensitive occurrences of the query's words (three letters or more),
 * so the preview can mark them.
 */
export function marks(text: string, query: string): { text: string; hit: boolean }[] {
  const words = query
    .toLowerCase()
    .split(/\s+/)
    .map((w) => w.replace(/^[-"']+|"+$/g, ""))
    .filter((w) => w.length >= 3 && !w.includes(":"));
  if (!words.length) return [{ text, hit: false }];
  const pattern = new RegExp(`(${words.map((w) => w.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")).join("|")})`, "gi");
  return text
    .split(pattern)
    .filter((part) => part !== "")
    .map((part) => ({ text: part, hit: words.includes(part.toLowerCase()) }));
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
