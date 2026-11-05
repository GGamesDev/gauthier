// Editor help: completion and hover (rust-analyzer), checking as you type (cargo check).

import { invoke } from '@tauri-apps/api/core';
import type { CompletionContext, CompletionResult } from '@codemirror/autocomplete';
import { EditorView, hoverTooltip, Tooltip } from '@codemirror/view';
import type { Extension } from '@codemirror/state';

export interface AssistHost {
  target(): string;
  /** Only Rust files get help. */
  isRust(): boolean;
  ready(): boolean;
}

interface Item {
  label: string;
  detail: string | null;
  kind: number;
  insert: string;
  from: number | null;
}

// LSP CompletionItemKind → CodeMirror completion type (icon).
const KIND: Record<number, string> = {
  2: 'method', 3: 'function', 4: 'function', 5: 'property', 6: 'variable', 7: 'class', 8: 'interface',
  9: 'namespace', 10: 'property', 12: 'constant', 13: 'enum', 14: 'keyword', 15: 'text', 20: 'enum',
  21: 'constant', 22: 'class', 23: 'type', 25: 'type',
};

/** Help on hover. */
export function assistExtensions(host: AssistHost): Extension[] {
  const hover = hoverTooltip(async (view: EditorView, pos: number): Promise<Tooltip | null> => {
    if (!host.isRust() || !host.ready()) return null;
    const line = view.state.doc.lineAt(pos);
    let text: string | null;
    try {
      text = await invoke<string | null>('analyzer_hover', {
        code: view.state.doc.toString(),
        target: host.target(),
        line: line.number - 1,
        character: pos - line.from,
      });
    } catch {
      return null;
    }
    if (!text) return null;
    return {
      pos,
      above: true,
      create: () => {
        const dom = document.createElement('div');
        dom.className = 'hover-doc';
        renderMarkdown(dom, text!);
        return { dom };
      },
    };
  }, { hoverTime: 450 });

  return [hover];
}

/** Completion source (registered through the Rust language data). */
export function completionSource(host: AssistHost): (ctx: CompletionContext) => Promise<CompletionResult | null> {
  {
    return async (ctx) => {
      if (!host.isRust() || !host.ready()) return null;
      const word = ctx.matchBefore(/[A-Za-z_][\w]*/);
      const trigger = ctx.matchBefore(/(\.|::)$/);
      if (!word && !trigger && !ctx.explicit) return null;
      const pos = ctx.pos;
      const line = ctx.state.doc.lineAt(pos);
      let items: Item[];
      try {
        items = await invoke<Item[]>('analyzer_complete', {
          code: ctx.state.doc.toString(),
          target: host.target(),
          line: line.number - 1,
          character: pos - line.from,
        });
      } catch {
        return null;
      }
      if (ctx.aborted || !items.length) return null;
      const from = items[0].from !== null ? line.from + items[0].from : word ? word.from : pos;
      return {
        from,
        options: items.map((it, i) => ({
          label: it.label.replace(/\(alias .*\)$/, '').trim(),
          detail: it.detail ?? undefined,
          type: KIND[it.kind] ?? 'text',
          apply: it.insert,
          boost: -i / 1000,
        })),
        validFor: /^[\w]*$/,
      };
    };
  }
}

/** Small markdown renderer for rust-analyzer's hover text (code blocks, paragraphs, `code`). */
export function renderMarkdown(root: HTMLElement, md: string) {
  const fence = /```[\w-]*\n([\s\S]*?)```/g;
  let last = 0;
  let m: RegExpExecArray | null;
  const text = (chunk: string) => {
    const clean = chunk.replace(/^\s*---\s*$/gm, '').trim();
    if (!clean) return;
    for (const para of clean.split(/\n{2,}/)) {
      const p = document.createElement('p');
      para.split(/(`[^`]+`)/).forEach((piece) => {
        if (piece.length > 1 && piece.startsWith('`') && piece.endsWith('`')) {
          const c = document.createElement('code');
          c.textContent = piece.slice(1, -1);
          p.appendChild(c);
        } else {
          p.appendChild(document.createTextNode(piece.replace(/\[([^\]]+)\]\([^)]*\)/g, '$1').replace(/\*\*|__/g, '')));
        }
      });
      root.appendChild(p);
    }
  };
  while ((m = fence.exec(md))) {
    text(md.slice(last, m.index));
    const pre = document.createElement('pre');
    pre.textContent = m[1].replace(/\n$/, '');
    root.appendChild(pre);
    last = fence.lastIndex;
  }
  text(md.slice(last));
}

// ---------------------------------------------------------------- checking as you type

export interface CheckDiagnostic {
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

export class LiveChecker {
  private timer: number | undefined;
  private seq = 0;

  constructor(
    private run: (code: string, target: string) => Promise<{ done: boolean; diagnostics: CheckDiagnostic[] }>,
    private onResult: (code: string, diags: CheckDiagnostic[]) => void,
    private onState: (checking: boolean) => void,
  ) {}

  /** Called on every edit; checks 900 ms after the last keystroke. */
  schedule(code: () => string, target: () => string, enabled: () => boolean) {
    window.clearTimeout(this.timer);
    this.timer = window.setTimeout(async () => {
      if (!enabled()) return;
      const mine = ++this.seq;
      const text = code();
      this.onState(true);
      try {
        const r = await this.run(text, target());
        if (r.done && mine === this.seq) this.onResult(text, r.diagnostics);
      } catch {
        /* tools missing etc.: Run will report it */
      } finally {
        if (mine === this.seq) this.onState(false);
      }
    }, 900);
  }
}
