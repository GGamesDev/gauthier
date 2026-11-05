// Scripted scenario run inside the real app (GAUTHIER_SELFTEST=desktop|pico|pico2).
import { invoke } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { acceptCompletion, startCompletion } from '@codemirror/autocomplete';
import type { EditorView } from '@codemirror/view';
import { toggleBreakpointAt } from './debugui';

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

async function until(what: string, cond: () => boolean, ms: number) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    if (cond()) return;
    await sleep(100);
  }
  throw new Error(`timeout waiting for ${what}`);
}

const consoleText = () => $('output').textContent ?? '';
const state = () => $('status-state').className;

function pickExample(name: string): Promise<void> {
  const sel = $<HTMLSelectElement>('examples');
  const opt = [...sel.options].find((o) => o.textContent === name);
  if (!opt) throw new Error(`example not found: ${name}`);
  sel.value = opt.value;
  window.confirm = () => true;
  sel.dispatchEvent(new Event('change'));
  return sleep(300).then(() => undefined); // the handler is async
}

function setTarget(t: string) {
  const sel = $<HTMLSelectElement>('target');
  sel.value = t;
  sel.dispatchEvent(new Event('change'));
}

async function type(line: string) {
  const input = $<HTMLInputElement>('input');
  input.value = line;
  $('input-form').dispatchEvent(new Event('submit', { cancelable: true }));
}

export async function selftest(mode: string, setCode: (s: string) => void, getCode: () => string) {
  const log: string[] = [];
  const step = async (name: string, fn: () => Promise<void>) => {
    const t0 = Date.now();
    try {
      await fn();
      log.push(`PASS ${name} (${Date.now() - t0} ms)`);
    } catch (e) {
      log.push(`FAIL ${name}: ${e}\n--- console ---\n${consoleText().slice(-1500)}`);
      throw e;
    }
  };
  const win = getCurrentWindow();
  try {
    await sleep(1500); // tools check
    if (mode === 'desktop') {
      setTarget('desktop');
      await step('interactive program: Run, read stdin, print', async () => {
        await pickExample('Ask for your name');
        $('btn-run').click();
        await until('question', () => consoleText().includes('What is your name?'), 120000);
        await type('Ada');
        await until('answer', () => consoleText().includes('Nice to meet you, Ada!'), 10000);
        await until('finished', () => consoleText().includes('Program finished.'), 10000);
      });
      await step('compile error appears in Problems and in the editor', async () => {
        setCode('fn main() {\n    let x: u32 = "five";\n    println!("{x}");\n}\n');
        $('btn-run').click();
        await until('problem listed', () => $('problem-list').children.length > 0, 120000);
        const first = $('problem-list').textContent ?? '';
        if (!/line 2/.test(first)) throw new Error(`unexpected problem: ${first}`);
        await until('lint marker', () => document.querySelectorAll('.cm-lintRange-error').length > 0, 3000);
        log.push(`     problem: ${first.replace(/\s+/g, ' ').trim()}`);
      });
      const g = (window as unknown as { __gauthier: { plotter: { snapshot(): Record<string, number> }; view: EditorView } }).__gauthier;

      await step('Plotter draws the numbers printed by the program', async () => {
        setCode('fn main() {\n    for i in 0..20 {\n        let t = 20.0 + i as f64 * 0.5;\n        println!("temp: {t} hum: {}", 40 + i);\n    }\n}\n');
        $('btn-run').click();
        await until('finished', () => consoleText().split('── Run').pop()!.includes('Program finished.'), 120000);
        document.querySelector<HTMLElement>('.tab[data-tab="plotter"]')!.click();
        const snap = g.plotter.snapshot();
        if (snap.temp !== 29.5 || snap.hum !== 59) throw new Error('plotter: ' + JSON.stringify(snap));
        await until('legend', () => ($('plot-legend').textContent ?? '').includes('temp'), 3000);
        log.push(`     series: ${JSON.stringify(snap)}`);
        document.querySelector<HTMLElement>('.tab[data-tab="console"]')!.click();
      });

      await step('errors appear while typing (no Run)', async () => {
        document.querySelectorAll('.cm-lintRange-error').length;
        setCode('fn main() {\n    let n: i32 = "not a number";\n    println!("{n}");\n}\n');
        await until('lint marker', () => document.querySelectorAll('.cm-lintRange-error').length > 0, 60000);
        const first = $('problem-list').textContent ?? '';
        if (!/mismatched types/.test(first)) throw new Error('problems: ' + first);
        setCode('fn main() {\n    let n: i32 = 5;\n    println!("{n}");\n}\n');
        await until('markers cleared', () => document.querySelectorAll('.cm-lintRange-error').length === 0, 60000);
      });

      await step('completion from rust-analyzer (v.pu → push)', async () => {
        const code = 'fn main() {\n    let mut v: Vec<i32> = Vec::new();\n    v.pu\n    println!("{v:?}");\n}\n';
        setCode(code);
        const pos = code.indexOf('v.pu') + 4;
        g.view.dispatch({ selection: { anchor: pos } });
        g.view.focus();
        let ok = false;
        for (let attempt = 0; attempt < 30 && !ok; attempt++) {
          startCompletion(g.view);
          try {
            await until('popup', () => [...document.querySelectorAll('.cm-tooltip-autocomplete li')].some((li) => li.textContent?.includes('push')), 3000);
            ok = true;
          } catch {
            await sleep(500); // rust-analyzer still loading
          }
        }
        if (!ok) throw new Error('push not offered');
        const options = [...document.querySelectorAll('.cm-tooltip-autocomplete li')].map((li) => li.textContent).slice(0, 4);
        // Accept once the popup is settled (no request still pending).
        for (let k = 0; k < 20 && !getCode().includes('v.push'); k++) {
          await sleep(150);
          acceptCompletion(g.view);
        }
        await until('inserted', () => getCode().includes('v.push'), 3000);
        log.push(`     first suggestions: ${options.join(' | ')}`);
      });

      await step('help on hover (Vec)', async () => {
        const code = 'fn main() {\n    let v: Vec<i32> = Vec::new();\n    println!("{v:?}");\n}\n';
        setCode(code);
        await sleep(300);
        const pos = code.indexOf('Vec<') + 1;
        let shown = '';
        for (let attempt = 0; attempt < 10 && !shown; attempt++) {
          const c = g.view.coordsAtPos(pos)!;
          const target = document.elementFromPoint(c.left + 2, (c.top + c.bottom) / 2) ?? g.view.contentDOM;
          target.dispatchEvent(new MouseEvent('mousemove', { bubbles: true, clientX: c.left + 2, clientY: (c.top + c.bottom) / 2 }));
          try {
            await until('hover', () => !!document.querySelector('.hover-doc'), 3000);
            shown = document.querySelector('.hover-doc')!.textContent ?? '';
          } catch {
            await sleep(300);
          }
        }
        if (!shown.includes('Vec')) throw new Error('hover: ' + shown.slice(0, 100));
        log.push(`     hover: ${shown.replace(/\s+/g, ' ').slice(0, 70)}…`);
      });

      const gx = (window as unknown as { __gauthier: { explorer: { setLocalDir(d: string): void; localDir: string } } }).__gauthier;
      (window as unknown as { __gauthierAutoConfirm: boolean }).__gauthierAutoConfirm = true;
      const runAndWait = async (label: string) => {
        const before = consoleText().split('── Run').length;
        $('btn-run').click();
        await until(`${label} started`, () => consoleText().split('── Run').length > before, 5000);
        await until(`${label} finished`, () => /Program finished\.|Program exited|error\[|could not compile/.test(consoleText().split('── Run').pop()!), 240000);
        return consoleText().split('── Run').pop()!;
      };
      const key = (k: string, opts: KeyboardEventInit = {}) => window.dispatchEvent(new KeyboardEvent('keydown', { key: k, metaKey: true, bubbles: true, ...opts }));

      await step('zoom (⌘+ / ⌘0) and theme preference', async () => {
        const size = () => getComputedStyle(document.documentElement).getPropertyValue('--font-size').trim();
        key('0');
        const base = size();
        key('=');
        key('=');
        if (size() !== `${parseInt(base) + 2}px`) throw new Error(`zoom: ${base} → ${size()}`);
        key('0');
        $('btn-prefs').click();
        const theme = $<HTMLSelectElement>('pref-theme');
        theme.value = 'dark';
        theme.dispatchEvent(new Event('input'));
        if (document.documentElement.dataset.theme !== 'dark') throw new Error('theme not applied');
        theme.value = 'system';
        theme.dispatchEvent(new Event('input'));
        $('prefs-close').click();
      });

      await step('program arguments', async () => {
        $<HTMLInputElement>('args').value = `alpha "two words" 3`;
        setCode('fn main() {\n    let args: Vec<String> = std::env::args().skip(1).collect();\n    println!("ARGS={args:?}");\n}\n');
        const out = await runAndWait('args');
        if (!out.includes('ARGS=["alpha", "two words", "3"]')) throw new Error(out.slice(-200));
        $<HTMLInputElement>('args').value = '';
      });

      await step('outline lists functions, structs and impls', async () => {
        setCode('struct Point {\n    x: i32,\n}\n\nimpl Point {\n    fn new() -> Self {\n        Point { x: 0 }\n    }\n}\n\nenum Dir { Up }\n\nfn main() {\n    let _p = Point::new();\n}\n');
        await until('outline', () => ($('outline-list').textContent ?? '').includes('Point'), 3000);
        const rows = [...document.querySelectorAll('#outline-list li')].map((li) => [...li.children].map((c) => c.textContent).join(' '));
        const want = ['struct Point 1', 'impl Point 5', 'fn new 6', 'enum Dir 11', 'fn main 13'];
        if (JSON.stringify(rows) !== JSON.stringify(want)) throw new Error(JSON.stringify(rows));
      });

      const dir = await invoke<string>('selftest_dir');
      await step('recent files', async () => {
        const path = await invoke<string>('join_path', { dir, name: 'recent-test.rs' });
        await invoke('write_text_file', { path, contents: 'fn main() {}\n' });
        const tabsBefore = document.querySelectorAll('#etab-list .etab').length;
        gx.explorer.setLocalDir(dir);
        await until('listed', () => !!document.querySelector(`#local-list li[data-path="${CSS.escape(path)}"]`), 5000);
        document.querySelector<HTMLElement>(`#local-list li[data-path="${CSS.escape(path)}"]`)!.dispatchEvent(new MouseEvent('click', { bubbles: true, detail: 2 }));
        await until('opened', () => ($('status-file').textContent ?? '').startsWith('recent-test.rs'), 5000);
        const options = [...$<HTMLSelectElement>('recent').options].map((o) => o.textContent ?? '');
        if (!options.some((o) => o.startsWith('recent-test.rs'))) throw new Error(options.join(' | '));
        void tabsBefore;
      });

      await step('explorer: new folder, then Move to Trash', async () => {
        const name = `gauthier-selftest-${Date.now()}`;
        $('btn-local-mkdir').click();
        await until('name dialog', () => (document.getElementById('name-dialog') as HTMLDialogElement).open, 3000);
        $<HTMLInputElement>('name-input').value = name;
        $('name-form').dispatchEvent(new Event('submit', { cancelable: true }));
        const path = await invoke<string>('join_path', { dir, name });
        await until('folder listed', () => !!document.querySelector(`#local-list li[data-path="${CSS.escape(path)}"]`), 5000);
        const li = document.querySelector<HTMLElement>(`#local-list li[data-path="${CSS.escape(path)}"]`)!;
        li.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, clientX: 40, clientY: 40 }));
        [...document.querySelectorAll<HTMLElement>('#context-menu li')].find((x) => x.textContent === 'Move to Trash')!.click();
        await until('gone from the list', () => !document.querySelector(`#local-list li[data-path="${CSS.escape(path)}"]`), 8000);
        if (await invoke<boolean>('path_exists', { path })) throw new Error('still on disk');
      });

      await step('errors explained in plain words', async () => {
        setCode('fn main() {\n    let n = 5;\n    n = 6;\n    println!("{n}");\n}\n');
        await until('problem', () => ($('problem-list').textContent ?? '').includes('let mut'), 60000);
        const text = $('problem-list').textContent ?? '';
        log.push(`     ${text.replace(/\s+/g, ' ').slice(0, 120)}…`);
      });

      await step('crates: search crates.io, add itoa, use it', async () => {
        setCode('fn main() {\n    let mut buf = itoa::Buffer::new();\n    println!("ITOA={}", buf.format(12345));\n}\n');
        $('btn-crates').click();
        $<HTMLInputElement>('crates-query').value = 'itoa';
        $('crates-form').dispatchEvent(new Event('submit', { cancelable: true }));
        await until('results', () => [...document.querySelectorAll('#crates-results li b')].some((b) => b.textContent === 'itoa'), 20000);
        const row = [...document.querySelectorAll<HTMLElement>('#crates-results li')].find((li) => li.querySelector('b')?.textContent === 'itoa')!;
        row.querySelector<HTMLButtonElement>('button')!.click();
        await until('in file', () => /\/\/! itoa = "\d+\.\d+\.\d+"/.test(getCode()), 3000);
        $('crates-close').click();
        const out = await runAndWait('crates');
        if (!out.includes('ITOA=12345')) throw new Error(out.slice(-300));
        log.push(`     ${getCode().split('\n')[2]}`);
      });

      await step('debugger: breakpoint, variables, step, expand, continue', async () => {
        const code = 'fn main() {\n    let names = vec!["Ada", "Linus"];\n    let mut total = 0;\n    for i in 1..=3 {\n        total += i;\n    }\n    println!("total={total} {names:?}");\n}\n';
        setCode(code);
        toggleBreakpointAt(g.view, g.view.state.doc.line(5).from); // total += i;
        $('btn-debug').click();
        await until('stopped', () => ($('dbg-reason').textContent ?? '').startsWith('Stopped at a breakpoint'), 120000);
        const line = () => g.view.state.doc.lineAt(g.view.state.selection.main.head).number;
        if (line() !== 5) throw new Error(`stopped at line ${line()}`);
        if (!document.querySelector('.cm-debug-line')) throw new Error('no highlighted line');
        const vars = () => [...document.querySelectorAll('#dbg-vars > li > .var-row')].map((r) => `${r.querySelector('.vn')!.textContent}=${r.querySelector('.vv')!.textContent}`);
        if (!vars().some((v) => v.startsWith('total=0'))) throw new Error('vars: ' + vars().join(', '));
        log.push(`     at line 5: ${vars().join(', ').slice(0, 110)}`);
        // Step over twice: back to the loop head, then into the body again.
        $('dbg-next').click();
        await until('stepped', () => !($<HTMLButtonElement>('dbg-next').disabled) && line() !== 5, 15000);
        // Expand the Vec.
        const names = [...document.querySelectorAll<HTMLElement>('#dbg-vars > li')].find((li) => li.querySelector('.vn')?.textContent === 'names')!;
        names.querySelector<HTMLElement>('.var-row')!.click();
        await until('children', () => (names.querySelector('ul')?.textContent ?? '').includes('Ada'), 8000);
        // Continue: hits the breakpoint again with total = 1.
        $('dbg-continue').click();
        await until('second stop', () => vars().some((v) => v.startsWith('total=1')), 15000);
        // Remove the breakpoint and continue to the end.
        toggleBreakpointAt(g.view, g.view.state.doc.line(5).from);
        await sleep(300);
        $('dbg-continue').click();
        await until('finished', () => consoleText().includes('total=6'), 15000);
        await until('ended', () => $('dbg-controls').hidden, 10000);
      });

      await step('Stop kills a running program', async () => {
        setCode('fn main() {\n    let mut n = 0u64;\n    loop {\n        n += 1;\n        if n % 50_000_000 == 0 { println!("{n}"); }\n    }\n}\n');
        $('btn-run').click();
        await until('running', () => state().includes('running'), 120000);
        await sleep(500);
        $('btn-stop').click();
        await until('stopped', () => state().includes('idle'), 5000);
      });
    } else if (mode === 'pico2-files' || mode === 'pico-files') {
      await filesScenario(mode.replace('-files', ''), setCode, getCode, step, log);
    } else {
      setTarget(mode);
      await sleep(300);
      await step(`Pico (${mode}): build, flash without BOOTSEL, console both ways`, async () => {
        await pickExample('Talk to the Pico');
        $('btn-run').click();
        await until('pico output', () => consoleText().includes('Type something'), 300000);
        await type('hello pico');
        await until('pico echo', () => consoleText().includes('The Pico heard: hello pico'), 10000);
      });
      await step(`Pico (${mode}): second Run re-flashes the board`, async () => {
        await pickExample('Blink the LED');
        const before = consoleText().split('Program sent to the Pico').length;
        $('btn-run').click();
        await until('flashed', () => consoleText().split('Program sent to the Pico').length > before, 120000);
        await until('blink output', () => consoleText().split('Program sent to the Pico').pop()!.includes('Blink!'), 15000);
        $('btn-stop').click();
      });
    }
  } catch {
    /* reported below */
  }
  const pos = await win.outerPosition().catch(() => null);
  const size = await win.outerSize().catch(() => null);
  const scale = await win.scaleFactor().catch(() => 1);
  if (pos && size) {
    // Lets the test script take a screenshot of the final state.
    const bounds = `${Math.round(pos.x / scale)},${Math.round(pos.y / scale)},${Math.round(size.width / scale)},${Math.round(size.height / scale)}`;
    await invoke('selftest_window', { bounds }).catch(() => undefined);
    await sleep(4000);
  }
  await invoke('selftest_report', { report: log.join('\n') + '\n' });
}

// ---------------------------------------------------------------- files on the Pico (explorer)

const TALK = `#![no_std]
#![no_main]

use embassy_executor::Spawner;
use picorust_runtime::println;

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    picorust_runtime::init(spawner, p.USB).await;

    println!("Type something (MARKER):");
    let mut buf = [0u8; 128];
    loop {
        let n = picorust_runtime::read_line(&mut buf).await;
        let text = core::str::from_utf8(&buf[..n]).unwrap_or("?");
        println!("The Pico heard: {}", text);
    }
}
`;

function program(marker: string) {
  return `// Gauthier self-test ${marker}\n// Accents et symboles : é è à ç ü ß — “quotes” «guillemets» 😀 🦀 中文\n` + TALK.replace('MARKER', marker);
}

const after = (marker: string) => consoleText().split(marker).pop() ?? '';
const picoItem = (name: string) => document.querySelector<HTMLElement>(`#pico-list li[data-name="${CSS.escape(name)}"]`);
const localItem = (path: string) => document.querySelector<HTMLElement>(`#local-list li[data-path="${CSS.escape(path)}"]`);

async function contextAction(li: HTMLElement, label: string) {
  li.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, clientX: 50, clientY: 50 }));
  const item = [...document.querySelectorAll<HTMLElement>('#context-menu li')].find((x) => x.textContent === label);
  if (!item) throw new Error(`menu item not found: ${label}`);
  item.click();
}

async function answerName(name: string) {
  await until('name dialog', () => (document.getElementById('name-dialog') as HTMLDialogElement).open, 3000);
  ($('name-input') as HTMLInputElement).value = name;
  $('name-form').dispatchEvent(new Event('submit', { cancelable: true }));
}

async function filesScenario(
  board: string,
  setCode: (s: string) => void,
  getCode: () => string,
  step: (name: string, fn: () => Promise<void>) => Promise<void>,
  log: string[],
) {
  (window as unknown as { __gauthierAutoConfirm: boolean }).__gauthierAutoConfirm = true;
  const g = (window as unknown as { __gauthier: { explorer: { setLocalDir(d: string): void; refreshPico(): Promise<void> } } }).__gauthier;
  setTarget(board);
  await sleep(300);
  const stamp = String(Date.now());
  const code = program(`A-${stamp}`);
  const title = () => $('status-file').textContent ?? '';
  const refresh = () => g.explorer.refreshPico();

  await step('Run saves main.rs on the Pico (no BOOTSEL to read it later)', async () => {
    setCode(code);
    $('btn-run').click();
    await until('saved', () => after('── Run').includes('Saved as main.rs on the Pico'), 300000);
    await until('output', () => after('── Run').includes(`Type something (A-${stamp})`), 15000);
    await refresh();
    await until('main.rs listed', () => !!picoItem('main.rs'), 8000);
  });

  await step('double-click main.rs: same code, program keeps running', async () => {
    setCode('// something else\n');
    const consoleBefore = consoleText().length;
    picoItem('main.rs')!.dispatchEvent(new MouseEvent('click', { bubbles: true, detail: 2 }));
    await until('opened', () => title().startsWith('main.rs (Pico)'), 8000);
    if (getCode() !== code) throw new Error(`different code (${getCode().length} vs ${code.length} chars)`);
    const added = consoleText().slice(consoleBefore);
    if (/BOOTSEL|Program sent|Connected to/.test(added)) throw new Error('the board was rebooted: ' + added);
    if (!state().includes('running')) throw new Error('console session lost');
    await type('still-running');
    await until('echo', () => consoleText().includes('The Pico heard: still-running'), 8000);
    log.push(`     ${code.length} characters, identical; console never disconnected`);
  });

  await step('edit + Save writes the file on the Pico', async () => {
    const edited = code + '// edited in Gauthier ✏️\n';
    setCode(edited);
    $('btn-save').click();
    await until('saved', () => !title().includes('•'), 8000);
    const back = await invoke<string>('pico_fs_read_text', { name: 'main.rs' });
    if (back !== edited) throw new Error('not saved on the Pico');
  });

  await step('tabs: several files open, each keeps its own text', async () => {
    const tabNames = () => [...document.querySelectorAll<HTMLElement>('#etab-list .etab')].map((t) => t.dataset.name);
    const tabEl = (name: string) => document.querySelector<HTMLElement>(`#etab-list .etab[data-name="${CSS.escape(name)}"]`)!;
    await invoke('pico_fs_write_text', { name: `second-${stamp}.txt`, contents: 'second file\n' });
    await refresh();
    await until('listed', () => !!picoItem(`second-${stamp}.txt`), 8000);
    picoItem(`second-${stamp}.txt`)!.dispatchEvent(new MouseEvent('click', { bubbles: true, detail: 2 }));
    await until('second tab', () => title().startsWith(`second-${stamp}.txt (Pico)`), 8000);
    if (!tabNames().includes('main.rs')) throw new Error('main.rs tab closed: ' + tabNames().join(', '));
    setCode('second file, modified\n');
    tabEl('main.rs').click();
    await until('main.rs active', () => title().startsWith('main.rs (Pico)'), 3000);
    if (!getCode().includes(`A-${stamp}`)) throw new Error('main.rs lost its text');
    tabEl(`second-${stamp}.txt`).click();
    await until('second active', () => getCode() === 'second file, modified\n', 3000);
    if (!tabEl(`second-${stamp}.txt`).classList.contains('dirty')) throw new Error('no unsaved marker');
    // Re-opening an open file switches to its tab instead of duplicating it.
    picoItem('main.rs')!.dispatchEvent(new MouseEvent('click', { bubbles: true, detail: 2 }));
    await until('switched', () => title().startsWith('main.rs (Pico)'), 3000);
    if (tabNames().filter((n) => n === 'main.rs').length !== 1) throw new Error('duplicate tab');
    // Closing a modified tab asks; "Don't save" closes it and keeps the Pico file unchanged.
    (tabEl(`second-${stamp}.txt`).querySelector('.etab-close') as HTMLElement).click();
    await until('save prompt', () => (document.getElementById('choice-dialog') as HTMLDialogElement).open, 3000);
    document.querySelector<HTMLElement>('#choice-buttons [data-choice="discard"]')!.click();
    await until('closed', () => !tabNames().includes(`second-${stamp}.txt`), 3000);
    if ((await invoke<string>('pico_fs_read_text', { name: `second-${stamp}.txt` })) !== 'second file\n') throw new Error('file changed');
    await invoke('pico_fs_delete', { name: `second-${stamp}.txt` });
    log.push(`     tabs now: ${tabNames().join(', ')}`);
  });

  await step('new file on the Pico from the explorer', async () => {
    $('btn-pico-new').click();
    await answerName(`notes-${stamp}.txt`);
    await until('listed', () => !!picoItem(`notes-${stamp}.txt`), 8000);
    await until('opened', () => title().startsWith(`notes-${stamp}.txt (Pico)`), 8000);
    setCode('Notes écrites sur le Pico 📝\nligne 2\n');
    $('btn-save').click();
    await until('saved', () => !title().includes('•'), 8000);
  });

  // A scratch folder of the test run plays "This computer".
  const out = await invoke<string>('selftest_dir');
  g.explorer.setLocalDir(out);
  const localPath = await invoke<string>('join_path', { dir: out, name: `from-computer-${stamp}.txt` });
  const localText = 'Fichier venu de l’ordinateur ✅\n'.repeat(20);

  await step('copy computer → Pico (context menu)', async () => {
    await invoke('write_text_file', { path: localPath, contents: localText });
    g.explorer.setLocalDir(out);
    await until('local listed', () => !!localItem(localPath), 5000);
    await contextAction(localItem(localPath)!, 'Copy to the Pico');
    await until('on the Pico', () => !!picoItem(`from-computer-${stamp}.txt`), 10000);
    const back = await invoke<string>('pico_fs_read_text', { name: `from-computer-${stamp}.txt` });
    if (back !== localText) throw new Error('content differs');
  });

  await step('copy Pico → computer (context menu)', async () => {
    const dest = await invoke<string>('join_path', { dir: out, name: `notes-${stamp}.txt` });
    await contextAction(picoItem(`notes-${stamp}.txt`)!, 'Copy to this computer');
    await until('on the computer', () => !!localItem(dest), 10000);
    const got = await invoke<string>('read_text_file', { path: dest });
    if (got !== 'Notes écrites sur le Pico 📝\nligne 2\n') throw new Error('content differs: ' + got);
  });

  await step('rename then delete on the Pico', async () => {
    await contextAction(picoItem(`from-computer-${stamp}.txt`)!, 'Rename…');
    await answerName(`renamed-${stamp}.txt`);
    await until('renamed', () => !!picoItem(`renamed-${stamp}.txt`) && !picoItem(`from-computer-${stamp}.txt`), 8000);
    await contextAction(picoItem(`renamed-${stamp}.txt`)!, 'Delete');
    await until('deleted', () => !picoItem(`renamed-${stamp}.txt`), 8000);
    await contextAction(picoItem(`notes-${stamp}.txt`)!, 'Delete');
    await until('deleted', () => !picoItem(`notes-${stamp}.txt`), 8000);
  });

  await step('100 KB text file round trip', async () => {
    let big = '';
    let i = 0;
    while (big.length < 100_000) big += `line ${i++}: àéîõü 😀 the quick brown fox\n`;
    const t0 = Date.now();
    await invoke('pico_fs_write_text', { name: `big-${stamp}.txt`, contents: big });
    const t1 = Date.now();
    const back = await invoke<string>('pico_fs_read_text', { name: `big-${stamp}.txt` });
    const t2 = Date.now();
    if (back !== big) throw new Error('content differs');
    log.push(`     write ${t1 - t0} ms, read ${t2 - t1} ms (${new TextEncoder().encode(big).length} bytes)`);
    await invoke('pico_fs_delete', { name: `big-${stamp}.txt` });
  });

  await step("the program's own fs API (write, read, counter across reboots)", async () => {
    const prog = `#![no_std]
#![no_main]

use embassy_executor::Spawner;
use picorust_runtime::{fs, println};

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    picorust_runtime::init(spawner, p.USB).await;

    // Boot counter kept in a file.
    let mut buf = [0u8; 16];
    let boots = match fs::read("boots.txt", &mut buf).await {
        Ok(n) => core::str::from_utf8(&buf[..n]).ok().and_then(|s| s.trim().parse::<u32>().ok()).unwrap_or(0),
        Err(_) => 0,
    } + 1;
    let mut out = [0u8; 16];
    let len = fmt_u32(boots, &mut out);
    fs::write("boots.txt", &out[..len]).await.unwrap();

    fs::write("from-program.txt", "écrit par le programme 🦀".as_bytes()).await.unwrap();
    let mut back = [0u8; 64];
    let n = fs::read("from-program.txt", &mut back).await.unwrap();
    println!("FSTEST boots={} read={}", boots, core::str::from_utf8(&back[..n]).unwrap());
    loop {
        embassy_time::Timer::after_secs(1).await;
    }
}

fn fmt_u32(mut v: u32, out: &mut [u8; 16]) -> usize {
    let mut tmp = [0u8; 10];
    let mut i = 0;
    loop {
        tmp[i] = b'0' + (v % 10) as u8;
        v /= 10;
        i += 1;
        if v == 0 { break; }
    }
    for k in 0..i { out[k] = tmp[i - 1 - k]; }
    i
}
`;
    await invoke('pico_fs_delete', { name: 'boots.txt' }).catch(() => undefined);
    setCode(prog);
    $('btn-run').click();
    await until('first boot', () => /FSTEST boots=1 read=écrit par le programme 🦀/.test(after('── Run')), 300000);
    // Second run of the same program: the counter must come from the file.
    $('btn-run').click();
    await sleep(500);
    await until('second boot', () => /FSTEST boots=2 /.test(after('── Run')), 120000);
    await refresh();
    await until('files listed', () => !!picoItem('from-program.txt') && !!picoItem('boots.txt'), 8000);
    const txt = await invoke<string>('pico_fs_read_text', { name: 'from-program.txt' });
    if (txt !== 'écrit par le programme 🦀') throw new Error('content differs: ' + txt);
    await invoke('pico_fs_delete', { name: 'boots.txt' });
    await invoke('pico_fs_delete', { name: 'from-program.txt' });
    // Leave the talking program on the board for the last step.
    setCode(code);
    $('btn-run').click();
    await sleep(500);
    await until('talk program back', () => after('── Run').includes(`Type something (A-${stamp})`), 120000);
  });

  await step('console still answers after all file operations', async () => {
    await type('end');
    await until('echo', () => consoleText().includes('The Pico heard: end'), 8000);
    // The last Run stored `code` as main.rs.
    await invoke('selftest_save', { name: 'expected.rs', contents: code });
  });
}
