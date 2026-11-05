// Minimal ANSI SGR → HTML (enough for rustc diagnostics and simple colored output).

const COLORS = ['black', 'red', 'green', 'yellow', 'blue', 'magenta', 'cyan', 'white'];

function escape(s: string) {
  return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

export function ansiToHtml(input: string): string {
  let out = '';
  let classes: string[] = [];
  let open = false;
  const re = /\x1b\[([0-9;]*)m/g;
  let last = 0;
  let m: RegExpExecArray | null;
  const flush = (text: string) => {
    if (!text) return;
    out += classes.length ? `<span class="${classes.join(' ')}">${escape(text)}</span>` : escape(text);
  };
  while ((m = re.exec(input))) {
    flush(input.slice(last, m.index));
    last = re.lastIndex;
    const codes = m[1] === '' ? [0] : m[1].split(';').map(Number);
    for (let i = 0; i < codes.length; i++) {
      const c = codes[i];
      if (c === 0) classes = [];
      else if (c === 1) classes.push('a-bold');
      else if (c === 2) classes.push('a-dim');
      else if (c === 3) classes.push('a-italic');
      else if (c === 4) classes.push('a-underline');
      else if (c >= 30 && c <= 37) classes = classes.filter((x) => !x.startsWith('a-fg')).concat(`a-fg-${COLORS[c - 30]}`);
      else if (c >= 90 && c <= 97) classes = classes.filter((x) => !x.startsWith('a-fg')).concat(`a-fg-${COLORS[c - 90]}`, 'a-bright');
      else if (c === 39) classes = classes.filter((x) => !x.startsWith('a-fg'));
      else if (c === 38 && codes[i + 1] === 5) {
        // 256 colors: map the first 16, ignore the rest.
        const n = codes[i + 2] ?? 7;
        classes = classes.filter((x) => !x.startsWith('a-fg')).concat(`a-fg-${COLORS[n % 8]}`);
        i += 2;
      }
    }
    void open;
  }
  flush(input.slice(last));
  // Strip any other escape sequence (cursor moves…).
  return out.replace(/\x1b\[[0-9;?]*[A-Za-z]/g, '');
}
