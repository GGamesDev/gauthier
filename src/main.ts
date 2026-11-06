import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { open as openDialog, save as saveDialog } from '@tauri-apps/plugin-dialog';
import type { Extension } from '@codemirror/state';
import { indentWithTab } from '@codemirror/commands';
import { rust } from '@codemirror/lang-rust';
import { HighlightStyle, syntaxHighlighting, indentUnit } from '@codemirror/language';
import { Diagnostic as CmDiagnostic, lintGutter, setDiagnostics } from '@codemirror/lint';
import { EditorState } from '@codemirror/state';
import { EditorView, keymap } from '@codemirror/view';
import { tags as t } from '@lezer/highlight';
import { basicSetup } from 'codemirror';
import { ansiToHtml } from './ansi';
import { EXAMPLES, TargetId, templateFor } from './examples';
import { selftest } from './selftest';
import { Explorer } from './explorer';
import { Plotter } from './plotter';
import { outline } from './outline';
import { explain } from './explain';
import { readDeps, removeDep, setDep } from './crates';
import { breakpointLines, clearDebugPanel, debugExtensions, renderStopped, showDebugLine, type Stopped } from './debugui';
import { assistExtensions, completionSource, LiveChecker, CheckDiagnostic } from './assist';

// ---------------------------------------------------------------- types

interface ToolStatus {
  cargo: string | null;
  linker: boolean;
  missing_targets: string[];
  picotool: string | null;
  analyzer: boolean;
}
interface Diagnostic {
  level: 'error' | 'warning';
  message: string;
  line: number;
  column: number;
  end_line: number;
  end_column: number;
  label: string | null;
  rendered: string;
  code?: string | null;
}
type RunStateName = 'building' | 'flashing' | 'running' | 'idle';

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

// ---------------------------------------------------------------- persistence

/** In self-test mode nothing is persisted: the user's tabs and settings stay untouched. */
const selftestMode = await invoke<string | null>('selftest_mode').catch(() => null);
if (selftestMode) (window as unknown as { __gauthierNoPersist: boolean }).__gauthierNoPersist = true;

/** Self-test: settings live in memory only. */
const memoryStore = new Map<string, string>();

const store = {
  get: (k: string) => {
    if (selftestMode) return memoryStore.get(k) ?? null;
    try {
      return localStorage.getItem(`gauthier.${k}`);
    } catch {
      return null;
    }
  },
  set: (k: string, v: string) => {
    if (selftestMode) {
      memoryStore.set(k, v);
      return;
    }
    try {
      localStorage.setItem(`gauthier.${k}`, v);
    } catch {
      /* private mode */
    }
  },
};

// ---------------------------------------------------------------- preferences

interface Prefs {
  theme: 'system' | 'light' | 'dark';
  fontSize: number;
  liveCheck: boolean;
  assist: boolean;
  explain: boolean;
}
const prefs: Prefs = { theme: 'system', fontSize: 14, liveCheck: true, assist: true, explain: true };
try {
  Object.assign(prefs, JSON.parse(store.get('prefs') ?? '{}'));
} catch {
  /* defaults */
}

function applyPrefs() {
  prefs.fontSize = Math.max(9, Math.min(32, Math.round(prefs.fontSize) || 14));
  const root = document.documentElement;
  if (prefs.theme === 'system') delete root.dataset.theme;
  else root.dataset.theme = prefs.theme;
  root.style.setProperty('--font-size', `${prefs.fontSize}px`);
  store.set('prefs', JSON.stringify(prefs));
}
applyPrefs();

// ---------------------------------------------------------------- state

let target = (store.get('target') as TargetId) || 'desktop';
/** What a tab shows: an unsaved buffer, a file on the computer or a file on the Pico. */
type Doc = { kind: 'untitled'; name: string } | { kind: 'local'; path: string } | { kind: 'pico'; name: string };

interface Tab {
  id: number;
  doc: Doc;
  /** Editor state (text, undo history, selection, lint markers). */
  state: EditorState;
  /** Text as last saved/opened ('' for a never-saved buffer). */
  saved: string;
  /** Diagnostics waiting to be shown when the tab becomes active. */
  pendingDiags: Diagnostic[] | null;
}

let tabs: Tab[] = [];
/** Tab being debugged (null when not debugging). */
let debugTab: Tab | null = null;
let active!: Tab;
let nextTabId = 1;
/** Tab whose code the last Run used (its errors go there). */
let runTab: Tab | null = null;
let explorer: Explorer | null = null;
let runState: RunStateName = 'idle';
let tools: ToolStatus | null = null;
/** True when the serial monitor (not a computer program) is the current session. */
let serialConnected = false;
/** The user clicked Disconnect/Stop: do not reconnect automatically until next Run/Connect. */
let userDisconnected = false;

// ---------------------------------------------------------------- editor

const highlight = HighlightStyle.define([
  { tag: [t.keyword, t.controlKeyword, t.modifier], color: 'var(--syn-keyword)', fontWeight: '600' },
  { tag: [t.string, t.special(t.string), t.character], color: 'var(--syn-string)' },
  { tag: [t.number, t.bool, t.null], color: 'var(--syn-number)' },
  { tag: [t.comment, t.lineComment, t.blockComment, t.docComment], color: 'var(--syn-comment)', fontStyle: 'italic' },
  { tag: [t.typeName, t.className, t.namespace], color: 'var(--syn-type)' },
  { tag: [t.function(t.variableName), t.function(t.propertyName), t.macroName], color: 'var(--syn-fn)' },
  { tag: [t.attributeName, t.meta, t.processingInstruction], color: 'var(--syn-attr)' },
  { tag: [t.operator, t.punctuation], color: 'var(--syn-punct)' },
  { tag: [t.labelName, t.special(t.variableName)], color: 'var(--syn-number)' },
]);

const theme = EditorView.theme({
  '&': { height: '100%', backgroundColor: 'var(--editor-bg)', color: 'var(--text)' },
  '.cm-content': { fontFamily: 'var(--mono)', fontSize: 'var(--font-size)', caretColor: 'var(--accent)' },
  '.cm-gutters .cm-gutterElement': { fontSize: 'calc(var(--font-size) - 1px)' },
  '.cm-gutters': { backgroundColor: 'var(--editor-bg)', color: 'var(--muted)', border: 'none' },
  '.cm-activeLine': { backgroundColor: 'var(--active-line)' },
  '.cm-activeLineGutter': { backgroundColor: 'var(--active-line)', color: 'var(--text)' },
  '&.cm-focused .cm-selectionBackground, .cm-selectionBackground, ::selection': { backgroundColor: 'var(--selection) !important' },
  '.cm-cursor': { borderLeftColor: 'var(--accent)' },
  '.cm-tooltip': { backgroundColor: 'var(--surface)', color: 'var(--text)', border: '1px solid var(--border)' },
  '.cm-panels': { backgroundColor: 'var(--surface)', color: 'var(--text)' },
});

let persistTimer: number | undefined;

// Completion & hover (rust-analyzer), checking as you type (cargo check).
const assistHost = {
  target: () => target,
  isRust: () => fileName().endsWith('.rs'),
  ready: () => prefs.assist && !!tools?.analyzer && (target === 'desktop' ? cargoReady() : picoReady()),
};
const rustLanguage = rust();
const liveChecker = new LiveChecker(
  (code, t) => invoke('check_program', { code, target: t }),
  (code, diags) => {
    if (text() === code) showDiagnostics(diags, false);
  },
  (checking) => ($('status-check').hidden = !checking),
);

const extensions: Extension[] = [
  basicSetup,
  rustLanguage,
  rustLanguage.language.data.of({ autocomplete: completionSource(assistHost) }),
  ...assistExtensions(assistHost),
  ...debugExtensions(() => {
    if (debugTab === active) void invoke('debug_breakpoints', { lines: breakpointLines(view.state) }).catch(() => undefined);
  }),
  indentUnit.of('    '),
  keymap.of([indentWithTab]),
  syntaxHighlighting(highlight),
  theme,
  lintGutter(),
  EditorView.updateListener.of((u) => {
    if (u.docChanged) {
      active.state = u.state;
      renderTabs();
      updateTitle();
      schedulePersist();
      scheduleCheck();
      scheduleOutline();
    }
    if (u.selectionSet || u.docChanged) {
      const pos = u.state.selection.main.head;
      const line = u.state.doc.lineAt(pos);
      $('status-pos').textContent = `Ln ${line.number}, Col ${pos - line.from + 1}`;
    }
  }),
];

const makeState = (text: string) => EditorState.create({ doc: text, extensions });

const view = new EditorView({ parent: $('editor'), state: makeState('') });

function text(tab: Tab = active) {
  return (tab === active ? view.state : tab.state).doc.toString();
}

/** Replaces the text of the active tab (keeps undo history). */
function setText(s: string) {
  view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: s } });
  view.dispatch(setDiagnostics(view.state, []));
}

function fileName(tab: Tab = active) {
  return tab.doc.kind === 'local' ? tab.doc.path.split(/[\\/]/).pop()! : tab.doc.name;
}

function isDirty(tab: Tab = active) {
  return text(tab) !== tab.saved;
}

function sameDoc(a: Doc, b: Doc) {
  if (a.kind === 'local' && b.kind === 'local') return a.path === b.path;
  if (a.kind === 'pico' && b.kind === 'pico') return a.name === b.name;
  return false;
}

/** The active tab now shows `d` (after Save / rename). */
function setDoc(d: Doc, newText: string | null, saved: string) {
  active.doc = d;
  remember(d);
  if (newText !== null) setText(newText);
  active.saved = saved;
  renderTabs();
  updateTitle();
  schedulePersist();
  explorer?.highlight(d);
}

function activate(tab: Tab) {
  if (active && active !== tab && tabs.includes(active)) active.state = view.state;
  active = tab;
  view.setState(tab.state);
  if (tab.pendingDiags) {
    applyLint(tab.pendingDiags);
    tab.pendingDiags = null;
    active.state = view.state;
  }
  renderTabs();
  updateTitle();
  explorer?.highlight(tab.doc);
  store.set('activeTab', String(tabs.indexOf(tab)));
  view.focus();
  scheduleCheck();
  scheduleOutline();
}

/** Opens `doc` in a tab (or switches to the tab that already shows it). */
// Recent files ---------------------------------------------------------------

function recentList(): Doc[] {
  try {
    return JSON.parse(store.get('recent') ?? '[]');
  } catch {
    return [];
  }
}

function remember(doc: Doc) {
  if (doc.kind === 'untitled') return;
  const list = [doc, ...recentList().filter((d) => !sameDoc(d, doc))].slice(0, 12);
  store.set('recent', JSON.stringify(list));
  fillRecent();
}

function fillRecent() {
  const sel = document.getElementById('recent') as HTMLSelectElement | null;
  if (!sel) return;
  sel.innerHTML = '<option value="">Recent…</option>';
  recentList().forEach((d, i) => {
    const o = document.createElement('option');
    o.value = String(i);
    o.textContent = d.kind === 'local' ? `${d.path.split(/[\\/]/).pop()} — ${d.path.replace(/^\/Users\/[^/]+/, '~').replace(/\/[^/]*$/, '')}` : `${d.name} — on the Pico`;
    sel.appendChild(o);
  });
}

function openTab(doc: Doc, content: string, saved: string): Tab {
  remember(doc);
  const existing = doc.kind === 'untitled' ? undefined : tabs.find((t) => sameDoc(t.doc, doc));
  if (existing) {
    // Refresh an unmodified tab with what is on disk / on the Pico now.
    if (!isDirty(existing) && text(existing) !== content) {
      existing.state = makeState(content);
      existing.saved = saved;
      if (existing === active) view.setState(existing.state);
    }
    activate(existing);
    return existing;
  }
  const tab: Tab = { id: nextTabId++, doc, state: makeState(content), saved, pendingDiags: null };
  // A single untouched starter tab is replaced, like most editors do.
  const replace = tabs.length === 1 && active.doc.kind === 'untitled' && !isDirty(active) && doc.kind !== 'untitled';
  if (replace) tabs = [tab];
  else tabs.splice(active ? tabs.indexOf(active) + 1 : tabs.length, 0, tab);
  activate(tab);
  schedulePersist();
  return tab;
}

function untitledName(base = 'untitled') {
  const names = new Set(tabs.map((t) => fileName(t)));
  if (!names.has(`${base}.rs`)) return `${base}.rs`;
  let i = 2;
  while (names.has(`${base}-${i}.rs`)) i++;
  return `${base}-${i}.rs`;
}

async function closeTab(tab: Tab) {
  if (isDirty(tab)) {
    activate(tab);
    const answer = await choose(`Save changes to ${fileName(tab)}?`, "Your changes will be lost if you don't save them.", [
      ['cancel', 'Cancel', 'secondary'],
      ['discard', "Don't save", 'secondary'],
      ['save', 'Save', 'primary'],
    ]);
    if (answer === 'cancel' || answer === null) return;
    if (answer === 'save' && !(await saveFile())) return;
  }
  const i = tabs.indexOf(tab);
  tabs.splice(i, 1);
  if (runTab === tab) runTab = null;
  if (!tabs.length) {
    const t = templateFor(target);
    tabs = [{ id: nextTabId++, doc: { kind: 'untitled', name: 'untitled.rs' }, state: makeState(t), saved: t, pendingDiags: null }];
  }
  activate(tabs[Math.min(i, tabs.length - 1)]);
  schedulePersist();
}

function renderTabs() {
  const list = $('etab-list');
  list.innerHTML = '';
  for (const tab of tabs) {
    const el = document.createElement('div');
    el.className = `etab${tab === active ? ' active' : ''}${isDirty(tab) ? ' dirty' : ''}`;
    el.dataset.name = fileName(tab);
    el.title = tab.doc.kind === 'local' ? tab.doc.path : tab.doc.kind === 'pico' ? `${tab.doc.name} on the Raspberry Pi Pico` : 'Not saved yet';
    el.innerHTML = `<span class="etab-name"></span>${tab.doc.kind === 'pico' ? '<span class="where">Pico</span>' : ''}<button class="etab-close" title="Close (⌘W)"><span>×</span></button>`;
    (el.querySelector('.etab-name') as HTMLElement).textContent = fileName(tab);
    el.addEventListener('mousedown', (e) => {
      if (e.button === 1) {
        e.preventDefault();
        void closeTab(tab);
      }
    });
    el.addEventListener('click', (e) => {
      if ((e.target as HTMLElement).closest('.etab-close')) void closeTab(tab);
      else if (tab !== active) activate(tab);
    });
    list.appendChild(el);
    if (tab === active) el.scrollIntoView({ block: 'nearest', inline: 'nearest' });
  }
}

// Outline ----------------------------------------------------------------------

let outlineTimer: number | undefined;
function scheduleOutline() {
  window.clearTimeout(outlineTimer);
  outlineTimer = window.setTimeout(renderOutline, 300);
}

function renderOutline() {
  const list = $('outline-list');
  list.innerHTML = '';
  if (!fileName().endsWith('.rs')) {
    list.innerHTML = '<li class="empty">Not a Rust file</li>';
    return;
  }
  const items = outline(view.state);
  if (!items.length) list.innerHTML = '<li class="empty">No functions yet</li>';
  for (const it of items) {
    const li = document.createElement('li');
    li.style.paddingLeft = `${10 + it.depth * 14}px`;
    li.innerHTML = `<span class="kind k-${it.kind}">${it.kind}</span><span class="name"></span><span class="size">${it.line}</span>`;
    (li.querySelector('.name') as HTMLElement).textContent = it.name;
    li.title = `${it.kind} ${it.name} — line ${it.line}`;
    li.onclick = () => {
      view.dispatch({ selection: { anchor: it.from }, scrollIntoView: true });
      view.focus();
    };
    list.appendChild(li);
  }
}

/** Live check of the active tab (Rust files only, once the tools are there). */
function scheduleCheck() {
  liveChecker.schedule(
    () => text(),
    () => target,
    () => prefs.liveCheck && fileName().endsWith('.rs') && runState !== 'building' && (target === 'desktop' ? cargoReady() : picoReady()),
  );
}

function updateTitle() {
  const where = active.doc.kind === 'pico' ? ' (Pico)' : '';
  const name = fileName() + where + (isDirty() ? ' •' : '');
  $('status-file').textContent = name;
  void getCurrentWindow().setTitle(`${name} — Gauthier`).catch(() => undefined);
}

/** Open tabs survive a restart, unsaved changes included (like Thonny). */
function schedulePersist() {
  window.clearTimeout(persistTimer);
  persistTimer = window.setTimeout(() => {
    if (active) active.state = view.state;
    store.set('tabs', JSON.stringify(tabs.map((t) => ({ doc: t.doc, text: text(t), saved: t.saved }))));
    store.set('activeTab', String(tabs.indexOf(active)));
  }, 400);
}

function restoreTabs() {
  let saved: { doc: Doc; text: string; saved: string }[] = [];
  try {
    saved = JSON.parse(store.get('tabs') ?? '[]');
  } catch {
    /* ignore */
  }
  if (!saved.length) {
    // Single-file versions of Gauthier stored one draft.
    const draft = store.get('draft');
    saved = [{ doc: { kind: 'untitled', name: 'untitled.rs' }, text: draft ?? templateFor(target), saved: draft ? '' : templateFor(target) }];
  }
  tabs = saved.map((t) => ({ id: nextTabId++, doc: t.doc, state: makeState(t.text), saved: t.saved, pendingDiags: null }));
  const i = Number(store.get('activeTab'));
  activate(tabs[Number.isInteger(i) && i >= 0 && i < tabs.length ? i : 0]);
}

// Little helpers ------------------------------------------------------------

let toastTimer: number | undefined;
function toast(message: string, error = false) {
  const el = $('toast');
  el.textContent = message;
  el.className = `toast${error ? ' error' : ''}`;
  el.hidden = false;
  window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => (el.hidden = true), error ? 6000 : 2500);
}

/** Modal with custom buttons; resolves with the chosen id (null on Escape). */
function choose(title: string, message: string, buttons: [string, string, string][]): Promise<string | null> {
  const d = $<HTMLDialogElement>('choice-dialog');
  $('choice-title').textContent = title;
  $('choice-text').textContent = message;
  const box = $('choice-buttons');
  box.innerHTML = '';
  return new Promise((resolve) => {
    const done = (v: string | null) => {
      d.close();
      resolve(v);
    };
    for (const [id, label, cls] of buttons) {
      const b = document.createElement('button');
      b.className = cls;
      b.dataset.choice = id;
      b.textContent = label;
      b.onclick = () => done(id);
      box.appendChild(b);
    }
    d.oncancel = () => done(null);
    d.showModal();
    (box.lastElementChild as HTMLButtonElement | null)?.focus();
  });
}

// ---------------------------------------------------------------- console

const output = $('output');
let autoScroll = true;
output.addEventListener('scroll', () => {
  autoScroll = output.scrollTop + output.clientHeight >= output.scrollHeight - 8;
});

function print(kind: string, s: string) {
  if (kind === 'out') plotter.feed(s);
  const span = document.createElement('span');
  span.className = `c-${kind}`;
  span.innerHTML = ansiToHtml(s);
  output.appendChild(span);
  // Keep the console light.
  while (output.childNodes.length > 4000) output.removeChild(output.firstChild!);
  if (autoScroll) output.scrollTop = output.scrollHeight;
}

const plotter = new Plotter($<HTMLCanvasElement>('plot-canvas'), $('plot-legend'), $('plot-empty'));
$('btn-clear').addEventListener('click', () => {
  output.innerHTML = '';
  plotter.clear();
});

$('input-form').addEventListener('submit', async (e) => {
  e.preventDefault();
  const input = $<HTMLInputElement>('input');
  const line = input.value;
  input.value = '';
  print('echo', line + '\n');
  try {
    await invoke('send_input', { text: line + '\n' });
  } catch (err) {
    print('error', `${err}\n`);
  }
});

// ---------------------------------------------------------------- problems

type PanelTab = 'console' | 'problems' | 'plotter' | 'debugger';
function showTab(name: PanelTab) {
  document.querySelectorAll<HTMLButtonElement>('.tab').forEach((b) => b.classList.toggle('active', b.dataset.tab === name));
  $('console').hidden = name !== 'console';
  $('problems').hidden = name !== 'problems';
  $('plotter').hidden = name !== 'plotter';
  $('debugger').hidden = name !== 'debugger';
  plotter.visible = name === 'plotter';
  if (plotter.visible) plotter.request();
}
document.querySelectorAll<HTMLButtonElement>('.tab').forEach((b) =>
  b.addEventListener('click', () => showTab(b.dataset.tab as PanelTab)),
);

function posOf(line: number, col: number) {
  const doc = view.state.doc;
  void doc;
  if (line < 1 || line > doc.lines) return 0;
  const l = doc.line(line);
  return Math.min(l.from + Math.max(col - 1, 0), l.to);
}

function applyLint(diags: Diagnostic[]) {
  const cm: CmDiagnostic[] = diags
    .filter((d) => d.line > 0)
    .map((d) => {
      const from = posOf(d.line, d.column);
      let to = posOf(d.end_line, d.end_column);
      if (to <= from) to = Math.min(from + 1, view.state.doc.length);
      const ex = prefs.explain ? explain(d.code, d.message) : null;
      let message = d.label ? `${d.message}\n→ ${d.label}` : d.message;
      if (ex) message += `\n\n💡 ${ex.what}\n${ex.fix.replace(/`/g, '')}`;
      return { from, to, severity: d.level, message, source: d.code ?? 'rustc' };
    });
  view.dispatch(setDiagnostics(view.state, cm));
}

function showDiagnostics(diags: Diagnostic[] | CheckDiagnostic[], fromRun = true) {
  const tab = fromRun && runTab && tabs.includes(runTab) ? runTab : active;
  if (tab === active) applyLint(diags);
  else tab.pendingDiags = diags;

  const list = $('problem-list');
  list.innerHTML = '';
  for (const d of diags) {
    const li = document.createElement('li');
    li.className = `problem ${d.level}`;
    li.innerHTML = `<span class="icon">${d.level === 'error' ? '✕' : '!'}</span>
      <span class="msg"></span><span class="where">${d.line ? `line ${d.line}` : ''}</span>`;
    (li.querySelector('.msg') as HTMLElement).textContent = d.message + (d.label ? ` — ${d.label}` : '');
    const ex = prefs.explain ? explain(d.code, d.message) : null;
    if (ex) {
      const help = document.createElement('div');
      help.className = 'explain';
      help.innerHTML = `<b>💡 </b><span></span><br><em></em>`;
      (help.querySelector('span') as HTMLElement).textContent = ex.what;
      // `code` in the tips becomes code formatting.
      const em = help.querySelector('em') as HTMLElement;
      ex.fix.split(/(`[^`]+`)/).forEach((part) => {
        if (part.startsWith('`') && part.endsWith('`') && part.length > 1) {
          const c = document.createElement('code');
          c.textContent = part.slice(1, -1);
          em.appendChild(c);
        } else em.appendChild(document.createTextNode(part));
      });
      (li.querySelector('.msg') as HTMLElement).appendChild(help);
    }
    li.title = d.rendered.replace(/\x1b\[[0-9;]*m/g, '');
    if (d.line) {
      li.addEventListener('click', () => {
        if (tab !== active && tabs.includes(tab)) activate(tab);
        const pos = posOf(d.line, d.column);
        view.dispatch({ selection: { anchor: pos }, scrollIntoView: true });
        view.focus();
      });
    }
    list.appendChild(li);
  }
  const errors = diags.filter((d) => d.level === 'error').length;
  const badge = $('problem-count');
  badge.hidden = diags.length === 0;
  badge.textContent = String(diags.length);
  badge.classList.toggle('has-errors', errors > 0);
  $('no-problems').hidden = diags.length > 0;
  if (errors > 0 && fromRun) showTab('problems');
}

// ---------------------------------------------------------------- run

function setRunState(state: RunStateName, message: string) {
  runState = state;
  serialConnected = state === 'running' && message.startsWith('Serial monitor');
  const connectBtn = $<HTMLButtonElement>('btn-connect');
  connectBtn.textContent = serialConnected ? 'Disconnect' : 'Connect';
  connectBtn.classList.toggle('connected', serialConnected);
  connectBtn.disabled = state === 'building' || state === 'flashing';
  explorer?.setBusy(state === 'building' || state === 'flashing');
  const busy = state !== 'idle';
  $<HTMLButtonElement>('btn-run').disabled = state === 'building' || state === 'flashing';
  $<HTMLButtonElement>('btn-debug').disabled = state !== 'idle';
  $<HTMLButtonElement>('btn-stop').disabled = !busy;
  $<HTMLSelectElement>('target').disabled = busy;
  const input = $<HTMLInputElement>('input');
  input.disabled = state !== 'running';
  if (state === 'running') input.focus();
  const label: Record<RunStateName, string> = {
    building: 'Building…',
    flashing: 'Sending to the Pico…',
    running: 'Running',
    idle: 'Ready',
  };
  const el = $('status-state');
  el.textContent = message || label[state];
  el.className = `state ${state}`;
}

async function run() {
  if (runState === 'building' || runState === 'flashing') return;
  if (tools && !(target === 'desktop' ? cargoReady() : picoReady())) {
    openTools();
    return;
  }
  showTab('console');
  print('rule', `\n── Run ${fileName()} on ${targetLabel()} ──\n`);
  view.dispatch(setDiagnostics(view.state, []));
  runTab = active;
  plotter.clear();
  setRunState('building', '');
  userDisconnected = false;
  await invoke('run_program', { code: text(), name: fileName(), target, args: target === 'desktop' ? parseArgs($<HTMLInputElement>('args').value) : [] });
}

/** Splits arguments like a shell: spaces separate, quotes group. */
function parseArgs(line: string): string[] {
  const out: string[] = [];
  const re = /"([^"]*)"|'([^']*)'|(\S+)/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(line))) out.push(m[1] ?? m[2] ?? m[3]);
  return out;
}

async function stop() {
  userDisconnected = true;
  if (debugTab) debugEnded();
  await invoke('stop_program');
}

function targetLabel() {
  return $<HTMLSelectElement>('target').selectedOptions[0].textContent ?? target;
}

// ---------------------------------------------------------------- debugger

async function debug() {
  if (target !== 'desktop') {
    toast('Step-by-step debugging works for programs on this computer (a Pico needs a debug probe).', true);
    return;
  }
  if (runState === 'building' || runState === 'flashing') return;
  const lines = breakpointLines(view.state);
  showTab('console');
  print('rule', `\n── Debug ${fileName()} ──\n`);
  if (!lines.length) print('info', 'No breakpoint: click in the left margin of a line to stop there.\n');
  view.dispatch(setDiagnostics(view.state, []));
  runTab = active;
  debugTab = active;
  plotter.clear();
  clearDebugPanel('Starting the debugger…');
  setRunState('building', 'Building for the debugger…');
  userDisconnected = true; // the serial monitor stays away
  try {
    const started = await invoke<boolean>('debug_start', {
      code: text(),
      args: parseArgs($<HTMLInputElement>('args').value),
      breakpoints: lines,
    });
    if (!started) debugEnded();
    else $('dbg-controls').hidden = false;
  } catch (e) {
    print('error', `${e}\n`);
    debugEnded();
  }
}

function setStepButtons(enabled: boolean) {
  for (const id of ['dbg-continue', 'dbg-next', 'dbg-in', 'dbg-out']) $<HTMLButtonElement>(id).disabled = !enabled;
}

async function step(command: 'continue' | 'next' | 'stepIn' | 'stepOut') {
  if (!debugTab) return;
  setStepButtons(false);
  try {
    await invoke('debug_step', { command });
  } catch (e) {
    print('error', `${e}\n`);
  }
}

function debugEnded() {
  if (debugTab && debugTab === active) showDebugLine(view, null);
  debugTab = null;
  $('dbg-controls').hidden = true;
  clearDebugPanel('The program is not being debugged.');
}

// The board plugged in is not the one selected: offer to switch and run again.
void listen<{ board: TargetId; target: TargetId }>('board-mismatch', async (e) => {
  const names: Record<TargetId, string> = { desktop: 'this computer', pico: 'Raspberry Pi Pico (RP2040)', pico2: 'Raspberry Pi Pico 2 (RP2350)' };
  const answer = await choose(
    `This board is a ${names[e.payload.board]}`,
    `The target menu says ${names[e.payload.target]}. Nothing was written to the board.`,
    [
      ['cancel', 'Cancel', 'secondary'],
      ['switch', `Use ${names[e.payload.board]} and Run`, 'primary'],
    ],
  );
  if (answer === 'switch') {
    setTarget(e.payload.board, false);
    // Let the failed run finish before starting again.
    for (let i = 0; i < 50 && runState !== 'idle'; i++) await new Promise((r) => setTimeout(r, 100));
    void run();
  }
});

void listen<Stopped>('debug-stopped', (e) => {
  if (!debugTab) return;
  if (debugTab !== active && tabs.includes(debugTab)) activate(debugTab);
  const top = e.payload.frames.find((f) => f.in_user_code);
  showDebugLine(view, top ? top.line : null);
  renderStopped(e.payload, (f) => showDebugLine(view, f.line));
  setStepButtons(true);
  showTab('debugger');
});
void listen('debug-continued', () => {
  if (debugTab === active) showDebugLine(view, null);
  setStepButtons(false);
});
void listen('debug-ended', () => {
  if (!debugTab) return;
  debugEnded();
  setRunState('idle', 'Debugging finished');
});

$('btn-debug').addEventListener('click', debug);
$('dbg-continue').addEventListener('click', () => step('continue'));
$('dbg-next').addEventListener('click', () => step('next'));
$('dbg-in').addEventListener('click', () => step('stepIn'));
$('dbg-out').addEventListener('click', () => step('stepOut'));

$('btn-run').addEventListener('click', run);
$('btn-stop').addEventListener('click', stop);

// ---------------------------------------------------------------- files

function newFile() {
  const t = templateFor(target);
  openTab({ kind: 'untitled', name: untitledName() }, t, t);
}

/** Opens a file of the computer in a tab (dialog when no path is given). */
async function openLocal(path?: string) {
  if (!path) {
    const picked = await openDialog({ multiple: false, defaultPath: explorer?.localDir, filters: [{ name: 'Rust', extensions: ['rs'] }, { name: 'All files', extensions: ['*'] }] });
    if (!picked || Array.isArray(picked)) return;
    path = picked;
  }
  try {
    const content = await invoke<string>('read_text_file', { path });
    openTab({ kind: 'local', path }, content, content);
    followTargetOf(content);
  } catch (e) {
    toast(`Cannot open ${path.split(/[\\/]/).pop()}: ${e}`, true);
  }
}

/** Opens a file stored on the Pico in a tab (read over USB: no BOOTSEL, the program keeps running). */
async function openPico(name: string) {
  const existing = tabs.find((t) => sameDoc(t.doc, { kind: 'pico', name }));
  if (existing && isDirty(existing)) {
    activate(existing);
    return;
  }
  toast(`Opening ${name} from the Pico…`);
  try {
    const content = await invoke<string>('pico_fs_read_text', { name });
    openTab({ kind: 'pico', name }, content, content);
    followTargetOf(content);
    $('toast').hidden = true;
  } catch (e) {
    toast(`Cannot open ${name}: ${e}`, true);
    print('error', `Cannot open ${name} from the Pico: ${e}\n`);
  }
}

/** A #![no_std] file is a Pico program; anything else runs on the computer. */
function followTargetOf(content: string) {
  if (!/\.rs$/.test(fileName())) return;
  if (/#!\[no_std\]/.test(content) && target === 'desktop') setTarget('pico2', false);
  if (!/#!\[no_std\]/.test(content) && target !== 'desktop') setTarget('desktop', false);
}

async function saveToLocal(path?: string) {
  if (!path) {
    const picked = await saveDialog({
      defaultPath: explorer ? await invoke<string>('join_path', { dir: explorer.localDir, name: fileName() }) : fileName(),
      filters: [{ name: 'Rust', extensions: ['rs'] }],
    });
    if (!picked) return false;
    path = picked;
  }
  try {
    await invoke('write_text_file', { path, contents: text() });
  } catch (e) {
    toast(`Could not save: ${e}`, true);
    return false;
  }
  setDoc({ kind: 'local', path }, null, text());
  void explorer?.refreshLocal();
  return true;
}

async function saveToPico(name?: string) {
  if (!name) {
    name = (await askName('Save on the Pico as', fileName())) ?? undefined;
    if (!name) return false;
  }
  try {
    await invoke('pico_fs_write_text', { name, contents: text() });
  } catch (e) {
    print('error', `Could not save on the Pico: ${e}\n`);
    toast(`Could not save on the Pico: ${e}`, true);
    return false;
  }
  setDoc({ kind: 'pico', name }, null, text());
  void explorer?.refreshPico();
  return true;
}

async function saveFile(saveAs = false): Promise<boolean> {
  const doc = active.doc;
  if (!saveAs && doc.kind === 'local') return saveToLocal(doc.path);
  if (!saveAs && doc.kind === 'pico') return saveToPico(doc.name);
  // Like Thonny: ask where.
  const where = await askWhere();
  if (where === 'local') return saveToLocal();
  if (where === 'pico') return saveToPico();
  return false;
}

function askWhere(): Promise<'local' | 'pico' | null> {
  const d = $<HTMLDialogElement>('save-dialog');
  $('save-local-dir').textContent = explorer?.localDir ?? '';
  const picoOk = !!explorer?.picoConnected;
  $<HTMLButtonElement>('save-pico').disabled = !picoOk;
  $('save-pico-state').textContent = picoOk ? 'stored in the Pico\'s flash' : 'no Pico connected';
  return new Promise((resolve) => {
    const done = (v: 'local' | 'pico' | null) => {
      d.close();
      resolve(v);
    };
    $('save-local').onclick = () => done('local');
    $('save-pico').onclick = () => done('pico');
    $('save-cancel').onclick = () => done(null);
    d.oncancel = () => done(null);
    d.showModal();
  });
}

/** Small modal asking for a file name. */
export function askName(title: string, initial: string, validate?: (v: string) => string | null): Promise<string | null> {
  const d = $<HTMLDialogElement>('name-dialog');
  const input = $<HTMLInputElement>('name-input');
  $('name-title').textContent = title;
  $('name-error').textContent = '';
  input.value = initial;
  return new Promise((resolve) => {
    const finish = (v: string | null) => {
      d.close();
      resolve(v);
    };
    $('name-form').onsubmit = (e) => {
      e.preventDefault();
      const v = input.value.trim();
      const err = !v ? 'Please type a name.' : v.includes('/') || v.includes('\\') ? 'No slashes in names.' : validate?.(v) ?? null;
      if (err) {
        $('name-error').textContent = err;
        return;
      }
      finish(v);
    };
    $('name-cancel').onclick = () => finish(null);
    d.oncancel = () => finish(null);
    d.showModal();
    const dot = initial.lastIndexOf('.');
    input.setSelectionRange(0, dot > 0 ? dot : initial.length);
    input.focus();
  });
}

$('btn-new').addEventListener('click', newFile);
$('etab-new').addEventListener('click', newFile);
$('btn-open').addEventListener('click', () => openLocal());
$('btn-save').addEventListener('click', () => saveFile());

// ---------------------------------------------------------------- targets & examples

function setTarget(t: TargetId, swapTemplate = true) {
  const previous = target;
  target = t;
  store.set('target', t);
  $<HTMLSelectElement>('target').value = t;
  // Untouched starter code follows the target.
  if (swapTemplate && (previous === 'desktop') !== (t === 'desktop') && active.doc.kind === 'untitled' && text() === templateFor(previous)) {
    setText(templateFor(t));
    active.saved = text();
    renderTabs();
    updateTitle();
  }
  fillExamples();
  updateToolsPill();
  $('args-wrap').hidden = t !== 'desktop';
  $('btn-debug').hidden = t !== 'desktop';
  warmUp();
  scheduleCheck();
  setRunState(runState, $('status-state').textContent === 'Ready' ? '' : $('status-state').textContent ?? '');
}

$<HTMLSelectElement>('target').addEventListener('change', (e) => setTarget((e.target as HTMLSelectElement).value as TargetId));

function fillExamples() {
  const sel = $<HTMLSelectElement>('examples');
  sel.innerHTML = '<option value="">Examples…</option>';
  EXAMPLES.forEach((ex, i) => {
    if (!ex.targets.includes(target)) return;
    const o = document.createElement('option');
    o.value = String(i);
    o.textContent = ex.name;
    sel.appendChild(o);
  });
}

$<HTMLSelectElement>('examples').addEventListener('change', async (e) => {
  const sel = e.target as HTMLSelectElement;
  const ex = EXAMPLES[Number(sel.value)];
  sel.value = '';
  if (!ex) return;
  const base = ex.name.toLowerCase().replace(/[^a-z0-9]+/g, '_').replace(/^_|_$/g, '');
  openTab({ kind: 'untitled', name: untitledName(base) }, ex.code, '');
});

// ---------------------------------------------------------------- serial monitor

interface PortInfo {
  path: string;
  label: string;
  pico: boolean;
}
let ports: PortInfo[] = [];
let connecting = false;

function renderPorts() {
  const sel = $<HTMLSelectElement>('port');
  const current = sel.value;
  sel.innerHTML = '<option value="">Auto (Pico)</option>';
  for (const p of ports) {
    const o = document.createElement('option');
    o.value = p.path;
    o.textContent = p.label;
    sel.appendChild(o);
  }
  if (current && !ports.some((p) => p.path === current)) {
    // Keep the choice while the board is unplugged.
    const o = document.createElement('option');
    o.value = current;
    o.textContent = `${current} (not connected)`;
    sel.appendChild(o);
  }
  sel.value = current;
}

async function connectMonitor(auto = false) {
  if (connecting) return;
  connecting = true;
  const path = $<HTMLSelectElement>('port').value || null;
  const baud = Number($<HTMLSelectElement>('baud').value);
  try {
    if (!auto) showTab('console');
    await invoke('connect_serial', { path, baud });
    userDisconnected = false;
  } catch (e) {
    // Automatic attempts fail silently (board busy, rebooting…).
    if (!auto) print('error', `${e}\n`);
  } finally {
    connecting = false;
  }
}

$('btn-connect').addEventListener('click', async () => {
  if (serialConnected) await stop();
  else await connectMonitor();
});

// Watches the serial ports; reconnects the monitor when the board comes back.
async function pollPorts() {
  try {
    const list = await invoke<PortInfo[]>('list_ports');
    if (JSON.stringify(list) !== JSON.stringify(ports)) {
      ports = list;
      renderPorts();
    }
    const auto = $<HTMLInputElement>('auto-reconnect').checked;
    const chosen = $<HTMLSelectElement>('port').value;
    const available = chosen ? ports.some((p) => p.path === chosen) : ports.some((p) => p.pico);
    const wanted = chosen !== '' || target !== 'desktop';
    if (auto && wanted && available && !userDisconnected && runState === 'idle' && !connecting) {
      await connectMonitor(true);
    }
  } catch {
    /* ignore */
  }
  setTimeout(pollPorts, 1500);
}

// ---------------------------------------------------------------- tools

const cargoReady = () => !!tools?.cargo && !!tools?.linker;
const picoReady = () => cargoReady() && tools!.missing_targets.length === 0 && !!tools!.picotool;

function updateToolsPill() {
  const pill = $('btn-tools');
  if (!tools) {
    pill.textContent = 'Checking tools…';
    pill.className = 'pill';
    return;
  }
  const ok = target === 'desktop' ? cargoReady() : picoReady();
  pill.textContent = ok ? `✓ ${tools.cargo?.split(' ').slice(0, 2).join(' ')}` : '⚠ Install tools';
  pill.className = ok ? 'pill ok' : 'pill warn';
}

function renderToolList() {
  const items: [string, boolean, string][] = tools
    ? [
        ['Rust (cargo)', !!tools.cargo, tools.cargo ?? 'not installed'],
        ...(navigator.userAgent.includes('Mac') ? [['Apple Command Line Tools', tools.linker, tools.linker ? 'installed' : 'needed to build programs'] as [string, boolean, string]] : []),
        ['Pico targets', tools.missing_targets.length === 0, tools.missing_targets.length ? `missing: ${tools.missing_targets.join(', ')}` : 'thumbv6m, thumbv8m'],
        ['picotool', !!tools.picotool, tools.picotool ?? 'not installed (needed for the Pico)'],
        ['rust-analyzer', tools.analyzer, tools.analyzer ? 'completion and help on hover' : 'not installed (completion, help on hover)'],
      ]
    : [];
  $('tool-list').innerHTML = items
    .map(([name, ok, detail]) => `<li class="${ok ? 'ok' : 'missing'}"><span>${ok ? '✓' : '✕'}</span><b>${name}</b><em></em></li>`)
    .join('');
  $('tool-list').querySelectorAll('em').forEach((em, i) => (em.textContent = items[i][2]));
  const allOk = tools && picoReady() && tools.analyzer;
  $<HTMLButtonElement>('btn-install').hidden = !!allOk;
}

async function refreshTools() {
  tools = await invoke<ToolStatus>('tool_status');
  updateToolsPill();
  renderToolList();
}

function openTools() {
  renderToolList();
  const d = $<HTMLDialogElement>('install-dialog');
  if (!d.open) d.showModal();
}

$('btn-tools').addEventListener('click', openTools);
$('btn-close-dialog').addEventListener('click', () => $<HTMLDialogElement>('install-dialog').close());
$('btn-install').addEventListener('click', async () => {
  const btn = $<HTMLButtonElement>('btn-install');
  btn.disabled = true;
  btn.textContent = 'Installing…';
  $('install-log').textContent = '';
  try {
    tools = await invoke<ToolStatus>('install_tools');
  } catch (e) {
    $('install-log').textContent += `\n${e}\n`;
  }
  btn.disabled = false;
  btn.textContent = 'Install missing tools';
  await refreshTools();
});

// ---------------------------------------------------------------- crates

interface CrateInfo {
  name: string;
  version: string;
  description: string;
  downloads: number;
}

function renderCurrentCrates() {
  const list = $('crates-current');
  list.innerHTML = '';
  const deps = readDeps(text());
  if (!deps.length) list.innerHTML = '<li class="empty">No crates yet: only the standard library is used.</li>';
  for (const d of deps) {
    const li = document.createElement('li');
    li.innerHTML = `<div><b></b><span class="ver"></span></div><button>Remove</button>`;
    (li.querySelector('b') as HTMLElement).textContent = d.name;
    (li.querySelector('.ver') as HTMLElement).textContent = d.spec;
    (li.querySelector('button') as HTMLElement).onclick = () => {
      setText(removeDep(text(), d.name));
      renderCurrentCrates();
    };
    list.appendChild(li);
  }
}

function openCrates() {
  $('crates-file').textContent = fileName();
  $('crates-note').textContent =
    target === 'desktop'
      ? 'Crates are listed at the top of your file and downloaded on the next Run.'
      : 'The Pico has no standard library: choose crates that work with no_std (for example heapless, micromath).';
  renderCurrentCrates();
  $('crates-results').innerHTML = '';
  $<HTMLDialogElement>('crates-dialog').showModal();
  $<HTMLInputElement>('crates-query').focus();
}

$('crates-form').addEventListener('submit', async (e) => {
  e.preventDefault();
  const q = $<HTMLInputElement>('crates-query').value.trim();
  const list = $('crates-results');
  if (!q) return;
  list.innerHTML = '<li class="empty">Searching…</li>';
  try {
    const found = await invoke<CrateInfo[]>('crates_search', { query: q });
    list.innerHTML = found.length ? '' : '<li class="empty">Nothing found.</li>';
    for (const c of found) {
      const li = document.createElement('li');
      li.innerHTML = `<div><b></b><span class="ver"></span></div><span class="desc"></span><button class="add">Add</button>`;
      (li.querySelector('b') as HTMLElement).textContent = c.name;
      (li.querySelector('.ver') as HTMLElement).textContent = `${c.version} · ${c.downloads.toLocaleString()} downloads`;
      (li.querySelector('.desc') as HTMLElement).textContent = c.description;
      const btn = li.querySelector('button') as HTMLButtonElement;
      btn.onclick = () => {
        setText(setDep(text(), c.name, c.version));
        renderCurrentCrates();
        btn.textContent = 'Added';
        btn.disabled = true;
      };
      list.appendChild(li);
    }
  } catch (err) {
    list.innerHTML = '';
    const li = document.createElement('li');
    li.className = 'empty';
    li.textContent = `Search failed: ${err}`;
    list.appendChild(li);
  }
});
$('btn-crates').addEventListener('click', openCrates);
$('crates-close').addEventListener('click', () => $<HTMLDialogElement>('crates-dialog').close());

// ---------------------------------------------------------------- preferences dialog, recent, args

function openPrefs() {
  $<HTMLSelectElement>('pref-theme').value = prefs.theme;
  $<HTMLInputElement>('pref-font').value = String(prefs.fontSize);
  $<HTMLInputElement>('pref-check').checked = prefs.liveCheck;
  $<HTMLInputElement>('pref-assist').checked = prefs.assist;
  $<HTMLInputElement>('pref-explain').checked = prefs.explain;
  $<HTMLDialogElement>('prefs-dialog').showModal();
}

const prefInputs = ['pref-theme', 'pref-font', 'pref-check', 'pref-assist', 'pref-explain'];
for (const id of prefInputs) {
  $(id).addEventListener('input', () => {
    prefs.theme = $<HTMLSelectElement>('pref-theme').value as Prefs['theme'];
    prefs.fontSize = Number($<HTMLInputElement>('pref-font').value);
    prefs.liveCheck = $<HTMLInputElement>('pref-check').checked;
    prefs.assist = $<HTMLInputElement>('pref-assist').checked;
    prefs.explain = $<HTMLInputElement>('pref-explain').checked;
    applyPrefs();
    if (!prefs.liveCheck) showDiagnostics([], false);
  });
}
$('btn-prefs').addEventListener('click', openPrefs);
$('prefs-close').addEventListener('click', () => $<HTMLDialogElement>('prefs-dialog').close());

$<HTMLSelectElement>('recent').addEventListener('change', (e) => {
  const sel = e.target as HTMLSelectElement;
  const d = recentList()[Number(sel.value)];
  sel.value = '';
  if (d?.kind === 'local') void openLocal(d.path);
  else if (d?.kind === 'pico') void openPico(d.name);
});

$<HTMLInputElement>('args').value = store.get('args') ?? '';
$('args').addEventListener('input', () => store.set('args', $<HTMLInputElement>('args').value));

// ---------------------------------------------------------------- events & keys

void listen<{ kind: string; text: string }>('console', (e) => print(e.payload.kind, e.payload.text));
void listen<{ state: RunStateName; message: string }>('run-state', (e) => setRunState(e.payload.state, e.payload.message));
void listen<Diagnostic[]>('diagnostics', (e) => showDiagnostics(e.payload, true));
void listen<string>('install-log', (e) => {
  const log = $('install-log');
  log.textContent += e.payload + '\n';
  log.scrollTop = log.scrollHeight;
});

window.addEventListener('keydown', (e) => {
  const mod = e.metaKey || e.ctrlKey;
  if (e.key === 'F5' && e.ctrlKey) {
    e.preventDefault();
    void debug();
  } else if (e.key === 'F6' && debugTab) {
    e.preventDefault();
    void step('next');
  } else if (e.key === 'F7' && debugTab) {
    e.preventDefault();
    void step(e.shiftKey ? 'stepOut' : 'stepIn');
  } else if (e.key === 'F8' && debugTab) {
    e.preventDefault();
    void step('continue');
  } else if (e.key === 'F5' || (mod && e.key.toLowerCase() === 'r')) {
    e.preventDefault();
    void run();
  } else if (mod && e.key === '.') {
    e.preventDefault();
    void stop();
  } else if (mod && e.key.toLowerCase() === 's') {
    e.preventDefault();
    void saveFile(e.shiftKey);
  } else if (mod && e.key.toLowerCase() === 'o') {
    e.preventDefault();
    void openLocal();
  } else if (mod && e.key.toLowerCase() === 'n') {
    e.preventDefault();
    newFile();
  } else if (mod && (e.key === '=' || e.key === '+')) {
    e.preventDefault();
    prefs.fontSize++;
    applyPrefs();
  } else if (mod && e.key === '-') {
    e.preventDefault();
    prefs.fontSize--;
    applyPrefs();
  } else if (mod && e.key === '0') {
    e.preventDefault();
    prefs.fontSize = 14;
    applyPrefs();
  } else if (mod && e.key === ',') {
    e.preventDefault();
    openPrefs();
  } else if (mod && e.key.toLowerCase() === 'w') {
    e.preventDefault();
    void closeTab(active);
  } else if ((e.ctrlKey && e.key === 'Tab') || (mod && e.shiftKey && (e.key === ']' || e.key === '[' || e.key === '}' || e.key === '{'))) {
    e.preventDefault();
    const back = e.key === '[' || e.key === '{' || (e.key === 'Tab' && e.shiftKey);
    activate(tabs[(tabs.indexOf(active) + (back ? -1 : 1) + tabs.length) % tabs.length]);
  }
});

// ---------------------------------------------------------------- splitter

(() => {
  const splitter = $('splitter');
  const panel = document.querySelector<HTMLElement>('.panel')!;
  const saved = Number(store.get('panel'));
  if (saved > 80) panel.style.height = `${saved}px`;
  splitter.addEventListener('pointerdown', (e) => {
    splitter.setPointerCapture(e.pointerId);
    const startY = e.clientY;
    const startH = panel.offsetHeight;
    const move = (ev: PointerEvent) => {
      const h = Math.max(90, Math.min(window.innerHeight - 160, startH - (ev.clientY - startY)));
      panel.style.height = `${h}px`;
    };
    const up = () => {
      splitter.removeEventListener('pointermove', move);
      store.set('panel', String(panel.offsetHeight));
    };
    splitter.addEventListener('pointermove', move);
    splitter.addEventListener('pointerup', up, { once: true });
  });
})();

// ---------------------------------------------------------------- start

explorer = new Explorer({
  print,
  openLocal: (path) => openLocal(path),
  openPico: (name) => openPico(name),
  askName,
  doc: () => active.doc,
  docMoved: (from, to) => {
    const tab = tabs.find((t) => sameDoc(t.doc, from));
    if (!tab) return;
    if (tab !== active) activate(tab);
    if (to) setDoc(to, null, tab.saved);
    else setDoc({ kind: 'untitled', name: fileName(tab) }, null, '');
  },
  toast,
});
void explorer.start();
(window as unknown as { __gauthier: unknown }).__gauthier = { explorer, plotter, view };

$<HTMLSelectElement>('target').value = target;
restoreTabs();
fillExamples();
fillRecent();
$('args-wrap').hidden = target !== 'desktop';
$('btn-debug').hidden = target !== 'desktop';
updateTitle();
setRunState('idle', '');
void pollPorts();
print('info', 'Welcome to Gauthier. Write your program above and press Run (⌘R).\n');
if (selftestMode) void selftest(selftestMode, (s) => setText(s), () => text());
void refreshTools().then(() => {
  if (!cargoReady()) openTools();
  warmUp();
  scheduleCheck();
});

/** Starts rust-analyzer early: the first completion is then instant. */
function warmUp() {
  if (assistHost.ready() && fileName().endsWith('.rs')) void invoke('analyzer_warm_up', { code: text(), target }).catch(() => undefined);
}
