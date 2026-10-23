import { Channel, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { createEffect, createMemo, createSignal, For, Match, on, onMount, Show, Switch } from "solid-js";
import {
  applyBatches,
  firstVisible,
  marks,
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

/** Actions that change an entry in place and leave the panel open. */
const STAY_OPEN = new Set([
  "clippin",
  "clipunpin",
  "clipdelete",
  "winminimize",
  "winclose",
  "endtask",
  "tododone",
  "todoundo",
  "tododelete",
]);
/** Actions that need a second Enter, and what the hint asks. */
const CONFIRM: Record<string, (item: Item) => string> = {
  recycle: (item) => `Move ${item.title} to the Recycle Bin`,
  restart: () => "Restart Windows now",
  shutdown: () => "Shut down now",
  signout: () => "Sign out of Windows now",
  emptybin: () => "Delete everything in the Recycle Bin for good",
  killfrozen: () => "End every app that isn't responding",
  endtask: (item) => `End ${item.title}`,
};
/** Commands that open a list by typing its prefix, and the setting that holds the prefix. */
const PREFIXES: Record<string, string> = {
  cliphistory: "prefixes.clipboard",
  switchwindows: "prefixes.windows",
  emojimode: "prefixes.emoji",
  notesmode: "prefixes.note",
  todosmode: "prefixes.todo",
};

/** Until the settings arrive. */
const DEFAULT_PREFIXES: Record<string, string> = {
  "prefixes.clipboard": "clip",
  "prefixes.windows": "win",
  "prefixes.emoji": "emoji",
  "prefixes.note": "note",
  "prefixes.todo": "todo",
};

/** One setting; `kind` is text, number, toggle, choice, lines or action. */
type Field = {
  key: string;
  group: string;
  label: string;
  kind: string;
  value: string | number | boolean;
  choices?: string[];
  hint: string;
};
type SettingsView = { fields: Field[]; problem: string | null; path: string; css: number | null };
type SettingsState = {
  view: SettingsView;
  index: number;
  draft?: string;
  error?: string;
  /** What the last maintenance action said, or the question before a rebuild. */
  notice?: string;
  /** A rebuild waiting for a second Enter. */
  confirm?: string;
};
/** Maintenance actions that ask for a second Enter. */
const REBUILDS = new Set(["rebuild_content", "rebuild_files"]);
type Latency = { metric: string; n: number; p50: number; p95: number; max: number; buckets: number[] };
type Diagnostics = {
  sections: { title: string; rows: { label: string; value: string }[] }[];
  latency: Latency[];
  quarantined: [string, string][];
  log: string[];
  logs: string;
};
const BUCKET_LABELS = ["≤5", "≤10", "≤16", "≤30", "≤50", "≤100", ">100"];

type Source = { n: number; path: string; line: number; name: string };
type AiEvent =
  | { kind: "sources"; sources: Source[] }
  | { kind: "status"; text: string }
  | { kind: "token"; text: string }
  | { kind: "done"; seconds: number; first: number }
  | { kind: "error"; text: string };
/** A conversation with the AI: questions and answers so far, and the passages it was given. */
type Answer = {
  /** A general (`??`) conversation with a cloud model, rather than one about the user's files. */
  general: boolean;
  /** The one file it is about, from "Summarize with AI". */
  file?: string;
  turns: { question: string; text: string }[];
  sources: Source[];
  busy: boolean;
  status?: string;
  error?: string;
  took?: string;
};

/** What the local AI needs downloaded, and how far the download has got. */
type AiSetup = {
  parts: { label: string; bytes: number; done: boolean }[];
  download: number;
  needed: number;
  installed: number;
  free: number;
  folder: string;
};
type SetupEvent =
  | { kind: "progress"; part: number; done: number }
  | { kind: "status"; text: string }
  | { kind: "done" }
  | { kind: "error"; text: string };
type AiSetupView = { info: AiSetup; running: boolean; got: number[]; status?: string; error?: string };

const gb = (bytes: number) => `${(bytes / 1e9).toFixed(bytes < 1e9 ? 2 : 1)} GB`;
const fileName = (path: string) => path.slice(path.lastIndexOf("\\") + 1);

/** Row heights in CSS px, comfortable and compact; App.css reads the one in use from --row. */
const ROWS = { comfortable: 44, compact: 34 };

/** Whether the panel is dark: the theme's mode, or Windows' when it follows the system. */
const isDark = () => {
  const mode = document.documentElement.dataset.theme;
  return mode ? mode === "dark" : matchMedia("(prefers-color-scheme: dark)").matches;
};

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
  const [confirming, setConfirming] = createSignal<{ item: Item; action: string } | undefined>();
  const [answer, setAnswer] = createSignal<Answer | undefined>();
  // A note being edited in the preview pane.
  const [editing, setEditing] = createSignal<{ path: string; text: string } | undefined>();

  const current = () => items()[selected()];
  // The content-search folder picker, shown on first run and from its command.
  const [setup, setSetup] = createSignal<Setup>();
  const [setupIndex, setSetupIndex] = createSignal(0);
  // The local AI's download screen.
  const [aiSetup, setAiSetup] = createSignal<AiSetupView>();
  const [settingsState, setSettingsState] = createSignal<SettingsState>();
  const [diagnostics, setDiagnostics] = createSignal<Diagnostics>();
  let diagnosticsPane!: HTMLElement;
  // The prefixes in config.toml, refreshed whenever the panel is shown.
  const [prefixes, setPrefixes] = createSignal<Record<string, string>>({});
  const covered = () => !!setup() || !!aiSetup() || !!settingsState() || !!diagnostics();
  const prefix = (key: string) => `${prefixes()[key] ?? DEFAULT_PREFIXES[key]} `;
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
  const [row, setRow] = createSignal(ROWS.comfortable);

  /** Applies the theme settings: CSS variables, light or dark, and the user's theme.css. */
  function applyTheme(view: SettingsView) {
    const theme = Object.fromEntries(
      view.fields.filter((f) => f.key.startsWith("theme.")).map((f) => [f.key.slice(6), f.value]),
    );
    const root = document.documentElement;
    if (theme.mode === "light" || theme.mode === "dark") root.dataset.theme = String(theme.mode);
    else delete root.dataset.theme;
    const set = (name: string, value: string | undefined) =>
      value ? root.style.setProperty(name, value) : root.style.removeProperty(name);
    set("--accent", theme.accent ? String(theme.accent) : undefined);
    set("--font", theme.font ? `"${theme.font}", var(--font-default)` : undefined);
    set("--font-size", `${theme.font_size}px`);
    const height = theme.density === "compact" ? ROWS.compact : ROWS.comfortable;
    setRow(height);
    set("--row", `${height}px`);
    const solid = theme.backdrop === "solid";
    set("--tint", `${solid ? 100 : Number(theme.opacity)}%`);
    set("--panel", solid ? "var(--solid)" : undefined);
    let link = document.getElementById("user-css") as HTMLLinkElement | null;
    if (view.css === null) link?.remove();
    else {
      if (!link) {
        link = document.createElement("link");
        link.id = "user-css";
        link.rel = "stylesheet";
        document.head.append(link);
      }
      const href = `http://theme.localhost/theme.css?v=${view.css}`;
      if (link.href !== href) link.href = href;
    }
  }

  function takePrefixes(view: SettingsView) {
    applyTheme(view);
    setPrefixes(Object.fromEntries(view.fields.filter((f) => f.key.startsWith("prefixes.")).map((f) => [f.key, String(f.value)])));
  }

  async function openSettings() {
    setActionsOpen(false);
    setSetup(undefined);
    setAiSetup(undefined);
    const view = await invoke<SettingsView>("settings");
    takePrefixes(view);
    setSettingsState({ view, index: 0 });
  }

  function closeSettings() {
    setSettingsState(undefined);
    input.focus();
    search(input.value);
  }

  async function saveSetting(field: Field, value: unknown) {
    const s = settingsState();
    if (!s) return;
    try {
      const view = await invoke<SettingsView>("save_setting", { key: field.key, value });
      takePrefixes(view);
      setSettingsState({ ...s, view, draft: undefined, error: undefined });
    } catch (e) {
      setSettingsState({ ...s, error: String(e) });
    }
  }

  /** Enter on a setting: flips a toggle, steps a choice, opens what an action names, or edits. */
  function activateSetting() {
    const s = settingsState();
    const field = s?.view.fields[s.index];
    if (!s || !field) return;
    if (field.kind === "toggle") saveSetting(field, !field.value);
    else if (field.kind === "choice") {
      const choices = field.choices ?? [];
      saveSetting(field, choices[(choices.indexOf(String(field.value)) + 1) % choices.length]);
    } else if (field.key === "folders") {
      setSettingsState(undefined);
      openSetup(false);
    } else if (field.key === "diagnostics") {
      setSettingsState(undefined);
      invoke<Diagnostics>("diagnostics").then(setDiagnostics);
    } else if (REBUILDS.has(field.key) && s.confirm !== field.key)
      setSettingsState({ ...s, confirm: field.key, notice: `${field.hint}. Press Enter again to go ahead.` });
    else if (["export", "import", ...REBUILDS].includes(field.key)) {
      setSettingsState({ ...s, confirm: undefined, notice: "Working…" });
      invoke<string>("maintenance", { key: field.key })
        .then(async (notice) => {
          const view = await invoke<SettingsView>("settings");
          takePrefixes(view);
          setSettingsState((now) => now && { ...now, view, notice: notice || undefined });
        })
        .catch((e) => setSettingsState((now) => now && { ...now, notice: undefined, error: String(e) }));
    } else if (field.key === "config") invoke("run_action", { item: "seekr:config", action: "open", query: "" });
    else if (field.key === "theme.css") invoke("run_action", { item: "seekr:themecss", action: "open", query: "" });
    else setSettingsState({ ...s, draft: String(field.value), error: undefined });
  }

  function commitDraft() {
    const s = settingsState();
    const field = s?.view.fields[s.index];
    if (!s || !field || s.draft === undefined) return;
    const number = Number(s.draft.trim());
    saveSetting(field, field.kind === "number" && s.draft.trim() !== "" && !isNaN(number) ? number : s.draft);
  }

  async function openAiSetup() {
    setActionsOpen(false);
    const info = await invoke<AiSetup>("ai_setup");
    setAiSetup({ info, running: false, got: info.parts.map(() => 0) });
  }

  function installAi() {
    const view = aiSetup();
    if (!view || view.running) return;
    if (view.info.free < view.info.needed) {
      setAiSetup({ ...view, error: `Free up ${gb(view.info.needed - view.info.free)} on this drive first.` });
      return;
    }
    setAiSetup({ ...view, running: true, error: undefined });
    const onEvent = new Channel<SetupEvent>();
    onEvent.onmessage = (e) => {
      const v = aiSetup();
      if (!v) return;
      if (e.kind === "progress") {
        const got = v.got.slice();
        got[e.part] = e.done;
        setAiSetup({ ...v, got, status: undefined });
      } else if (e.kind === "status") setAiSetup({ ...v, status: e.text });
      else if (e.kind === "done") closeAiSetup();
      else if (e.text === "cancelled") return;
      else setAiSetup({ ...v, running: false, status: undefined, error: e.text });
    };
    invoke("install_ai", { onEvent });
  }

  /** Esc stops a download in progress; what finished stays installed. */
  function closeAiSetup() {
    if (aiSetup()?.running) invoke("cancel_ai_setup");
    setAiSetup(undefined);
    input.focus();
    search(input.value);
  }

  const [preview, setPreview] = createSignal<Preview | undefined>();
  const previewOf = <K extends Preview["kind"]>(kind: K) => {
    const p = preview();
    return p?.kind === kind ? (p as Extract<Preview, { kind: K }>) : undefined;
  };
  // The selected setting or folder stays in sight as the arrow keys move it.
  createEffect(() => {
    const s = settingsState();
    const id = s ? `setting-${s.index}` : setup() ? `folder-${setupIndex()}` : undefined;
    if (id) requestAnimationFrame(() => document.getElementById(id)?.scrollIntoView({ block: "nearest" }));
  });
  let previewTimer = 0;
  // Files and folders get a content preview, asked for once the selection settles.
  createEffect(
    on(current, (item) => {
      setPreview(undefined);
      clearTimeout(previewTimer);
      // A copied image is its own thumbnail.
      if (item?.id.startsWith("clip:") && item.icon?.startsWith("http://thumb.localhost/")) {
        setPreview({ kind: "image", url: item.icon });
        return;
      }
      if (!item?.id.startsWith("path:")) return;
      const dark = isDark();
      previewTimer = setTimeout(() => {
        invoke<Preview>("preview", { path: item.id.slice(5), dark, line: item.line ?? null }).then((p) => {
          if (current()?.id === item.id) setPreview(p);
        });
      }, 60);
    }),
  );
  const range = createMemo(() => visibleRange(scrollTop(), viewport(), row(), items().length));
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
    // Leaving `?` ends the conversation.
    if (!text.trimStart().startsWith("?") && !answer()?.busy) setAnswer(undefined);
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
    if (list) list.scrollTop = scrollFor(index, list.scrollTop, list.clientHeight, row());
  }

  function run(item: Item | undefined, actionId: string | undefined) {
    if (!item || !actionId) return;
    if (actionId === "setup") {
      openSetup(false);
      return;
    }
    if (actionId === "ask") {
      const text = input.value.trimStart();
      const general = text.startsWith("??");
      ask(text.slice(general ? 2 : 1).trim(), general);
      return;
    }
    if (actionId === "settings") {
      openSettings();
      return;
    }
    if (actionId === "aisetup") {
      openAiSetup();
      return;
    }
    if (actionId === "summarize") {
      const path = item.id.slice(5);
      setActionsOpen(false);
      input.value = "?";
      search(input.value);
      ask(`Summarize ${fileName(path)}`, false, path);
      return;
    }
    if (actionId === "askmode") {
      input.value = "?";
      search(input.value);
      return;
    }
    if (actionId === "editnote") {
      const path = item.id.slice(5);
      setActionsOpen(false);
      invoke<string>("read_note", { path }).then((text) => setEditing({ path, text }));
      return;
    }
    if (actionId === "todoadd") {
      // The list comes back with the new to-do in it.
      invoke("run_action", { item: item.id, action: actionId, query: input.value }).then(() => {
        input.value = prefix("prefixes.todo");
        search(input.value);
      });
      return;
    }
    if (actionId in PREFIXES) {
      input.value = prefix(PREFIXES[actionId]);
      search(input.value);
      return;
    }
    const asked = confirming();
    if (actionId in CONFIRM && (asked?.item !== item || asked.action !== actionId)) {
      setActionsOpen(false);
      setConfirming({ item, action: actionId });
      return;
    }
    setConfirming(undefined);
    if (actionId === "pin" || actionId === "unpin") {
      // Pinning keeps the panel open and re-ranks the current query.
      invoke("toggle_pin", { item: item.id }).then(() => search(input.value));
    } else if (STAY_OPEN.has(actionId)) {
      // Pinning or deleting a clipboard entry keeps the history on screen, refreshed.
      invoke("run_action", { item: item.id, action: actionId, query: input.value }).then(() => search(input.value));
    } else {
      invoke("run_action", { item: item.id, action: actionId, query: input.value });
    }
  }

  function ask(question: string, general: boolean, file?: string) {
    const before = answer();
    if (!question || before?.busy) return;
    // Switching between ? and ?? starts a new conversation, as does a file's summary.
    const followUp = !file && !!before && before.turns.length > 0 && !before.error && before.general === general;
    setAnswer({
      general,
      file: file ?? (followUp ? before.file : undefined),
      turns: [...(followUp ? before.turns : []), { question, text: "" }],
      sources: followUp ? before.sources : [],
      busy: true,
    });
    const onEvent = new Channel<AiEvent>();
    onEvent.onmessage = (e) => {
      const a = answer();
      if (!a) return;
      if (e.kind === "sources") setAnswer({ ...a, sources: e.sources });
      else if (e.kind === "status") setAnswer({ ...a, status: e.text });
      else if (e.kind === "token") {
        const turns = a.turns.slice();
        const last = turns[turns.length - 1];
        turns[turns.length - 1] = { ...last, text: last.text + e.text };
        setAnswer({ ...a, turns, status: undefined });
      } else if (e.kind === "done")
        setAnswer({ ...a, busy: false, status: undefined, took: `${e.first.toFixed(1)} s to start, ${e.seconds.toFixed(1)} s in all` });
      else setAnswer({ ...a, busy: false, status: undefined, error: e.text });
    };
    invoke("ask", { question, general, followUp, file: file ?? null, onEvent });
  }

  function openSource(n: number) {
    const source = answer()?.sources.find((s) => s.n === n);
    if (source) invoke("run_action", { item: `path:${source.path}`, action: "open", query: input.value });
  }

  function saveNote() {
    const note = editing();
    if (!note) return;
    invoke("save_note_text", { path: note.path, text: note.text }).then(() => {
      setEditing(undefined);
      input.focus();
      search(input.value);
    });
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape" && answer()?.busy) {
      invoke("stop_answer");
      setAnswer({ ...answer()!, busy: false, status: undefined, took: "Stopped" });
      e.preventDefault();
      return;
    }
    const s = settingsState();
    if (s) {
      const field = s.view.fields[s.index];
      const last = s.view.fields.length - 1;
      if (s.draft !== undefined) {
        // The edit box has the keys, apart from saving and leaving; a list saves on Ctrl+Enter.
        if (e.key === "Escape") setSettingsState({ ...s, draft: undefined, error: undefined });
        else if (e.key === "Enter" && (field?.kind !== "lines" || e.ctrlKey)) commitDraft();
        else return;
      } else if (e.key === "ArrowDown")
        setSettingsState({ ...s, index: Math.min(s.index + 1, last), error: undefined, notice: undefined, confirm: undefined });
      else if (e.key === "ArrowUp")
        setSettingsState({ ...s, index: Math.max(s.index - 1, 0), error: undefined, notice: undefined, confirm: undefined });
      else if (e.key === "Enter" || (e.key === " " && field?.kind === "toggle")) activateSetting();
      else if (e.key === "Escape") closeSettings();
      else return;
      e.preventDefault();
      return;
    }
    if (diagnostics()) {
      if (e.key === "ArrowDown" || e.key === "ArrowUp")
        diagnosticsPane.scrollBy({ top: e.key === "ArrowDown" ? 60 : -60 });
      else if (e.key === "Escape") {
        setDiagnostics(undefined);
        openSettings();
      } else if (e.key === "Enter") invoke("run_action", { item: "seekr:logs", action: "open", query: "" });
      else return;
      e.preventDefault();
      return;
    }
    const download = aiSetup();
    if (download) {
      if (e.key === "Enter") installAi();
      else if (e.key === "Escape") closeAiSetup();
      else return;
      e.preventDefault();
      return;
    }
    if (editing()) {
      // The editor has the keys, apart from saving and leaving.
      if (e.ctrlKey && e.key === "s") saveNote();
      else if (e.key === "Escape") {
        setEditing(undefined);
        input.focus();
      } else return;
      e.preventDefault();
      return;
    }
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
      if (e.key === "Enter") run(pending.item, pending.action);
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
    } else if (e.ctrlKey && /^Digit[1-9]$/.test(e.code) && answer()?.sources.length) {
      // Citations, without the mouse.
      openSource(Number(e.code.slice(5)));
    } else if (e.altKey && /^Digit[1-9]$/.test(e.code)) {
      const target = items()[firstVisible(list.scrollTop, row()) + Number(e.code.slice(5)) - 1];
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
      invoke<SettingsView>("settings").then(takePrefixes);
      if (!askedSetup) {
        askedSetup = true;
        invoke<boolean>("content_setup_needed").then((needed) => {
          if (needed) openSetup(true);
        });
      }
    });
    window.addEventListener("keydown", onKey);
    listen("open-settings", () => openSettings());
    invoke<SettingsView>("settings").then(takePrefixes);
    new ResizeObserver(() => setViewport(list.clientHeight)).observe(list);
    search("");
  });

  const numberOf = (index: number) => {
    const n = index - firstVisible(scrollTop(), row()) + 1;
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
            <ul
              class="folders"
              role="listbox"
              aria-label="Folders to search inside"
              aria-activedescendant={`folder-${setupIndex()}`}
              tabIndex={-1}
              ref={(el) => requestAnimationFrame(() => el.focus())}
            >
              <For each={s().folders}>
                {(f, i) => (
                  <li
                    id={`folder-${i()}`}
                    role="option"
                    aria-selected={i() === setupIndex()}
                    aria-checked={f.checked}
                    classList={{ selected: i() === setupIndex() }}
                    onClick={() => {
                      setSetupIndex(i());
                      toggleFolder(i());
                    }}
                  >
                    <input type="checkbox" checked={f.checked} tabIndex={-1} aria-hidden="true" />
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
      <Show when={settingsState()}>
        {(s) => (
          <section class="setup">
            <h2>Settings</h2>
            <p class="subtitle">Saved to {s().view.path} and applied at once. Edits made there apply too.</p>
            <Show when={s().view.problem}>
              {(problem) => <p class="error">config.toml isn't applied until this is fixed: {problem()}</p>}
            </Show>
            <ul
              class="folders fields"
              role="listbox"
              aria-label="Settings"
              aria-activedescendant={`setting-${s().index}`}
              tabIndex={-1}
              ref={(el) => requestAnimationFrame(() => el.focus())}
            >
              <For each={s().view.fields}>
                {(f, i) => (
                  <>
                    <Show when={i() === 0 || s().view.fields[i() - 1].group !== f.group}>
                      <li class="group" role="presentation">
                        {f.group}
                      </li>
                    </Show>
                    <li
                      id={`setting-${i()}`}
                      role="option"
                      aria-selected={i() === s().index}
                      aria-description={[f.group, f.hint].filter(Boolean).join(". ")}
                      classList={{ selected: i() === s().index }}
                      onClick={() => setSettingsState({ ...s(), index: i(), draft: undefined })}
                      onDblClick={activateSetting}
                    >
                      <span>{f.label}</span>
                      <Show
                        when={i() === s().index && s().draft !== undefined}
                        fallback={
                          <span class="size">
                            {f.kind === "toggle"
                              ? f.value
                                ? "On"
                                : "Off"
                              : f.kind === "lines"
                                ? `${String(f.value).split("\n").filter((l) => l.trim()).length || "None"}`
                                : String(f.value) || "Default"}
                          </span>
                        }
                      >
                        <Show
                          when={f.kind === "lines"}
                          fallback={
                            <input
                              class="edit"
                              value={s().draft}
                              spellcheck={false}
                              ref={(el) => requestAnimationFrame(() => el.select())}
                              onInput={(e) => setSettingsState({ ...s(), draft: e.currentTarget.value })}
                            />
                          }
                        >
                          <textarea
                            class="edit"
                            value={s().draft}
                            spellcheck={false}
                            rows={5}
                            ref={(el) => requestAnimationFrame(() => el.focus())}
                            onInput={(e) => setSettingsState({ ...s(), draft: e.currentTarget.value })}
                          />
                        </Show>
                      </Show>
                      <Show when={f.hint}>
                        <span class="hint">{f.hint}</span>
                      </Show>
                    </li>
                  </>
                )}
              </For>
            </ul>
          </section>
        )}
      </Show>
      <Show when={diagnostics()}>
        {(d) => (
          <section
            class="setup diagnostics"
            aria-label="Diagnostics"
            tabIndex={-1}
            ref={(el) => {
              diagnosticsPane = el;
              requestAnimationFrame(() => el.focus());
            }}
          >
            <h2>Diagnostics</h2>
            <For each={d().sections}>
              {(section) => (
                <>
                  <h3>{section.title}</h3>
                  <table>
                  <tbody>
                    <For each={section.rows}>
                      {(r) => (
                        <tr>
                          <td>{r.label}</td>
                          <td>{r.value}</td>
                        </tr>
                      )}
                    </For>
                  </tbody>
                  </table>
                </>
              )}
            </For>
            <h3>Latency since Seekr started (ms)</h3>
            <Show when={d().latency.length > 0} fallback={<p class="subtitle">No samples yet.</p>}>
              <table class="latency">
              <tbody>
                <tr>
                  <th />
                  <th>n</th>
                  <th>p50</th>
                  <th>p95</th>
                  <th>max</th>
                  <th>
                    <For each={BUCKET_LABELS}>{(label) => <span class="bucket">{label}</span>}</For>
                  </th>
                </tr>
                <For each={d().latency}>
                  {(l) => (
                    <tr>
                      <td>{l.metric}</td>
                      <td>{l.n}</td>
                      <td>{l.p50.toFixed(1)}</td>
                      <td>{l.p95.toFixed(1)}</td>
                      <td>{l.max.toFixed(1)}</td>
                      <td>
                        <For each={l.buckets}>
                          {(count) => (
                            <span class="bucket" title={`${count}`}>
                              <span class="bar" style={{ height: `${Math.round((count / l.n) * 100)}%` }} />
                            </span>
                          )}
                        </For>
                      </td>
                    </tr>
                  )}
                </For>
              </tbody>
              </table>
            </Show>
            <h3>Quarantined files</h3>
            <Show when={d().quarantined.length > 0} fallback={<p class="subtitle">None.</p>}>
              <ul class="listing">
                <For each={d().quarantined}>{([path, why]) => <li title={why}>{path}</li>}</For>
              </ul>
            </Show>
            <h3>Log</h3>
            <p class="subtitle">{d().logs}</p>
            <pre class="log">{d().log.join("\n")}</pre>
          </section>
        )}
      </Show>
      <Show when={aiSetup()}>
        {(v) => (
          <section class="setup">
            <h2>Local AI</h2>
            <p class="subtitle">
              Questions about your files are answered by a model on this PC, so your files never leave it. It needs
              these, downloaded once from GitHub and Hugging Face and checked before use:
            </p>
            <ul class="folders downloads" aria-label="Downloads">
              <For each={v().info.parts}>
                {(part, i) => (
                  <li>
                    <span>{part.label}</span>
                    <span class="size">
                      {part.done
                        ? "Installed"
                        : v().got[i()] > 0
                          ? `${gb(v().got[i()])} of ${gb(part.bytes)}`
                          : gb(part.bytes)}
                    </span>
                    <progress
                      max={part.bytes}
                      value={part.done ? part.bytes : v().got[i()]}
                      aria-label={part.label}
                    />
                  </li>
                )}
              </For>
            </ul>
            <p class="subtitle">
              Downloads {gb(v().info.download)} and needs {gb(v().info.needed)} free while installing ({gb(v().info.free)}{" "}
              free now). Once installed it takes {gb(v().info.installed)} in {v().info.folder}. The model uses about 4 GB
              of graphics memory while answering and is unloaded after 10 idle minutes.
            </p>
            <Show when={v().status}>{(status) => <p class="subtitle">{status()}</p>}</Show>
            <Show when={v().error}>{(error) => <p class="error">{error()}</p>}</Show>
          </section>
        )}
      </Show>
      <input
        ref={input}
        classList={{ hidden: covered() }}
        class="query"
        role="combobox"
        aria-label="Search"
        aria-expanded={items().length > 0}
        aria-controls="results"
        aria-autocomplete="list"
        aria-activedescendant={
          actionsOpen() ? `action-${actionIndex()}` : items().length ? `row-${selected()}` : undefined
        }
        placeholder="Search apps, files and more"
        spellcheck={false}
        autofocus
        onInput={(e) => search(e.currentTarget.value)}
      />
      <Show when={!covered() && chips().length > 0}>
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
      <div class="body" classList={{ hidden: covered() }}>
        <div
          class="results"
          id="results"
          role="listbox"
          aria-label="Results"
          ref={list}
          onScroll={(e) => setScrollTop(e.currentTarget.scrollTop)}
        >
          <div class="rows" style={{ height: `${items().length * row()}px` }}>
            <For each={rows()}>
              {(i) => (
                <div
                  class="row"
                  id={`row-${i}`}
                  role="option"
                  aria-selected={i === selected()}
                  aria-setsize={items().length}
                  aria-posinset={i + 1}
                  aria-description={items()[i]?.subtitle}
                  classList={{ selected: i === selected() }}
                  style={{ top: `${i * row()}px` }}
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
                  <kbd class="number" aria-hidden="true">
                    {numberOf(i)}
                  </kbd>
                </div>
              )}
            </For>
          </div>
          <Show when={items().length === 0}>
            <div class="empty" role="status">
              {query().trim() ? "No results" : "Type to search apps"}
            </div>
          </Show>
        </div>
        <aside class="preview" aria-label="Preview">
          <Show when={editing()}>
            {(note) => (
              <textarea
                class="editor"
                value={note().text}
                spellcheck={false}
                ref={(el) => requestAnimationFrame(() => el.focus())}
                onInput={(e) => setEditing({ path: note().path, text: e.currentTarget.value })}
              />
            )}
          </Show>
          <Show when={!editing() && answer()}>
            {(a) => (
              <div class="answer" aria-live="polite" aria-busy={a().busy}>
                <Show when={a().file}>
                  {(file) => <p class="subtitle">About {fileName(file())} · follow-ups with ? ask about it too</p>}
                </Show>
                <For each={a().turns}>
                  {(turn) => (
                    <>
                      <p class="question">{turn.question}</p>
                      <p class="reply">
                        <For each={turn.text.split(/(\[\d+\])/)}>
                          {(part) => {
                            const cited = /^\[(\d+)\]$/.exec(part);
                            return cited ? (
                              <button
                                class="cite"
                                title="Open this file"
                                aria-label={`Source ${cited[1]}`}
                                tabIndex={-1}
                                onClick={() => openSource(Number(cited[1]))}
                              >
                                {cited[1]}
                              </button>
                            ) : (
                              part
                            );
                          }}
                        </For>
                      </p>
                    </>
                  )}
                </For>
                <Show when={a().status}>{(status) => <p class="subtitle">{status()}</p>}</Show>
                <Show when={a().error}>{(error) => <p class="error">Couldn't answer: {error()}</p>}</Show>
                <Show when={a().took}>{(took) => <p class="subtitle">{took()}</p>}</Show>
                <Show when={a().sources.length > 0}>
                  <ol class="sources">
                    <For each={a().sources}>
                      {(s) => (
                        <li value={s.n} title={s.path} onClick={() => openSource(s.n)}>
                          {s.name} · line {s.line}
                        </li>
                      )}
                    </For>
                  </ol>
                </Show>
              </div>
            )}
          </Show>
          <Show when={!editing() && !answer() && current()}>
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
                      <pre class="code" classList={{ numbered: p().focus !== undefined }}>
                        <For each={p().lines}>
                          {(line, i) => {
                            const number = p().first + i();
                            const focused = number === p().focus;
                            return (
                              <div
                                classList={{ focus: focused }}
                                ref={(el) => focused && requestAnimationFrame(() => el.scrollIntoView({ block: "center" }))}
                              >
                                <Show when={p().focus !== undefined}>
                                  <span class="gutter">{number}</span>
                                </Show>
                                <For each={line}>
                                  {([color, text]) => (
                                    <span style={{ color }}>
                                      <For each={marks(text, query())}>
                                        {(m) => (m.hit ? <mark>{m.text}</mark> : m.text)}
                                      </For>
                                    </span>
                                  )}
                                </For>
                              </div>
                            );
                          }}
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
          <ul class="actions" role="listbox" aria-label="Actions">
            <For each={current()?.actions ?? []}>
              {(action, i) => (
                <li
                  id={`action-${i()}`}
                  role="option"
                  aria-selected={i() === actionIndex()}
                  classList={{ selected: i() === actionIndex() }}
                  onClick={() => run(current(), action.id)}
                >
                  {action.label}
                </li>
              )}
            </For>
          </ul>
        </Show>
      </div>
      <footer class="hints">
        <Show when={settingsState()}>
          {(s) => (
            <Show
              when={s().draft !== undefined}
              fallback={
                <>
                  <span><kbd>↵</kbd> {s().confirm ? "Confirm" : "Change"}</span>
                  <span><kbd>Esc</kbd> Close</span>
                  <Show when={s().notice}>
                    {(notice) => (
                      <span class="confirm" role="status">
                        {notice()}
                      </span>
                    )}
                  </Show>
                  <Show when={s().error}>
                    {(error) => (
                      <span class="error" role="alert">
                        {error()}
                      </span>
                    )}
                  </Show>
                </>
              }
            >
              <span><kbd>{s().view.fields[s().index]?.kind === "lines" ? "Ctrl+↵" : "↵"}</kbd> Save</span>
              <span><kbd>Esc</kbd> Cancel</span>
              <Show when={s().error}>
                {(error) => (
                  <span class="error" role="alert">
                    {error()}
                  </span>
                )}
              </Show>
            </Show>
          )}
        </Show>
        <Show when={diagnostics()}>
          <span><kbd>↑↓</kbd> Scroll</span>
          <span><kbd>↵</kbd> Open the logs folder</span>
          <span><kbd>Esc</kbd> Back to settings</span>
        </Show>
        <Show when={aiSetup()}>
          {(v) => (
            <>
              <Show when={!v().running}>
                <span><kbd>↵</kbd> {v().error ? "Try again" : "Download"}</span>
              </Show>
              <span><kbd>Esc</kbd> {v().running ? "Stop download" : "Cancel"}</span>
            </>
          )}
        </Show>
        <Show
          when={!covered()}
          fallback={
            <Show when={setup()}>
              <span><kbd>Space</kbd> Tick</span>
              <span><kbd>Ctrl+O</kbd> Add folder</span>
              <span><kbd>↵</kbd> Save</span>
              <span><kbd>Esc</kbd> {firstRun ? "Not now" : "Cancel"}</span>
            </Show>
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
                <Show when={answer()?.sources.length}>
                  <span><kbd>Ctrl+1…9</kbd> Open a source</span>
                </Show>
                <span><kbd>Esc</kbd> {answer()?.busy ? "Stop" : actionsOpen() ? "Back" : "Close"}</span>
              </>
            }
          >
            {(asked) => (
              <span class="confirm" role="alert">
                <kbd>↵</kbd> {CONFIRM[asked().action](asked().item)} · any other key cancels
              </span>
            )}
          </Show>
        </Show>
      </footer>
    </main>
  );
}

export default App;
