// File explorer: "This computer" (a folder) and "Raspberry Pi Pico" (files in the board's flash,
// reached over USB while the program runs: no BOOTSEL).

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { ask, open as openDialog } from '@tauri-apps/plugin-dialog';

export type DocRef = { kind: 'untitled'; name: string } | { kind: 'local'; path: string } | { kind: 'pico'; name: string };

export interface ExplorerHost {
  print(kind: string, text: string): void;
  openLocal(path: string): Promise<void>;
  openPico(name: string): Promise<void>;
  askName(title: string, initial: string, validate?: (v: string) => string | null): Promise<string | null>;
  doc(): DocRef;
  /** A file shown in a tab was renamed (`to`) or deleted (null). */
  docMoved(from: DocRef, to: DocRef | null): void;
  toast(message: string, error?: boolean): void;
}

/** Real confirmation dialog (window.confirm does not block in the app). */
function confirmAction(message: string, okLabel: string) {
  // Self-test only: answer automatically.
  if ((window as unknown as { __gauthierAutoConfirm?: boolean }).__gauthierAutoConfirm) return Promise.resolve(true);
  return ask(message, { title: 'Gauthier', kind: 'warning', okLabel, cancelLabel: 'Cancel' });
}

/** Double-click (and Enter) to open, robust to draggable items. */
function onOpen(li: HTMLElement, open: () => void) {
  li.tabIndex = 0;
  li.addEventListener('click', (e) => {
    document.querySelectorAll('.file-list li.selected').forEach((x) => x.classList.remove('selected'));
    li.classList.add('selected');
    if (e.detail === 2) open();
  });
  li.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') open();
  });
}

interface LocalEntry {
  name: string;
  path: string;
  is_dir: boolean;
  size: number;
}
interface PicoFile {
  name: string;
  size: number;
}
interface PicoFsInfo {
  capacity: number;
  free: number;
  files: PicoFile[];
}

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

function human(n: number) {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(n < 10 * 1024 ? 1 : 0)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

const ICON_FILE = '<svg viewBox="0 0 24 24"><path d="M6 3h8l5 5v13H6z M14 3v5h5"/></svg>';
const ICON_RUST = '<svg viewBox="0 0 24 24" class="rs"><path d="M6 3h8l5 5v13H6z M14 3v5h5"/><path d="M9 13h6M9 17h4"/></svg>';
const ICON_DIR = '<svg viewBox="0 0 24 24" class="dir"><path d="M3 6h7l2 2h9v11H3z"/></svg>';

export class Explorer {
  localDir = '';
  picoConnected = false;
  private picoFiles: PicoFile[] = [];
  private busy = false;
  private pollTimer: number | undefined;
  private menu = $<HTMLUListElement>('context-menu');

  constructor(private host: ExplorerHost) {
    $('btn-local-refresh').onclick = () => this.refreshLocal();
    $('btn-local-mkdir').onclick = () => this.newLocalFolder();
    $('btn-local-up').onclick = async () => {
      const parent = await invoke<string | null>('parent_dir', { path: this.localDir });
      if (parent) this.setLocalDir(parent);
    };
    $('btn-local-folder').onclick = async () => {
      const picked = await openDialog({ directory: true, defaultPath: this.localDir });
      if (picked && !Array.isArray(picked)) this.setLocalDir(picked);
    };
    $('btn-pico-refresh').onclick = () => this.refreshPico(true);
    $('btn-pico-new').onclick = () => this.newPicoFile();
    document.addEventListener('click', () => this.hideMenu());
    window.addEventListener('blur', () => this.hideMenu());
    this.setupDrop('local-list', 'local');
    this.setupDrop('pico-list', 'pico');
    void listen('pico-files-changed', () => this.refreshPico());
    this.setupSplitter();
  }

  async start() {
    let dir = '';
    try {
      dir = (window as unknown as { __gauthierAutoConfirm?: boolean }).__gauthierAutoConfirm ? '' : localStorage.getItem('gauthier.localDir') ?? '';
    } catch {
      /* ignore */
    }
    if (!dir || !(await invoke<boolean>('path_exists', { path: dir }))) dir = await invoke<string>('default_folder');
    this.setLocalDir(dir);
    this.poll();
  }

  setBusy(busy: boolean) {
    this.busy = busy;
    document.getElementById('pico-section')!.classList.toggle('busy', busy);
  }

  highlight(doc: DocRef) {
    document.querySelectorAll('.file-list li').forEach((li) => {
      const el = li as HTMLElement;
      const on =
        (doc.kind === 'local' && el.dataset.side === 'local' && el.dataset.path === doc.path) ||
        (doc.kind === 'pico' && el.dataset.side === 'pico' && el.dataset.name === doc.name);
      el.classList.toggle('current', on);
    });
  }

  // ------------------------------------------------------------ computer

  setLocalDir(dir: string) {
    this.localDir = dir;
    try {
      if (!(window as unknown as { __gauthierNoPersist?: boolean }).__gauthierNoPersist) localStorage.setItem('gauthier.localDir', dir);
    } catch {
      /* ignore */
    }
    const crumb = $('local-path');
    crumb.textContent = dir.replace(/^\/Users\/[^/]+/, '~');
    crumb.title = dir;
    void this.refreshLocal();
  }

  async refreshLocal() {
    const list = $('local-list');
    let entries: LocalEntry[] = [];
    try {
      entries = await invoke<LocalEntry[]>('list_dir', { path: this.localDir });
    } catch (e) {
      list.innerHTML = `<li class="empty">${e}</li>`;
      return;
    }
    list.innerHTML = '';
    if (!entries.length) list.innerHTML = '<li class="empty">Empty folder</li>';
    for (const e of entries) {
      const li = this.item(e.is_dir ? ICON_DIR : e.name.endsWith('.rs') ? ICON_RUST : ICON_FILE, e.name, e.is_dir ? '' : human(e.size));
      li.dataset.side = 'local';
      li.dataset.path = e.path;
      if (e.is_dir) {
        onOpen(li, () => this.setLocalDir(e.path));
        li.oncontextmenu = (ev) =>
          this.showMenu(ev, [
            ['Open', () => this.setLocalDir(e.path)],
            ['Rename…', () => this.renameLocal(e)],
            ['Move to Trash', () => this.trashLocal(e)],
          ]);
      } else {
        li.draggable = true;
        li.ondragstart = (ev) => ev.dataTransfer?.setData('application/x-gauthier', JSON.stringify({ side: 'local', path: e.path, name: e.name }));
        onOpen(li, () => this.host.openLocal(e.path));
        li.oncontextmenu = (ev) =>
          this.showMenu(ev, [
            ['Open', () => this.host.openLocal(e.path)],
            ['Copy to the Pico', () => this.upload(e.path, e.name), !this.picoConnected],
            ['Rename…', () => this.renameLocal(e)],
            ['Move to Trash', () => this.trashLocal(e)],
          ]);
      }
      list.appendChild(li);
    }
    this.highlight(this.host.doc());
  }

  private async newLocalFolder() {
    const name = await this.host.askName('New folder', 'new folder');
    if (!name) return;
    try {
      await invoke('create_dir', { path: await invoke<string>('join_path', { dir: this.localDir, name }) });
    } catch (err) {
      this.host.toast(String(err), true);
    }
    void this.refreshLocal();
  }

  /** The Trash, not a permanent delete: it can be undone from the Finder. */
  private async trashLocal(e: LocalEntry) {
    if (!(await confirmAction(`Move ${e.name} to the Trash?`, 'Move to Trash'))) return;
    try {
      await invoke('trash_path', { path: e.path });
      this.host.print('info', `Moved ${e.name} to the Trash.\n`);
      if (!e.is_dir) this.host.docMoved({ kind: 'local', path: e.path }, null);
    } catch (err) {
      this.host.toast(String(err), true);
    }
    void this.refreshLocal();
  }

  private async renameLocal(e: LocalEntry) {
    const to = await this.host.askName('Rename', e.name);
    if (!to || to === e.name) return;
    const dest = await invoke<string>('join_path', { dir: this.localDir, name: to });
    if (await invoke<boolean>('path_exists', { path: dest })) {
      this.host.print('error', `${to} already exists.\n`);
      return;
    }
    try {
      await invoke('rename_path', { from: e.path, to: dest });
      this.host.docMoved({ kind: 'local', path: e.path }, { kind: 'local', path: dest });
    } catch (err) {
      this.host.print('error', `${err}\n`);
    }
    void this.refreshLocal();
  }

  // ------------------------------------------------------------ Pico

  private poll() {
    window.clearTimeout(this.pollTimer);
    const next = this.picoConnected ? 5000 : 2000;
    this.pollTimer = window.setTimeout(async () => {
      if (!this.busy) await this.refreshPico();
      this.poll();
    }, next);
  }

  async refreshPico(showErrors = false) {
    if (this.busy) return;
    const status = $('pico-status');
    try {
      const info = await invoke<PicoFsInfo>('pico_fs_info');
      const was = this.picoConnected;
      this.picoConnected = true;
      const used = info.capacity - info.free;
      status.textContent = `${info.files.length} file${info.files.length === 1 ? '' : 's'} · ${human(used)} of ${human(info.capacity)} used`;
      status.classList.remove('off');
      $('pico-usage-bar').style.width = `${Math.min(100, (used / Math.max(1, info.capacity)) * 100)}%`;
      if (!was || JSON.stringify(info.files) !== JSON.stringify(this.picoFiles)) {
        this.picoFiles = info.files;
        this.renderPico();
      }
    } catch (e) {
      this.picoConnected = false;
      this.picoFiles = [];
      const msg = String(e);
      status.textContent = msg.includes('No Pico') ? 'No Pico connected' : msg;
      status.classList.add('off');
      $('pico-usage-bar').style.width = '0';
      $('pico-list').innerHTML = '';
      if (showErrors) this.host.print('error', `${msg}\n`);
    }
  }

  private renderPico() {
    const list = $('pico-list');
    list.innerHTML = '';
    if (!this.picoFiles.length) list.innerHTML = '<li class="empty">No files yet. Run a program, or drop files here.</li>';
    for (const f of this.picoFiles) {
      const li = this.item(f.name.endsWith('.rs') ? ICON_RUST : ICON_FILE, f.name, human(f.size));
      li.dataset.side = 'pico';
      li.dataset.name = f.name;
      li.draggable = true;
      li.ondragstart = (ev) => ev.dataTransfer?.setData('application/x-gauthier', JSON.stringify({ side: 'pico', name: f.name }));
      onOpen(li, () => this.host.openPico(f.name));
      li.oncontextmenu = (ev) =>
        this.showMenu(ev, [
          ['Open', () => this.host.openPico(f.name)],
          ['Copy to this computer', () => this.download(f.name)],
          ['Rename…', () => this.renamePico(f.name)],
          ['Delete', () => this.deletePico(f.name)],
        ]);
      list.appendChild(li);
    }
    this.highlight(this.host.doc());
  }

  private validPicoName = (v: string) =>
    new TextEncoder().encode(v).length > 48 ? 'Names are limited to 48 bytes on the Pico.' : /[\t\n]/.test(v) ? 'Invalid name.' : null;

  private async newPicoFile() {
    if (!this.picoConnected) return;
    const name = await this.host.askName('New file on the Pico', 'notes.txt', this.validPicoName);
    if (!name) return;
    if (this.picoFiles.some((f) => f.name === name)) {
      this.host.print('error', `${name} already exists on the Pico.\n`);
      return;
    }
    await this.run(`Created ${name} on the Pico.`, () => invoke('pico_fs_write_text', { name, contents: '' }));
    void this.host.openPico(name);
  }

  private async renamePico(name: string) {
    const to = await this.host.askName('Rename on the Pico', name, this.validPicoName);
    if (!to || to === name) return;
    const ok = await this.run(`Renamed ${name} → ${to}.`, () => invoke('pico_fs_rename', { from: name, to }));
    if (ok) this.host.docMoved({ kind: 'pico', name }, { kind: 'pico', name: to });
  }

  private async deletePico(name: string) {
    if (!(await confirmAction(`Delete ${name} from the Pico?`, 'Delete'))) return;
    const ok = await this.run(`Deleted ${name} from the Pico.`, () => invoke('pico_fs_delete', { name }));
    if (ok) this.host.docMoved({ kind: 'pico', name }, null);
  }

  async upload(path: string, name: string) {
    if (new TextEncoder().encode(name).length > 48) {
      name = (await this.host.askName('Name on the Pico (48 bytes max)', name.slice(0, 40), this.validPicoName)) ?? '';
      if (!name) return;
    }
    if (this.picoFiles.some((f) => f.name === name) && !(await confirmAction(`Replace ${name} on the Pico?`, 'Replace'))) return;
    await this.run(`Copied ${name} to the Pico.`, () => invoke('pico_fs_upload', { path, name }));
  }

  async download(name: string) {
    const dest = await invoke<string>('join_path', { dir: this.localDir, name });
    if ((await invoke<boolean>('path_exists', { path: dest })) && !(await confirmAction(`Replace ${name} in ${this.localDir}?`, 'Replace'))) return;
    const ok = await this.run(`Copied ${name} to ${this.localDir}.`, () => invoke('pico_fs_download', { name, path: dest }));
    if (ok) void this.refreshLocal();
  }

  private async run(done: string, op: () => Promise<unknown>): Promise<boolean> {
    try {
      await op();
      this.host.print('info', done + '\n');
      return true;
    } catch (e) {
      this.host.print('error', `${e}\n`);
      this.host.toast(String(e), true);
      return false;
    } finally {
      await this.refreshPico();
    }
  }

  // ------------------------------------------------------------ UI helpers

  private item(icon: string, name: string, detail: string) {
    const li = document.createElement('li');
    li.innerHTML = `${icon}<span class="name"></span><span class="size"></span>`;
    (li.querySelector('.name') as HTMLElement).textContent = name;
    (li.querySelector('.size') as HTMLElement).textContent = detail;
    li.title = name;
    return li;
  }

  private showMenu(ev: MouseEvent, items: [string, () => void, boolean?][]) {
    ev.preventDefault();
    ev.stopPropagation();
    this.menu.innerHTML = '';
    for (const [label, action, disabled] of items) {
      const li = document.createElement('li');
      li.textContent = label;
      if (disabled) li.classList.add('disabled');
      else
        li.onclick = () => {
          this.hideMenu();
          action();
        };
      this.menu.appendChild(li);
    }
    this.menu.hidden = false;
    const { innerWidth: w, innerHeight: h } = window;
    const r = this.menu.getBoundingClientRect();
    this.menu.style.left = `${Math.min(ev.clientX, w - r.width - 4)}px`;
    this.menu.style.top = `${Math.min(ev.clientY, h - r.height - 4)}px`;
  }

  private hideMenu() {
    this.menu.hidden = true;
  }

  private setupDrop(listId: string, side: 'local' | 'pico') {
    const list = $(listId);
    list.ondragover = (ev) => {
      if (ev.dataTransfer?.types.includes('application/x-gauthier')) {
        ev.preventDefault();
        list.classList.add('drop');
      }
    };
    list.ondragleave = () => list.classList.remove('drop');
    list.ondrop = (ev) => {
      ev.preventDefault();
      list.classList.remove('drop');
      const raw = ev.dataTransfer?.getData('application/x-gauthier');
      if (!raw) return;
      const item = JSON.parse(raw) as { side: string; path?: string; name: string };
      if (item.side === side) return;
      if (side === 'pico' && item.path) void this.upload(item.path, item.name);
      if (side === 'local') void this.download(item.name);
    };
  }

  private setupSplitter() {
    const splitter = $('sidebar-splitter');
    const sidebar = $('sidebar');
    try {
      const w = Number(localStorage.getItem('gauthier.sidebar'));
      if (w > 120) sidebar.style.width = `${w}px`;
    } catch {
      /* ignore */
    }
    splitter.addEventListener('pointerdown', (e) => {
      splitter.setPointerCapture(e.pointerId);
      const startX = e.clientX;
      const startW = sidebar.offsetWidth;
      const move = (ev: PointerEvent) => {
        sidebar.style.width = `${Math.max(150, Math.min(520, startW + ev.clientX - startX))}px`;
      };
      splitter.addEventListener('pointermove', move);
      splitter.addEventListener(
        'pointerup',
        () => {
          splitter.removeEventListener('pointermove', move);
          try {
            localStorage.setItem('gauthier.sidebar', String(sidebar.offsetWidth));
          } catch {
            /* ignore */
          }
        },
        { once: true },
      );
    });
  }
}
