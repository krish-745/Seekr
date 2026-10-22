import { Channel, invoke } from "@tauri-apps/api/core";
import { createEffect, createMemo, createSignal, For, Match, on, onMount, Show, Switch } from "solid-js";
import {
  applyBatches,
  firstVisible,
  scrollFor,
  visibleRange,
  type Batch,
  type Chip,
  type Item,
  type Preview,
  type Setup,
  type Shown,
} from "./results";
import "./App.css";

/** Row height in CSS px; must match `.row` in App.css. */
const ROW = 44;

function App() {
  let input!: HTMLInputElement;
  let list!: HTMLDivElement;
  const [items, setItems] = createSignal<Item[]>([]);
  const [chips, setChips] = createSignal<Chip[]>([]);
  const [query, setQuery] = createSignal("");
  const [selected, setSelected] = createSignal(0);
  const [scrollTop, setScrollTop] = createSignal(0);
  const [viewport, setViewport] = createSignal(0);
  const [actionsOpen, setActionsOpen] = createSignal(false);
  const [actionIndex, setActionIndex] = createSignal(0);
  // An item waiting for a second Enter before it goes to the Recycle Bin.
  const [confirming, setConfirming] = createSignal<Item | undefined>();

  const current = () => items()[selected()];
  // The content-search folder picker, shown on first run and from its command.
  const [setup, setSetup] = createSignal<Setup>();
  const [setupIndex, setSetupIndex] = createSignal(0);
  let firstRun = false;
  let askedSetup = false;

  async function openSetup(first: boolean) {
    firstRun = first;
    setActionsOpen(false);
    setSetupIndex(0);
    setSetup(await invoke<Setup>("content_setup"));
  }

  function toggleFolder(index: number) {
    setSetup((s) => s && { ...s, folders: s.folders.map((f, i) => (i === index ? { ...f, checked: !f.checked } : f)) });
  }

  async function addFolder() {
    const path = await invoke<string | null>("choose_folder");
    if (!path) return;
    setSetup((s) => {
      if (!s) return s;
      const at = s.folders.findIndex((f) => f.path.toLowerCase() === path.toLowerCase());
      const folders =
        at >= 0 ? s.folders.map((f, i) => (i === at ? { ...f, checked: true } : f)) : [...s.folders, { path, checked: true }];
      setSetupIndex(at >= 0 ? at : folders.length - 1);
      return { ...s, folders };
    });
  }

  /** Enter saves the ticked folders. Esc keeps what was saved before, or on first run records
   * that none were chosen, so the picker doesn't come back uninvited. */
  function closeSetup(save: boolean) {
    const s = setup();
    setSetup(undefined);
    if (s && (save || firstRun)) {
      const paths = save ? s.folders.filter((f) => f.checked).map((f) => f.path) : [];
      invoke("save_content_folders", { paths });
    }
    input.focus();
  }
  const [preview, setPreview] = createSignal<Preview | undefined>();
  const previewOf = <K extends Preview["kind"]>(kind: K) => {
    const p = preview();
    return p?.kind === kind ? (p as Extract<Preview, { kind: K }>) : undefined;
  };
  let previewTimer = 0;
  // Files and folders get a content preview, asked for once the selection settles.
  createEffect(
    on(current, (item) => {
      setPreview(undefined);
      clearTimeout(previewTimer);
      if (!item?.id.startsWith("path:")) return;
      const dark = matchMedia("(prefers-color-scheme: dark)").matches;
      previewTimer = setTimeout(() => {
        invoke<Preview>("preview", { path: item.id.slice(5), dark }).then((p) => {
          if (current()?.id === item.id) setPreview(p);
        });
      }, 60);
    }),
  );
  const range = createMemo(() => visibleRange(scrollTop(), viewport(), ROW, items().length));
  const rows = createMemo(() => {
    const [start, end] = range();
    return Array.from({ length: end - start }, (_, i) => start + i);
  });

  let shown: Shown = { id: 0, items: [], chips: [] };
  let nextId = 0;
  let queue: Batch[] = [];
  let frame = 0;
  const typedAt = new Map<number, number>();
  let firstPainted = 0;
  // The query whose file-name and file-content batches are waiting to be painted.
  let filesArrived = 0;
  let contentArrived = 0;

  function record(metric: string, t0: number) {
    // The callback runs at the start of the next frame, after this one is painted.
    requestAnimationFrame(() => invoke("record_latency", { metric, ms: performance.now() - t0 }));
  }

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
    setChips(shown.chips);
    const t0 = typedAt.get(shown.id);
    if (t0 !== undefined && firstPainted !== shown.id) {
      firstPainted = shown.id;
      record("keystroke", t0);
    }
    if (t0 !== undefined && filesArrived === shown.id) {
      filesArrived = 0;
      record("files", t0);
    }
    if (t0 !== undefined && contentArrived === shown.id) {
      contentArrived = 0;
      record("content", t0);
    }
    for (const id of typedAt.keys()) if (id < shown.id) typedAt.delete(id);
  }

  function search(text: string) {
    setQuery(text);
    const id = ++nextId;
    typedAt.set(id, performance.now());
    const onBatch = new Channel<Batch>();
    onBatch.onmessage = (batch) => {
      if (batch.source === "files") filesArrived = batch.id;
      if (batch.source === "content") contentArrived = batch.id;
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
    if (actionId === "setup") {
      openSetup(false);
      return;
    }
    if (actionId === "recycle" && confirming() !== item) {
      setActionsOpen(false);
      setConfirming(item);
      return;
    }
    setConfirming(undefined);
    if (actionId === "pin" || actionId === "unpin") {
      // Pinning keeps the panel open and re-ranks the current query.
      invoke("toggle_pin", { item: item.id }).then(() => search(input.value));
    } else {
      invoke("run_action", { item: item.id, action: actionId, query: input.value });
    }
  }

  function onKey(e: KeyboardEvent) {
    const picker = setup();
    if (picker) {
      const last = picker.folders.length - 1;
      if (e.key === "ArrowDown") setSetupIndex((i) => Math.min(i + 1, last));
      else if (e.key === "ArrowUp") setSetupIndex((i) => Math.max(i - 1, 0));
      else if (e.key === " ") toggleFolder(setupIndex());
      else if (e.ctrlKey && e.key === "o") addFolder();
      else if (e.key === "Enter") closeSetup(true);
      else if (e.key === "Escape") closeSetup(false);
      else return;
      e.preventDefault();
      return;
    }
    const actions = current()?.actions ?? [];
    const pending = confirming();
    if (pending) {
      // Enter confirms; any other key backs out.
      if (e.key === "Enter") run(pending, "recycle");
      else setConfirming(undefined);
      if (e.key === "Enter" || e.key === "Escape") e.preventDefault();
      return;
    }
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
      if (!askedSetup) {
        askedSetup = true;
        invoke<boolean>("content_setup_needed").then((needed) => {
          if (needed) openSetup(true);
        });
      }
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
      <Show when={setup()}>
        {(s) => (
          <section class="setup">
            <h2>Content search</h2>
            <p class="subtitle">
              Seekr can search inside the files in folders you choose, by words and by meaning. Nothing is read until you
              tick a folder.
            </p>
            <ul class="folders">
              <For each={s().folders}>
                {(f, i) => (
                  <li
                    classList={{ selected: i() === setupIndex() }}
                    onClick={() => {
                      setSetupIndex(i());
                      toggleFolder(i());
                    }}
                  >
                    <input type="checkbox" checked={f.checked} tabIndex={-1} />
                    <span>{f.path}</span>
                  </li>
                )}
              </For>
            </ul>
            <Show when={s().notes}>
              {(notes) => <p class="subtitle">Always included: your notes in {notes()}</p>}
            </Show>
          </section>
        )}
      </Show>
      <input
        ref={input}
        classList={{ hidden: !!setup() }}
        class="query"
        placeholder="Search apps, files and more"
        spellcheck={false}
        autofocus
        onInput={(e) => search(e.currentTarget.value)}
      />
      <Show when={!setup() && chips().length > 0}>
        <div class="chips">
          <For each={chips()}>
            {(chip) => (
              <span class="chip" classList={{ invalid: !chip.valid }} title={chip.valid ? "" : "Not understood, ignored"}>
                {chip.label}
              </span>
            )}
          </For>
        </div>
      </Show>
      <div class="body" classList={{ hidden: !!setup() }}>
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
                <Switch fallback={<pre>{item().preview}</pre>}>
                  <Match when={previewOf("image")}>
                    {(p) => <img class="thumb" src={p().url} alt="" onError={() => setPreview({ kind: "nothing" })} />}
                  </Match>
                  <Match when={previewOf("code")}>
                    {(p) => (
                      <pre class="code">
                        <For each={p().lines}>
                          {(line) => (
                            <div>
                              <For each={line}>{([color, text]) => <span style={{ color }}>{text}</span>}</For>
                            </div>
                          )}
                        </For>
                        <Show when={p().truncated}>
                          <div class="more">…</div>
                        </Show>
                      </pre>
                    )}
                  </Match>
                  <Match when={previewOf("markdown")}>
                    {/* Raw HTML was dropped in Rust; links stay inert so the pane never navigates. */}
                    {(p) => <div class="markdown" innerHTML={p().html} onClick={(e) => e.preventDefault()} />}
                  </Match>
                  <Match when={previewOf("folder")}>
                    {(p) => (
                      <ul class="listing">
                        <For each={p().entries}>
                          {(e) => <li classList={{ dir: e.dir }}>{e.name}</li>}
                        </For>
                        <Show when={p().total > p().entries.length}>
                          <li class="more">and {p().total - p().entries.length} more</li>
                        </Show>
                        <Show when={p().total === 0}>
                          <li class="more">Empty folder</li>
                        </Show>
                      </ul>
                    )}
                  </Match>
                  <Match when={previewOf("document")}>
                    {(p) => <pre>{p().text + (p().truncated ? "\n…" : "")}</pre>}
                  </Match>
                </Switch>
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
        <Show
          when={!setup()}
          fallback={
            <>
              <span><kbd>Space</kbd> Tick</span>
              <span><kbd>Ctrl+O</kbd> Add folder</span>
              <span><kbd>↵</kbd> Save</span>
              <span><kbd>Esc</kbd> {firstRun ? "Not now" : "Cancel"}</span>
            </>
          }
        >
          <Show
            when={confirming()}
            fallback={
              <>
                <Show when={current()?.actions[0]}>{(a) => <span><kbd>↵</kbd> {a().label}</span>}</Show>
                <Show when={current()?.actions[1]}>{(a) => <span><kbd>Ctrl+↵</kbd> {a().label}</span>}</Show>
                <Show when={(current()?.actions.length ?? 0) > 0}>
                  <span><kbd>Tab</kbd> Actions</span>
                </Show>
                <span><kbd>Esc</kbd> {actionsOpen() ? "Back" : "Close"}</span>
              </>
            }
          >
            {(item) => (
              <span class="confirm">
                <kbd>↵</kbd> Move {item().title} to the Recycle Bin · any other key cancels
              </span>
            )}
          </Show>
        </Show>
      </footer>
    </main>
  );
}

export default App;
