// Debugger UI: breakpoints gutter, current line, call stack and variables.

import { invoke } from '@tauri-apps/api/core';
import { RangeSet, StateEffect, StateField, type EditorState, type Extension } from '@codemirror/state';
import { Decoration, DecorationSet, EditorView, gutter, GutterMarker } from '@codemirror/view';

export interface Frame {
  id: number;
  name: string;
  line: number;
  in_user_code: boolean;
}
export interface Variable {
  name: string;
  value: string;
  type_name: string | null;
  reference: number;
}
export interface Stopped {
  reason: string;
  frames: Frame[];
  variables: Variable[];
}

// ------------------------------------------------------------ breakpoints

const toggleBreakpoint = StateEffect.define<{ pos: number; on: boolean }>({
  map: (v, m) => ({ pos: m.mapPos(v.pos), on: v.on }),
});

class BreakpointMarker extends GutterMarker {
  toDOM() {
    const d = document.createElement('div');
    d.className = 'bp-dot';
    return d;
  }
}
const bpMarker = new BreakpointMarker();

export const breakpointField = StateField.define<RangeSet<GutterMarker>>({
  create: () => RangeSet.empty,
  update(set, tr) {
    set = set.map(tr.changes);
    for (const e of tr.effects) {
      if (e.is(toggleBreakpoint)) {
        if (e.value.on) set = set.update({ add: [bpMarker.range(e.value.pos)] });
        else set = set.update({ filter: (from) => from !== e.value.pos });
      }
    }
    return set;
  },
});

export function breakpointLines(state: EditorState): number[] {
  const lines: number[] = [];
  const iter = state.field(breakpointField, false)?.iter();
  if (!iter) return lines;
  while (iter.value) {
    lines.push(state.doc.lineAt(iter.from).number);
    iter.next();
  }
  return [...new Set(lines)].sort((a, b) => a - b);
}

export function toggleBreakpointAt(view: EditorView, linePos: number) {
  const line = view.state.doc.lineAt(linePos);
  let has = false;
  view.state.field(breakpointField).between(line.from, line.from, () => {
    has = true;
  });
  view.dispatch({ effects: toggleBreakpoint.of({ pos: line.from, on: !has }) });
}

// ------------------------------------------------------------ current line

const setDebugLine = StateEffect.define<number | null>();
const debugLineDeco = Decoration.line({ class: 'cm-debug-line' });

const debugLineField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(deco, tr) {
    deco = deco.map(tr.changes);
    for (const e of tr.effects) {
      if (e.is(setDebugLine)) {
        deco = e.value === null ? Decoration.none : Decoration.set([debugLineDeco.range(tr.state.doc.line(e.value).from)]);
      }
    }
    return deco;
  },
  provide: (f) => EditorView.decorations.from(f),
});

export function showDebugLine(view: EditorView, line: number | null) {
  if (line !== null && (line < 1 || line > view.state.doc.lines)) line = null;
  view.dispatch({
    effects: setDebugLine.of(line),
    ...(line !== null ? { selection: { anchor: view.state.doc.line(line).from }, scrollIntoView: true } : {}),
  });
}

export function debugExtensions(onToggle: (view: EditorView) => void): Extension[] {
  return [
    breakpointField,
    debugLineField,
    gutter({
      class: 'cm-breakpoint-gutter',
      markers: (v) => v.state.field(breakpointField),
      initialSpacer: () => bpMarker,
      domEventHandlers: {
        mousedown(view, line) {
          toggleBreakpointAt(view, line.from);
          return true;
        },
      },
    }),
    // Any change of breakpoints (click, code) is sent to the running debugger.
    EditorView.updateListener.of((u) => {
      if (u.transactions.some((tr) => tr.effects.some((e) => e.is(toggleBreakpoint)))) onToggle(u.view);
    }),
    EditorView.baseTheme({
      '.cm-breakpoint-gutter': { width: '14px', cursor: 'pointer' },
      '.cm-breakpoint-gutter .cm-gutterElement': { display: 'flex', alignItems: 'center', justifyContent: 'center' },
      '.bp-dot': { width: '9px', height: '9px', borderRadius: '50%', background: '#e5484d' },
      '.cm-debug-line': { backgroundColor: 'rgba(255, 196, 0, 0.28) !important' },
    }),
  ];
}

// ------------------------------------------------------------ panel

const $ = (id: string) => document.getElementById(id)!;

export function renderStopped(s: Stopped, onFrame: (f: Frame) => void) {
  const stack = $('dbg-stack');
  stack.innerHTML = '';
  s.frames.forEach((f, i) => {
    const li = document.createElement('li');
    li.className = (f.in_user_code ? 'mine' : 'other') + (i === 0 ? ' top' : '');
    li.innerHTML = `<span class="fn"></span><span class="ln"></span>`;
    (li.querySelector('.fn') as HTMLElement).textContent = f.name;
    (li.querySelector('.ln') as HTMLElement).textContent = f.in_user_code ? `line ${f.line}` : '';
    if (f.in_user_code) li.onclick = () => onFrame(f);
    stack.appendChild(li);
  });
  const vars = $('dbg-vars');
  vars.innerHTML = '';
  if (!s.variables.length) vars.innerHTML = '<li class="empty">No local variables here.</li>';
  for (const v of s.variables) vars.appendChild(variableItem(v));
  $('dbg-reason').textContent = reasonText(s.reason);
}

function reasonText(r: string) {
  if (r === 'breakpoint') return 'Stopped at a breakpoint';
  if (r === 'step') return 'Paused after a step';
  if (r === 'exception' || r === 'signal') return 'Stopped: the program crashed (panic?)';
  return `Paused (${r})`;
}

function variableItem(v: Variable): HTMLElement {
  const li = document.createElement('li');
  const row = document.createElement('div');
  row.className = 'var-row';
  row.innerHTML = `<span class="twisty"></span><span class="vn"></span><span class="vv"></span><span class="vt"></span>`;
  (row.querySelector('.vn') as HTMLElement).textContent = v.name;
  (row.querySelector('.vv') as HTMLElement).textContent = v.value;
  (row.querySelector('.vt') as HTMLElement).textContent = v.type_name ?? '';
  li.appendChild(row);
  if (v.reference > 0) {
    li.classList.add('has-children');
    const children = document.createElement('ul');
    children.hidden = true;
    li.appendChild(children);
    let loaded = false;
    row.onclick = async () => {
      const open = children.hidden;
      li.classList.toggle('open', open);
      children.hidden = !open;
      if (open && !loaded) {
        loaded = true;
        try {
          const kids = await invoke<Variable[]>('debug_variables', { reference: v.reference });
          for (const k of kids) children.appendChild(variableItem(k));
          if (!kids.length) children.innerHTML = '<li class="empty">empty</li>';
        } catch (e) {
          children.textContent = String(e);
        }
      }
    };
  }
  return li;
}

export function clearDebugPanel(message: string) {
  $('dbg-stack').innerHTML = '';
  $('dbg-vars').innerHTML = '';
  $('dbg-reason').textContent = message;
}
