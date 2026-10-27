'use strict';

// Shared state, formatting, scanning events and the side panel.
// The UI is split into plain scripts loaded in order (see index.html);
// they share their top-level names.

// Tauri's JS API, exposed because tauri.conf.json sets withGlobalTauri.
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const ROW_H = 30; // must match .row height in style.css
const OVERSCAN = 10;

// Node flags, as in scanner::flags.
const FLAG = { DUP: 1, ONLINE: 4, COMPRESSED: 8, SPARSE: 16, READ_ERROR: 32, REMOVED: 64, EXCLUDED: 128 };

// Colour names used by the rules files. A rules file may also give #rgb hex.
const COLORS = {
  violet: '#7c5ce6', rose: '#e05a78', amber: '#e8a33c', pink: '#d96bb0',
  blue: '#4a86e8', orange: '#e9773a', slate: '#7b8794', purple: '#a35ad6',
  teal: '#38aaa0', cyan: '#3bb4d6', lime: '#8cc63f', indigo: '#5b6ad6',
  gray: '#9aa0a6', green: '#4caf6a', zinc: '#b0b0b8', red: '#e04848',
  yellow: '#e8c93c', sky: '#4fb3f0', brown: '#a0703c',
};

const $ = (id) => document.getElementById(id);

// Script errors go to the log file (see About), at most 50 per run.
let errorsLogged = 0;
function logError(what) {
  if (errorsLogged++ < 50) invoke('log_ui', { msg: String(what) }).catch(() => {});
}
window.addEventListener('error', (e) => logError(`${e.message} at ${e.filename}:${e.lineno}`));
window.addEventListener('unhandledrejection', (e) => logError(e.reason?.stack ?? e.reason));
const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' });

let lastPath = null;
let summary = null;
let cats = new Map();      // category id -> category
let cache = new Map();     // folder id -> its children, sorted
let expanded = new Set();  // folder ids
let visible = [];          // rows on screen in order: { row, depth, parentSize }
let selected = -1;         // index into `visible`: the cursor
let marked = new Set();    // ids of the selected rows (one or more)
let anchorId = -1;         // node id where a Shift range starts

// Search results grouped by folder are a second tree run by the same code:
// the variables above hold whichever tree is showing, `otherTree` the other.
let shownTree = 'tree';    // 'tree' or 'groups'
let otherTree = null;

function useTree(which) {
  if (which === shownTree || !otherTree) return;
  const mine = { cache, expanded, visible, selected, marked, anchorId };
  ({ cache, expanded, visible, selected, marked, anchorId } = otherTree);
  otherTree = mine;
  shownTree = which;
  // Sorting may have changed while this tree was away.
  for (const kids of cache.values()) sortRows(kids);
  rebuild();
  select(selected, 'keep'); // the status bar shows this tree's selection
}

/** A fresh tree with just `root`, its children not loaded yet. */
function newTree(root) {
  return {
    cache: new Map(), expanded: new Set(),
    visible: [{ row: root, depth: 0, parentSize: root.size }],
    selected: 0, marked: new Set([root.id]), anchorId: root.id,
  };
}

/** Whether the list shows search results as one list by size. */
const flat = () => view === 'results' && !grouped;

// Explorer-style icons in front of names: a yellow folder, an outlined page.
const FOLDER_ICO = '<svg class="ico" viewBox="0 0 16 16" aria-hidden="true"><path d="M1.5 4A1.5 1.5 0 0 1 3 2.5h3.2l1.5 1.5H13A1.5 1.5 0 0 1 14.5 5.5v6.5A1.5 1.5 0 0 1 13 13.5H3A1.5 1.5 0 0 1 1.5 12z" fill="#e0a83a"/></svg>';
const FILE_ICO = '<svg class="ico file" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.2" aria-hidden="true"><path d="M4 1.5h5l3 3v10H4zM9 1.5v3h3"/></svg>';
const iconOf = (r) => (r.kind === 'dir' ? FOLDER_ICO : FILE_ICO);
const SHIELD = '<svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true"><path d="M8 1 2.5 3.2v4.3c0 3.4 2.3 6 5.5 7.5 3.2-1.5 5.5-4.1 5.5-7.5V3.2z" fill="currentColor"/></svg>';

/** A shield before the name of anything that probably shouldn't be touched. */
function riskHtml(r) {
  return r.risk ? `<span class="risk ${r.risk.level}" title="${esc(r.risk.reason)}">${SHIELD}</span>` : '';
}
let sort = { key: 'size', desc: true };

// ---------- formatting ----------

function human(bytes) {
  const units = ['B', 'KB', 'MB', 'GB', 'TB', 'PB'];
  let v = bytes, u = 0;
  while (v >= 1024 && u < units.length - 1) { v /= 1024; u++; }
  return u === 0 ? `${bytes} B` : `${v.toFixed(1)} ${units[u]}`;
}

const fmtInt = (n) => n.toLocaleString();
const plural = (n, word) => `${fmtInt(n)} ${word}${n === 1 ? '' : 's'}`;

function fmtDate(secs) {
  if (!secs) return '';
  const d = new Date(secs * 1000);
  const p = (n) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

function esc(s) {
  return s.replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
}

function colorOf(cat) {
  if (!cat) return COLORS.gray;
  if (COLORS[cat.color]) return COLORS[cat.color];
  return /^#[0-9a-f]{3,8}$/i.test(cat.color) ? cat.color : COLORS.gray;
}

function setStatus(text, isError = false) {
  const s = $('status');
  s.textContent = text;
  s.classList.toggle('error', isError);
}

// ---------- scanning ----------

async function pick() {
  const path = await invoke('pick_folder');
  if (path) startScan(path);
}

/** What start_scan takes for "All drives" (see ALL_DRIVES in main.rs). */
const ALL_DRIVES = '::all-drives';
const pathLabel = (p) => (p === ALL_DRIVES ? 'All drives' : p);

async function startScan(path) {
  try {
    await invoke('start_scan', { path });
  } catch (e) {
    setStatus(String(e), true);
    return;
  }
  lastPath = path;
  store.set('lastPath', path);
  rememberRecent(path);
  $('elevate').hidden = true;
  summary = null;
  showScanning(path);
  setScanning(true);
  setStatus(`Scanning ${pathLabel(path)}…`);
}

function setScanning(on) {
  $('cancel').hidden = !on;
  $('pick').disabled = on;
  $('rescan').hidden = on || !lastPath;
}

listen('scan-progress', (e) => {
  const p = e.payload;
  setStatus(`Scanning… ${plural(p.files, 'file')} · ${plural(p.dirs, 'folder')} · ${human(p.bytes)} · ${(p.elapsedMs / 1000).toFixed(1)} s`);
  $('ss-files').textContent = fmtInt(p.files);
  $('ss-dirs').textContent = fmtInt(p.dirs);
  $('ss-bytes').textContent = human(p.bytes);
  $('ss-time').textContent = `${(p.elapsedMs / 1000).toFixed(1)} s`;
  $('scan-stage').hidden = !p.stage;
  $('scan-stage').textContent = p.stage ? `Now scanning ${p.stage}` : '';
});

listen('scan-done', async () => {
  setScanning(false);
  await loadResult();
});

listen('scan-error', (e) => {
  setScanning(false);
  const cancelled = e.payload === 'scan cancelled';
  setStatus(cancelled ? 'Scan cancelled.' : `Scan failed: ${e.payload}`, !cancelled);
  showStart();
});

async function loadResult() {
  useTree('tree');
  otherTree = null;
  summary = await invoke('summary');
  cats = new Map(summary.categories.map((c) => [c.id, c]));
  cache = new Map();
  expanded = new Set();
  const root = summary.root;
  visible = [{ row: root, depth: 0, parentSize: root.size }];
  selected = 0;
  marked = new Set();
  anchorId = root.id;
  search = null;
  histCompare = null;
  dupesKeep = new Map();
  dupesShown = 100;
  $('dupes').innerHTML = '';
  basket = EMPTY_BASKET;
  basketIds = new Set();
  fileTypes = null;
  typesShown = 200;
  renderBasket();
  resetFilters();
  $('f-cat').innerHTML = '<option value="">All categories</option>' + [...summary.categories]
    .filter((c) => c.files > 0)
    .map((c) => `<option value="${c.id}">${esc(c.name)}</option>`).join('');
  $('f-summary').textContent = '';
  $('f-clear').hidden = true;
  view = 'tree';

  renderSide();
  $('scanning').hidden = true;
  $('start').hidden = true;
  $('main').hidden = false;
  $('statusbar').hidden = false;
  $('scroller').scrollTop = 0;
  const how = summary.method === 'mft' ? "read from the drive's file table" : 'folder by folder';
  const took = summary.scanMs < 1000 ? `${summary.scanMs} ms` : `${(summary.scanMs / 1000).toFixed(1)} s`;
  setStatus(`Scanned in ${took} (${how}), categorised in ${summary.categoriseMs} ms`);
  $('elevate').hidden = !summary.adminWouldHelp;
  await expand(0);
  select(0);
  $('scroller').focus();
  map.details = new Map();
  zoomTo(root.id);
  setView('overview');
}

// ---------- side panel ----------

/** The navigation pane's drives: how full each is; a click scans it. */
async function renderNavDrives() {
  const drives = await invoke('drives').catch(() => []);
  const here = (lastPath ?? '').toLowerCase();
  $('nav-drives').innerHTML = drives.map((d) => {
    const used = d.total ? ((d.total - d.free) * 100) / d.total : 0;
    const on = d.path.toLowerCase() === here;
    return `<button data-path="${esc(d.path)}" class="${on ? 'on' : ''}" title="${on ? 'Scanned now' : `Scan ${esc(d.path)}`}">
        ${DRIVE_ICON}<span class="nd-name">${driveName(d)}</span>
        <span class="nd-bar"><span class="${used > 90 ? 'full' : ''}" style="width:${used}%"></span></span>
        <span class="nd-free">${human(d.free)} free of ${human(d.total)}</span>
      </button>`;
  }).join('');
}

function renderSide() {
  renderNavDrives();
  const s = summary;
  const note = $('rules-note');
  note.classList.toggle('error', !!s.rulesError);
  note.textContent = s.rulesError ? `Your rules file has a problem, so the built-in rules were used: ${s.rulesError}` : '';
  if (view === 'overview') renderOverview();
}

/** The scan's totals, at the top of Overview. */
function totalsHtml() {
  const s = summary;
  const r = s.root;
  const extra = [];
  if (s.hardlinkDups > 0) extra.push(['Hardlinks', `${fmtInt(s.hardlinkDups)} counted once`]);
  if (s.errors > 0) extra.push(['Unreadable', fmtInt(s.errors)]);
  const rows = [
    ['On disk', human(r.alloc)],
    ['Files', fmtInt(s.files)],
    ['Folders', fmtInt(s.dirs)],
    ...extra,
  ];
  let drive = '';
  if (s.driveTotal) {
    const used = s.driveTotal - s.driveFree;
    const pct = (n) => Math.min(100, (n * 100) / s.driveTotal);
    drive = `<div class="drive-use" title="The drive: ${human(s.driveFree)} free. The darker part is what you scanned.">
      <span class="du-pct">${pct(used).toFixed(0)}%</span> of the ${r.name === 'All drives' ? 'drives' : 'drive'} used · <b>${human(used)}</b> of ${human(s.driveTotal)}
      <div class="du-bar"><span class="du-used" style="width:${pct(used)}%"></span><span class="du-scan" style="width:${pct(Math.min(r.alloc, used))}%"></span></div>
    </div>`;
  }
  return `
    <div class="root-name">${esc(r.name)}</div>
    <div class="big-size">${human(r.size)}</div>
    ${drive}
    <dl>${rows.map(([k, v]) => `<dt>${k}</dt><dd>${v}</dd>`).join('')}</dl>`;
}
