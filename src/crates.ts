// Crates (dependencies) of a single-file program, stored at the top of the file:
//
//   //! ```cargo
//   //! [dependencies]
//   //! rand = "0.9"
//   //! ```

export interface Dep {
  name: string;
  spec: string; // what follows "=" (e.g. "0.9" or { version = "1", features = [...] })
}

const BLOCK = /^(?:[ \t]*(?:\/\/(?!!).*|#!\[.*\]|)\n)*?([ \t]*\/\/!\s*```cargo\n(?:[ \t]*\/\/!.*\n)*?[ \t]*\/\/!\s*```[ \t]*\n?)/;

/** Dependencies declared in the file. */
export function readDeps(code: string): Dep[] {
  const m = BLOCK.exec(code);
  if (!m) return [];
  const deps: Dep[] = [];
  let inDeps = false;
  for (const raw of m[1].split('\n')) {
    const line = raw.replace(/^\s*\/\/!\s?/, '').trim();
    if (line.startsWith('[')) {
      inDeps = line === '[dependencies]';
      continue;
    }
    const kv = /^([A-Za-z0-9_-]+)\s*=\s*(.+)$/.exec(line);
    if (inDeps && kv) deps.push({ name: kv[1], spec: kv[2] });
  }
  return deps;
}

/** Returns the code with its dependency block replaced (removed when `deps` is empty). */
export function writeDeps(code: string, deps: Dep[]): string {
  const m = BLOCK.exec(code);
  let rest = code;
  let at = 0;
  if (m) {
    at = m.index + m[0].length - m[1].length;
    rest = code.slice(0, at) + code.slice(at + m[1].length);
  }
  if (!deps.length) return rest;
  const block =
    ['//! ```cargo', '//! [dependencies]', ...deps.map((d) => `//! ${d.name} = ${d.spec}`), '//! ```'].join('\n') + '\n';
  return rest.slice(0, at) + block + rest.slice(at);
}

export function setDep(code: string, name: string, version: string): string {
  const deps = readDeps(code).filter((d) => d.name !== name);
  deps.push({ name, spec: `"${version}"` });
  deps.sort((a, b) => a.name.localeCompare(b.name));
  return writeDeps(code, deps);
}

export function removeDep(code: string, name: string): string {
  return writeDeps(
    code,
    readDeps(code).filter((d) => d.name !== name),
  );
}
