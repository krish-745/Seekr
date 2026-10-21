import { Channel, invoke } from "@tauri-apps/api/core";
import { createMemo, createSignal, For, onMount, Show } from "solid-js";
import {
  applyBatches,
  firstVisible,
  scrollFor,
  visibleRange,
  type Batch,
  type Item,
  type Shown,
} from "./results";
import "./App.css";

/** Row height in CSS px; must match `.row` in App.css. */
const ROW = 44;

function App() {
  let input!: HTMLInputElement;
  let list!: HTMLDivElement;
  const [items, setItems] = createSignal<Item[]>([]);
  const [query, setQuery] = createSignal("");
  const [selected, setSelected] = createSignal(0);
  const [scrollTop, setScrollTop] = createSignal(0);
  const [viewport, setViewport] = createSignal(0);
  const [actionsOpen, setActionsOpen] = createSignal(false);
  const [actionIndex, setActionIndex] = createSignal(0);

  const current = () => items()[selected()];
  const range = createMemo(() => visibleRange(scrollTop(), viewport(), ROW, items().length));
  const rows = createMemo(() => {
    const [start, end] = range();
    return Array.from({ length: end - start }, (_, i) => start + i);
  });

  let shown: Shown = { id: 0, items: [] };
  let nextId = 0;
  let queue: Batch[] = [];
  let frame = 0;
  const typedAt = new Map<number, number>();

  // Batches land at most once per frame, however fast providers answer.
  function flush() {
    frame = 0;
    const before = shown.id;
    shown = applyBatches(shown, queue);
    queue = [];
    if (shown.id !== before) {
      setActionsOpen(false);
      select(0);
    }
    setItems(shown.items);
    const t0 = typedAt.get(shown.id);
    for (const id of typedAt.keys()) if (id <= shown.id) typedAt.delete(id);
    if (t0 !== undefined) {
      // The callback runs at the start of the next frame, after this one is painted.
      requestAnimationFrame(() =>
        invoke("record_latency", { metric: "keystroke", ms: performance.now() - t0 }),
      );
    }
  }

  function search(text: string) {
    setQuery(text);
    const id = ++nextId;
    typedAt.set(id, performance.now());
    const onBatch = new Channel<Batch>();
    onBatch.onmessage = (batch) => {
      queue.push(batch);
      frame ||= requestAnimationFrame(flush);
    };
    invoke("query", { id, text, onBatch });
  }

  function select(index: number) {
    setSelected(index);
    if (list) list.scrollTop = scrollFor(index, list.scrollTop, list.clientHeight, ROW);
  }

  function run(item: Item | undefined, actionId: string | undefined) {
    if (!item || !actionId) return;
    if (actionId === "pin" || actionId === "unpin") {
      // Pinning keeps the panel open and re-ranks the current query.
      invoke("toggle_pin", { item: item.id }).then(() => search(input.value));
    } else {
      invoke("run_action", { item: item.id, action: actionId, query: input.value });
    }
  }

  function onKey(e: KeyboardEvent) {
    const actions = current()?.actions ?? [];
    if (actionsOpen()) {
      if (e.key === "ArrowDown") setActionIndex((i) => Math.min(i + 1, actions.length - 1));
      else if (e.key === "ArrowUp") setActionIndex((i) => Math.max(i - 1, 0));
      else if (e.key === "Enter") run(current(), actions[actionIndex()]?.id);
      else if (e.key === "Escape" || e.key === "Tab" || (e.ctrlKey && e.key === "k"))
        setActionsOpen(false);
      else return;
    } else if (e.key === "ArrowDown") select(Math.min(selected() + 1, items().length - 1));
    else if (e.key === "ArrowUp") select(Math.max(selected() - 1, 0));
    else if (e.key === "Enter") run(current(), actions[e.ctrlKey ? 1 : 0]?.id);
    else if (e.key === "Tab" || (e.ctrlKey && e.key === "k")) {
      if (actions.length) {
        setActionIndex(0);
        setActionsOpen(true);
      }
    } else if (e.altKey && /^Digit[1-9]$/.test(e.code)) {
      const target = items()[firstVisible(list.scrollTop, ROW) + Number(e.code.slice(5)) - 1];
      run(target, target?.actions[0]?.id);
    } else if (e.key === "Escape") invoke("hide");
    else return;
    e.preventDefault();
  }

  onMount(() => {
    // The window is reused, so refocus and select the last query every time it is shown.
    window.addEventListener("focus", () => {
      input.focus();
      input.select();
      invoke("panel_ready");
    });
    window.addEventListener("keydown", onKey);
    new ResizeObserver(() => setViewport(list.clientHeight)).observe(list);
    search("");
  });

  const numberOf = (index: number) => {
    const n = index - firstVisible(scrollTop(), ROW) + 1;
    return n >= 1 && n <= 9 ? `Alt+${n}` : "";
  };

  return (
    <main class="panel">
      <input
        ref={input}
        class="query"
        placeholder="Search apps, files and more"
        spellcheck={false}
        autofocus
        onInput={(e) => search(e.currentTarget.value)}
      />
      <div class="body">
        <div class="results" ref={list} onScroll={(e) => setScrollTop(e.currentTarget.scrollTop)}>
          <div class="rows" style={{ height: `${items().length * ROW}px` }}>
            <For each={rows()}>
              {(i) => (
                <div
                  class="row"
                  classList={{ selected: i === selected() }}
                  style={{ top: `${i * ROW}px` }}
                  onClick={() => select(i)}
                  onDblClick={() => run(items()[i], items()[i]?.actions[0]?.id)}
                >
                  <Show when={items()[i]?.icon} fallback={<span class="icon" />}>
                    {(src) => <img class="icon" src={src()} alt="" />}
                  </Show>
                  <div class="text">
                    <div class="title">{items()[i]?.title}</div>
                    <div class="subtitle">{items()[i]?.subtitle}</div>
                  </div>
                  <kbd class="number">{numberOf(i)}</kbd>
                </div>
              )}
            </For>
          </div>
          <Show when={items().length === 0}>
            <div class="empty">{query().trim() ? "No results" : "Type to search apps"}</div>
          </Show>
        </div>
        <aside class="preview">
          <Show when={current()}>
            {(item) => (
              <>
                <Show when={item().icon}>{(src) => <img class="preview-icon" src={src()} alt="" />}</Show>
                <h2>{item().title}</h2>
                <p class="subtitle">{item().subtitle}</p>
                <pre>{item().preview}</pre>
              </>
            )}
          </Show>
        </aside>
        <Show when={actionsOpen()}>
          <ul class="actions">
            <For each={current()?.actions ?? []}>
              {(action, i) => (
                <li classList={{ selected: i() === actionIndex() }} onClick={() => run(current(), action.id)}>
                  {action.label}
                </li>
              )}
            </For>
          </ul>
        </Show>
      </div>
      <footer class="hints">
        <Show when={current()?.actions[0]}>{(a) => <span><kbd>↵</kbd> {a().label}</span>}</Show>
        <Show when={current()?.actions[1]}>{(a) => <span><kbd>Ctrl+↵</kbd> {a().label}</span>}</Show>
        <Show when={(current()?.actions.length ?? 0) > 0}>
          <span><kbd>Tab</kbd> Actions</span>
        </Show>
        <span><kbd>Esc</kbd> {actionsOpen() ? "Back" : "Close"}</span>
      </footer>
    </main>
  );
}

export default App;
