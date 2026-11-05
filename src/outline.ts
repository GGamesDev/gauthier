// Outline of the open file (like Thonny's "Outline" view): fn, struct, enum, impl, trait…

import { ensureSyntaxTree } from '@codemirror/language';
import type { EditorState } from '@codemirror/state';
import type { SyntaxNode } from '@lezer/common';

export interface OutlineItem {
  kind: string;
  name: string;
  depth: number;
  from: number;
  line: number;
}

const KINDS: Record<string, string> = {
  FunctionItem: 'fn',
  StructItem: 'struct',
  EnumItem: 'enum',
  ImplItem: 'impl',
  TraitItem: 'trait',
  ConstItem: 'const',
  StaticItem: 'static',
  ModItem: 'mod',
  TypeItem: 'type',
  UnionItem: 'union',
};

function nameOf(node: SyntaxNode, state: EditorState): string {
  const text = (n: SyntaxNode) => state.doc.sliceString(n.from, n.to);
  if (node.name === 'ImplItem') {
    // "impl Display for Point" → "Display for Point"; "impl Point" → "Point"
    const header = text(node).split('{')[0].replace(/^\s*(unsafe\s+)?impl(<[^>]*>)?\s*/, '');
    return header.replace(/\s+where[\s\S]*$/, '').replace(/\s+/g, ' ').trim();
  }
  for (let c = node.firstChild; c; c = c.nextSibling) {
    if (c.name === 'BoundIdentifier' || c.name === 'TypeIdentifier' || c.name === 'Identifier') return text(c);
  }
  return '?';
}

export function outline(state: EditorState): OutlineItem[] {
  const tree = ensureSyntaxTree(state, state.doc.length, 300);
  if (!tree) return [];
  const items: OutlineItem[] = [];
  let depth = 0;
  tree.iterate({
    enter: (n) => {
      const kind = KINDS[n.name];
      if (!kind) return;
      items.push({ kind, name: nameOf(n.node, state), depth, from: n.from, line: state.doc.lineAt(n.from).number });
      // Children of impl / trait / mod are shown indented; function bodies are not explored.
      if (kind === 'fn') return false;
      depth++;
    },
    leave: (n) => {
      const kind = KINDS[n.name];
      if (kind && kind !== 'fn') depth--;
    },
  });
  return items;
}
