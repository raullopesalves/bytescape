'use strict';

// Tauri's JS API, exposed because tauri.conf.json sets withGlobalTauri.
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const ROW_H = 26; // must match .row height in style.css
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
  setStatus(`Scanned in ${(summary.scanMs / 1000).toFixed(1)} s (${how}), categorised in ${summary.categoriseMs} ms`);
  $('elevate').hidden = !summary.adminWouldHelp;
  await expand(0);
  select(0);
  $('scroller').focus();
  map.details = new Map();
  zoomTo(root.id);
  setView('overview');
}

// ---------- side panel ----------

function renderSide() {
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
  $('totals').innerHTML = `
    <div class="root-name">${esc(r.name)}</div>
    <div class="big-size">${human(r.size)}</div>
    ${drive}
    <dl>${rows.map(([k, v]) => `<dt>${k}</dt><dd>${v}</dd>`).join('')}</dl>`;

  const list = [...s.categories].filter((c) => c.files > 0).sort((a, b) => b.size - a.size);
  $('cats').innerHTML = list.map((c) => {
    const pct = r.size > 0 ? (c.size * 100) / r.size : 0;
    const color = colorOf(c);
    const safe = c.cleanup === 'safe' ? '<span class="safe">usually safe to clean</span>' : '';
    return `<li data-id="${c.id}" title="Show only ${esc(c.name)}">
      <div class="cat-head"><span class="swatch" style="background:${color}"></span>
        <span class="name">${esc(c.name)}</span><span class="size">${human(c.size)}</span></div>
      <div class="cat-bar"><span style="width:${pct}%;background:${color}"></span></div>
      <div class="cat-sub"><span>${pct.toFixed(1)}% · ${plural(c.files, 'file')}</span>${safe}</div>
    </li>`;
  }).join('');

  const note = $('rules-note');
  note.classList.toggle('error', !!s.rulesError);
  if (s.rulesError) {
    note.textContent = `Your rules file has a problem, so the built-in rules were used: ${s.rulesError}`;
  } else if (s.rulesFile) {
    note.textContent = `Your own category rules go in ${s.rulesFile}`;
  } else {
    note.textContent = '';
  }
}

// ---------- tree ----------

function sortRows(rows) {
  const dir = sort.desc ? -1 : 1;
  const byName = (a, b) => collator.compare(a.name, b.name);
  let cmp;
  if (sort.key === 'name') {
    cmp = (a, b) => dir * byName(a, b);
  } else if (sort.key === 'category') {
    const name = (r) => cats.get(r.category)?.name ?? '';
    cmp = (a, b) => dir * collator.compare(name(a), name(b)) || b.size - a.size;
  } else {
    const k = sort.key;
    cmp = (a, b) => dir * (a[k] - b[k]) || byName(a, b);
  }
  rows.sort(cmp);
}

async function expand(i) {
  const v = visible[i];
  const r = v.row;
  if (r.kind !== 'dir' || r.children === 0 || expanded.has(r.id)) return;
  let kids = cache.get(r.id);
  if (!kids) {
    const into = cache;
    try {
      kids = shownTree === 'groups'
        ? await invoke('search_children', { generation: search.generation, id: r.id })
        : await invoke('children', { id: r.id });
    } catch {
      return; // a newer search replaced this one
    }
    sortRows(kids);
    into.set(r.id, kids);
    if (cache !== into) return; // the other tree is showing now
  }
  // The list may have changed while waiting.
  const at = visible.indexOf(v);
  if (at < 0 || expanded.has(r.id)) return;
  expanded.add(r.id);
  const add = kids.map((k) => ({ row: k, depth: v.depth + 1, parentSize: r.size }));
  visible = visible.slice(0, at + 1).concat(add, visible.slice(at + 1));
  if (selected > at) selected += add.length;
  render();
}

function collapse(i) {
  const v = visible[i];
  if (!expanded.has(v.row.id)) return;
  let j = i + 1;
  const had = marked.size;
  while (j < visible.length && visible[j].depth > v.depth) {
    expanded.delete(visible[j].row.id);
    marked.delete(visible[j].row.id); // nothing hidden stays selected
    j++;
  }
  if (!marked.size) marked.add(v.row.id);
  expanded.delete(v.row.id);
  visible.splice(i + 1, j - i - 1);
  if (selected > i && selected < j) selected = i;
  else if (selected >= j) selected -= j - i - 1;
  if (marked.size !== had || !marked.has(visible[selected].row.id)) select(selected, 'keep');
  else render();
}

function toggle(i) {
  if (expanded.has(visible[i].row.id)) collapse(i);
  else expand(i);
}

/** Rebuilds the visible list from the expanded set, e.g. after sorting. */
function rebuild() {
  if (!visible.length) return;
  const selId = visible[selected]?.row.id;
  const root = visible[0];
  const out = [];
  const walk = (entry) => {
    out.push(entry);
    if (!expanded.has(entry.row.id)) return;
    for (const k of cache.get(entry.row.id) ?? []) {
      walk({ row: k, depth: entry.depth + 1, parentSize: entry.row.size });
    }
  };
  walk(root);
  visible = out;
  selected = Math.max(0, visible.findIndex((v) => v.row.id === selId));
  render();
  ensureVisible(selected);
}

function setSort(key) {
  if (sort.key === key) sort.desc = !sort.desc;
  else sort = { key, desc: !(key === 'name' || key === 'category') };
  for (const kids of cache.values()) sortRows(kids);
  markSortHeader();
  rebuild();
}

// ---------- columns ----------

// Widths in px. Name and Folder share the free space (at least `w` px)
// until they're resized. Drag a header's right edge to resize (double-click
// resets); widths are remembered. The list scrolls sideways when the columns
// don't fit.
const COLS = {
  name: { label: 'Name', w: 180, flex: 1, sort: 'name' },
  folder: { label: 'Folder', w: 160, flex: 1.3 },
  size: { label: 'Size', w: 84, num: true, sort: 'size' },
  pct: { label: '% of parent', w: 120 },
  alloc: { label: 'On disk', w: 84, num: true, sort: 'alloc' },
  files: { label: 'Files', w: 76, num: true, sort: 'files' },
  modified: { label: 'Modified', w: 92, num: true, sort: 'modified' },
  category: { label: 'Category', w: 160, sort: 'category' },
};
const TREE_COLS = ['name', 'size', 'pct', 'alloc', 'files', 'modified', 'category'];
const RESULT_COLS = ['name', 'folder', 'size', 'pct', 'alloc', 'files', 'modified', 'category'];
const MIN_COL = 40;
let colWidths = {};

const colWidth = (k) => colWidths[k] ?? COLS[k].w;
const columnsOf = (v) => (v === 'results' && !grouped ? RESULT_COLS : TREE_COLS);

function applyColumns() {
  const keys = columnsOf(view);
  const flex = (k) => COLS[k].flex && colWidths[k] === undefined;
  let cols = keys.map((k) => (flex(k) ? `minmax(${COLS[k].w}px, ${COLS[k].flex}fr)` : `${colWidth(k)}px`)).join(' ');
  if (!keys.some(flex)) cols += ' minmax(0, 1fr)'; // fills the rest
  const pane = $('thead').parentElement;
  pane.style.setProperty('--cols', cols);
  pane.style.setProperty('--cols-min', `${keys.reduce((t, k) => t + colWidth(k), 0)}px`);
}

function renderHead() {
  $('thead').innerHTML = columnsOf(view).map((k) => {
    const c = COLS[k];
    const label = k === 'pct' && flat() ? '% of all' : c.label;
    return `<div${c.sort ? ` data-key="${c.sort}"` : ''}${c.num ? ' class="num"' : ''} role="columnheader">${label}<span class="grip" data-grip="${k}" title="Drag to resize, double-click to reset"></span></div>`;
  }).join('');
  markSortHeader();
  applyColumns();
}

function markSortHeader() {
  for (const el of $('thead').children) {
    const on = el.dataset.key === sort.key;
    el.classList.toggle('sorted', on);
    el.classList.toggle('asc', on && !sort.desc);
  }
}

function rowHtml(i) {
  const { row: r, depth, parentSize } = visible[i];
  const pct = parentSize > 0 ? (r.size * 100) / parentSize : 0;
  const cls = ['row', r.kind];
  const tags = [];
  if (marked.has(r.id)) cls.push('sel');
  if (i === selected && (marked.size > 1 || !marked.has(r.id))) cls.push('cur');
  if (basketIds.has(r.id)) cls.push('inbasket');
  if (r.flags & FLAG.DUP) { cls.push('dup'); tags.push('hardlink, counted once'); }
  if (r.flags & FLAG.ONLINE) { cls.push('online'); tags.push('online-only'); }
  if (r.flags & FLAG.COMPRESSED) tags.push('compressed');
  if (r.flags & FLAG.READ_ERROR) { cls.push('readerr'); tags.push('partly unreadable'); }
  if (r.kind === 'link') tags.push('link, not followed');
  if (r.flags & FLAG.EXCLUDED) tags.push('left out (Settings)');
  if (r.kind === 'error') tags.push('unreadable');

  const open = expanded.has(r.id);
  const twisty = r.kind === 'dir' && r.children > 0 ? (open ? '▾' : '▸') : '';
  const cat = cats.get(r.category);
  const catCell = cat
    ? `<span class="swatch" style="background:${colorOf(cat)}"></span>${esc(cat.name)}`
    : '';
  const tagHtml = (tags.length ? `<span class="tag">${tags.join(', ')}</span>` : '')
    + (basketIds.has(r.id) ? '<span class="tag basket">in basket</span>' : '');

  const hits = shownTree === 'groups' && r.kind === 'dir' && !r.hit
    ? `<span class="hits" title="Matching files in it">(${fmtInt(r.files)})</span>` : '';
  return `<div class="${cls.join(' ')}" data-i="${i}" role="treeitem" aria-level="${depth + 1}"${r.kind === 'dir' && r.children > 0 ? ` aria-expanded="${open}"` : ''}>
    <div class="name-cell" style="padding-left:${4 + depth * 16}px"><span class="twisty">${twisty}</span>${riskHtml(r)}<span class="label" title="${esc(r.name)}">${esc(r.name)}</span>${hits}${tagHtml}</div>
    <div class="num">${human(r.size)}</div>
    <div class="pct"><div class="bar"><span style="width:${Math.min(100, pct)}%"></span></div><span class="val">${pct.toFixed(1)}%</span></div>
    <div class="num">${human(r.alloc)}</div>
    <div class="num">${r.kind === 'dir' ? fmtInt(r.files) : ''}</div>
    <div class="num">${fmtDate(r.modified)}</div>
    <div class="cat-cell">${catCell}</div>
  </div>`;
}

function render() {
  if (flat()) return renderResults();
  const sc = $('scroller');
  $('spacer').style.height = `${visible.length * ROW_H}px`;
  const first = Math.max(0, Math.floor(sc.scrollTop / ROW_H) - OVERSCAN);
  const last = Math.min(visible.length, Math.ceil((sc.scrollTop + sc.clientHeight) / ROW_H) + OVERSCAN);
  const rows = $('rows');
  rows.style.transform = `translateY(${first * ROW_H}px)`;
  let html = '';
  for (let i = first; i < last; i++) html += rowHtml(i);
  rows.innerHTML = html;
}

let frame = 0;
function renderSoon() {
  if (!frame) frame = requestAnimationFrame(() => { frame = 0; render(); });
}

function ensureVisible(i) {
  const sc = $('scroller');
  const top = i * ROW_H;
  if (top < sc.scrollTop) sc.scrollTop = top;
  else if (top + ROW_H > sc.scrollTop + sc.clientHeight) sc.scrollTop = top + ROW_H - sc.clientHeight;
}

/**
 * Moves the cursor to row i. `how`: 'one' selects just that row; 'toggle'
 * adds or removes it (Ctrl+click); 'range' selects from the anchor to it
 * (Shift); 'addRange' adds that range (Ctrl+Shift); 'keep' only moves the
 * cursor (Ctrl+arrows).
 */
async function select(i, how = 'one') {
  if (i < 0 || i >= visible.length) return;
  const id = visible[i].row.id;
  if (how === 'toggle') {
    if (marked.has(id)) marked.delete(id);
    else marked.add(id);
    anchorId = id;
  } else if (how === 'range' || how === 'addRange') {
    let a = visible.findIndex((v) => v.row.id === anchorId);
    if (a < 0) a = Math.max(0, selected);
    if (how === 'range') marked = new Set();
    for (let k = Math.min(a, i); k <= Math.max(a, i); k++) marked.add(visible[k].row.id);
  } else if (how !== 'keep') {
    marked = new Set([id]);
    anchorId = id;
  }
  selected = i;
  ensureVisible(i);
  render();
  drawOverlay();
  if (marked.size > 1) return showMany(marked.size, treeMarkedBytes());
  const risk = visible[i].row.risk;
  try {
    const path = await invoke('node_path', { id });
    if (visible[selected]?.row.id === id) {
      $('selpath').textContent = risk ? `${path}  ·  ${risk.reason}` : path;
      $('reveal').hidden = false;
    }
  } catch (e) {
    $('selpath').textContent = String(e);
  }
}

/** Size of the selected rows, not counting rows inside a selected folder twice. */
function treeMarkedBytes() {
  let bytes = 0;
  let inside = -1; // depth of the selected folder being walked through
  for (const v of visible) {
    if (inside >= 0 && v.depth > inside) continue;
    inside = -1;
    if (marked.has(v.row.id)) {
      bytes += v.row.size;
      inside = v.depth;
    }
  }
  return bytes;
}

/** Whether tree row i is in the basket, itself or inside a folder that is. */
function treeRowInBasket(i) {
  for (; i >= 0; i = parentIndex(i)) if (basketIds.has(visible[i].row.id)) return true;
  return false;
}

/** A drive (in an All drives scan), which the basket refuses. */
function isDriveRoot(id) {
  const row = visible.find((v) => v.row.id === id)?.row;
  return !!row && /^([A-Za-z]:\\|\\\\[^\\]+\\[^\\]+\\?)$/.test(row.name);
}

/** Status bar for a selection of several rows. */
function showMany(n, bytes) {
  $('selpath').textContent = `${fmtInt(n)} items selected${bytes === undefined ? '' : ` · ${human(bytes)}`}`;
  $('reveal').hidden = true;
}

/** How a click or key with these modifiers changes the selection. */
function selectHow(e, moving) {
  if (e.shiftKey) return e.ctrlKey ? 'addRange' : 'range';
  if (e.ctrlKey) return moving ? 'keep' : 'toggle';
  return 'one';
}

function parentIndex(i) {
  const d = visible[i].depth;
  for (let j = i - 1; j >= 0; j--) if (visible[j].depth < d) return j;
  return -1;
}

function onKey(e) {
  if (keyMenu(e) || itemKey(e)) return;
  if (flat()) return onResultsKey(e);
  if (selected < 0) return;
  const page = Math.max(1, Math.floor($('scroller').clientHeight / ROW_H) - 1);
  const v = visible[selected];
  const how = selectHow(e, true);
  switch (e.key) {
    case 'ArrowDown': select(Math.min(visible.length - 1, selected + 1), how); break;
    case 'ArrowUp': select(Math.max(0, selected - 1), how); break;
    case 'PageDown': select(Math.min(visible.length - 1, selected + page), how); break;
    case 'PageUp': select(Math.max(0, selected - page), how); break;
    case 'Home': select(0, how); break;
    case 'End': select(visible.length - 1, how); break;
    case ' ': if (!e.ctrlKey) return; select(selected, 'toggle'); break;
    case 'a': case 'A':
      if (!e.ctrlKey) return;
      marked = new Set(visible.map((x) => x.row.id));
      select(selected, 'keep');
      break;
    case 'ArrowRight':
      if (v.row.kind === 'dir' && v.row.children > 0) {
        if (!expanded.has(v.row.id)) expand(selected);
        else select(selected + 1);
      }
      break;
    case 'ArrowLeft':
      if (expanded.has(v.row.id)) collapse(selected);
      else select(parentIndex(selected));
      break;
    case 'Enter': toggle(selected); break;
    case 'b': case 'B': case 'Delete': basketToggleSelection(); break;
    default: return;
  }
  e.preventDefault();
}

// ---------- treemap ----------

// Rectangle kinds, as in src/treemap.rs.
const K_FILE = 0, K_DIR = 1, K_REST = 2;
const RECORD = 24; // bytes per rectangle

const map = {
  root: 0,     // node id the treemap shows
  n: 0,        // rectangle count; the arrays below hold n entries
  x: null, y: null, w: null, h: null, id: null, cat: null, depth: null, kind: null,
  index: new Map(), // node id -> rectangle index (files and folders)
  hover: -1,
  seq: 0,
  details: new Map(), // node id -> { row, path }, for tooltips
};

function decodeRects(buf) {
  const n = Math.floor(buf.byteLength / RECORD);
  const dv = new DataView(buf);
  const f = () => new Float32Array(n);
  Object.assign(map, {
    n, x: f(), y: f(), w: f(), h: f(),
    id: new Uint32Array(n), cat: new Uint16Array(n), depth: new Uint8Array(n), kind: new Uint8Array(n),
    dim: new Uint8Array(n),
  });
  map.index = new Map();
  for (let i = 0, o = 0; i < n; i++, o += RECORD) {
    map.x[i] = dv.getFloat32(o, true);
    map.y[i] = dv.getFloat32(o + 4, true);
    map.w[i] = dv.getFloat32(o + 8, true);
    map.h[i] = dv.getFloat32(o + 12, true);
    map.id[i] = dv.getUint32(o + 16, true);
    map.cat[i] = dv.getUint16(o + 20, true);
    map.depth[i] = dv.getUint8(o + 22);
    const kind = dv.getUint8(o + 23);
    map.kind[i] = kind & 0x7f;
    map.dim[i] = kind >> 7; // set while a filter is on: no match here
    if (map.kind[i] !== K_REST) map.index.set(map.id[i], i);
  }
}

async function layoutMap() {
  if (!summary) return;
  const wrap = $('map-wrap');
  const dpr = window.devicePixelRatio || 1;
  const width = Math.max(1, Math.round(wrap.clientWidth * dpr));
  const height = Math.max(1, Math.round(wrap.clientHeight * dpr));
  const seq = ++map.seq;
  const buf = await invoke('treemap', {
    id: map.root, width, height,
    minArea: 6 * dpr * dpr, // merge items under ~6 square CSS pixels
    pad: Math.max(1, Math.round(dpr)),
  });
  if (seq !== map.seq) return; // a newer layout was requested meanwhile
  decodeRects(buf);
  map.dpr = dpr;
  for (const c of [$('map'), $('map-overlay')]) {
    c.width = width;
    c.height = height;
  }
  map.hover = -1;
  drawMap();
  drawOverlay();
}

function cssVar(name) {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

/** True if folder rectangle i has nothing drawn inside it. */
function isCollapsed(i) {
  return i + 1 >= map.n || map.depth[i + 1] <= map.depth[i];
}

/** A CSS colour as [r, g, b, a] (0-255, alpha 0-1), resolved by a canvas. */
const rgbaCache = new Map();
function rgba(color) {
  let c = rgbaCache.get(color);
  if (!c) {
    const ctx = document.createElement('canvas').getContext('2d', { willReadFrequently: true });
    ctx.fillStyle = color;
    ctx.fillRect(0, 0, 1, 1);
    const [r, g, b, a] = ctx.getImageData(0, 0, 1, 1).data;
    c = [r, g, b, a / 255];
    rgbaCache.set(color, c);
  }
  return c;
}

// Cushion shading, after van Wijk and van de Wetering, "Cushion Treemaps"
// (1999): each level of folders adds a parabolic ridge across its block, so
// a block's surface is the sum of its ancestors' ridges and its own, and the
// light (from the top left) shows the nesting as bumps on bumps.
const CUSHION = { height: 0.18, falloff: 0.75, ambient: 0.3, diffuse: 0.75 };
const LIGHT = (() => {
  const l = [-1, -1, 3];
  const n = Math.hypot(...l);
  return l.map((v) => v / n);
})();

function drawMap() {
  const canvas = $('map');
  const ctx = canvas.getContext('2d', { alpha: false, willReadFrequently: true });
  const W = canvas.width, H = canvas.height;
  const frames = [cssVar('--map-frame-a'), cssVar('--map-frame-b')];
  ctx.fillStyle = cssVar('--map-bg');
  ctx.fillRect(0, 0, W, H);
  // Open folders are frames behind their contents.
  for (let i = 0; i < map.n; i++) {
    if (map.kind[i] === K_DIR && !isCollapsed(i)) {
      ctx.fillStyle = frames[map.depth[i] % 2];
      ctx.fillRect(map.x[i], map.y[i], map.w[i], map.h[i]);
    }
  }
  if (!map.n || !W || !H) return;

  const img = ctx.getImageData(0, 0, W, H);
  const px = img.data;
  const rest = rgba(cssVar('--map-rest'));
  const dim = rgba(cssVar('--map-dim'));
  const other = rgba(cssVar('--map-other'));
  const colours = new Map(); // category id -> [r, g, b, a]
  // Surface coefficients per depth on the path to the current block:
  // z(x) = s2x·x² + s1x·x, and the same for y.
  const surf = new Float64Array(257 * 4);
  const [lx, ly, lz] = LIGHT;
  for (let i = 0; i < map.n; i++) {
    const x = map.x[i], y = map.y[i], w = map.w[i], h = map.h[i];
    const d = map.depth[i];
    const at = (d + 1) * 4, up = d * 4; // slot d + 1; the parent's is slot d
    let s1x = d ? surf[up] : 0, s2x = d ? surf[up + 1] : 0;
    let s1y = d ? surf[up + 2] : 0, s2y = d ? surf[up + 3] : 0;
    const ridge = CUSHION.height * CUSHION.falloff ** d;
    if (w > 0) { s1x += (4 * ridge * (2 * x + w)) / w; s2x -= (4 * ridge) / w; }
    if (h > 0) { s1y += (4 * ridge * (2 * y + h)) / h; s2y -= (4 * ridge) / h; }
    surf[at] = s1x; surf[at + 1] = s2x; surf[at + 2] = s1y; surf[at + 3] = s2y;

    const kind = map.kind[i];
    if (kind === K_DIR && !isCollapsed(i)) continue;
    // Leaves: files, folders too small to open, and "smaller items" blocks.
    // Leave a 1 px seam on the right and bottom so neighbours stay apart.
    const gw = w >= 3 ? w - 1 : w, gh = h >= 3 ? h - 1 : h;
    let base = other;
    if (kind <= K_REST && cats.has(map.cat[i])) {
      base = colours.get(map.cat[i]);
      if (!base) colours.set(map.cat[i], (base = rgba(colorOf(cats.get(map.cat[i])))));
    }
    // A veil over the block: "smaller items", or no match while filtering.
    const veil = map.dim[i] ? dim : kind === K_REST ? rest : null;
    const va = veil ? veil[3] : 0;
    const x0 = Math.max(0, Math.round(x)), x1 = Math.min(W, Math.round(x + gw));
    const y0 = Math.max(0, Math.round(y)), y1 = Math.min(H, Math.round(y + gh));
    for (let py = y0; py < y1; py++) {
      const ny = -(2 * s2y * (py + 0.5) + s1y);
      let o = (py * W + x0) * 4;
      for (let qx = x0; qx < x1; qx++, o += 4) {
        const nx = -(2 * s2x * (qx + 0.5) + s1x);
        const cos = (nx * lx + ny * ly + lz) / Math.sqrt(nx * nx + ny * ny + 1);
        const k = CUSHION.ambient + CUSHION.diffuse * Math.max(0, cos);
        let r = base[0] * k, g = base[1] * k, b = base[2] * k;
        if (va) {
          r = r * (1 - va) + veil[0] * va;
          g = g * (1 - va) + veil[1] * va;
          b = b * (1 - va) + veil[2] * va;
        }
        px[o] = r; px[o + 1] = g; px[o + 2] = b; // clamped by the array
      }
    }
  }
  ctx.putImageData(img, 0, 0);
}

/** Rectangle of the tree's selected node, or of its nearest drawn ancestor. */
function selectedRect() {
  if (flat()) return resultRect();
  for (let i = selected; i >= 0; i = parentIndex(i)) {
    const r = map.index.get(visible[i]?.row.id);
    if (r !== undefined) return r;
  }
  return -1;
}

function drawOverlay() {
  const c = $('map-overlay');
  const ctx = c.getContext('2d');
  ctx.clearRect(0, 0, c.width, c.height);
  if (!map.n) return;
  const dpr = window.devicePixelRatio || 1;
  const outline = (i, color, width) => {
    const lw = width * dpr;
    ctx.lineWidth = lw;
    ctx.strokeStyle = color;
    ctx.strokeRect(map.x[i] + lw / 2, map.y[i] + lw / 2, Math.max(0, map.w[i] - lw), Math.max(0, map.h[i] - lw));
  };
  ctx.setLineDash([4 * dpr, 3 * dpr]);
  for (const id of basketIds) {
    const r = map.index.get(id);
    if (r !== undefined && r > 0) outline(r, '#e04848', 2);
  }
  ctx.setLineDash([]);
  // Outlining the whole map (the zoomed-in folder) would just frame it.
  const sel = selectedRect();
  if (sel > 0) {
    outline(sel, 'rgba(0,0,0,0.6)', 4);
    outline(sel, '#ffffff', 2);
  }
  if (map.hover > 0 && map.hover !== sel) outline(map.hover, 'rgba(255,255,255,0.85)', 1.5);
}

/** The deepest rectangle under a canvas pixel. Children follow their parents. */
function hitTest(px, py) {
  for (let i = map.n - 1; i >= 0; i--) {
    if (px >= map.x[i] && py >= map.y[i] && px < map.x[i] + map.w[i] && py < map.y[i] + map.h[i]) return i;
  }
  return -1;
}

function eventRect(e) {
  const r = $('map-overlay').getBoundingClientRect();
  const dpr = window.devicePixelRatio || 1;
  const cx = e.clientX - r.left, cy = e.clientY - r.top;
  return { i: hitTest(cx * dpr, cy * dpr), cx, cy };
}

async function details(id) {
  let d = map.details.get(id);
  if (!d) {
    d = await invoke('node_details', { id });
    map.details.set(id, d);
  }
  return d;
}

let tipFor = null;
async function showTip(i, cx, cy) {
  const tip = $('tip');
  if (i < 0) {
    tip.hidden = true;
    tipFor = null;
    return;
  }
  const isRest = map.kind[i] === K_REST;
  const key = `${map.id[i]}:${isRest}`;
  if (tipFor !== key) {
    tipFor = key;
    const d = await details(map.id[i]);
    if (tipFor !== key) return;
    const cat = cats.get(map.cat[i]);
    const mostly = isRest || d.row.kind === 'dir' ? ' (mostly)' : '';
    const catHtml = cat ? `<span class="swatch" style="background:${colorOf(cat)}"></span>${esc(cat.name)}${mostly}` : '';
    const share = summary.root.size > 0 ? ` · ${((d.row.size * 100) / summary.root.size).toFixed(1)}% of all` : '';
    tip.innerHTML = isRest
      ? `<div class="t-name">Smaller items in ${esc(d.row.name)}</div>
         <div class="t-meta">${catHtml}</div><div class="t-path">${esc(d.path)}</div>`
      : `<div class="t-name">${esc(d.row.name)}</div>
         <div class="t-meta">${human(d.row.size)}${share}${catHtml ? ' · ' + catHtml : ''}</div>
         <div class="t-path">${esc(d.path)}</div>`;
    tip.hidden = false;
  }
  // Keep the tip inside the map, beside the pointer.
  const wrap = $('map-wrap');
  const tw = tip.offsetWidth, th = tip.offsetHeight;
  let left = cx + 14, top = cy + 14;
  if (left + tw > wrap.clientWidth) left = Math.max(0, cx - tw - 14);
  if (top + th > wrap.clientHeight) top = Math.max(0, cy - th - 14);
  tip.style.left = `${left}px`;
  tip.style.top = `${top}px`;
}

async function zoomTo(id) {
  map.root = id;
  const chain = await invoke('ancestors', { id });
  $('crumbs').innerHTML = chain.map(([cid, name], k) => (k === chain.length - 1
    ? `<span class="here">${esc(name)}</span>`
    : `<button data-id="${cid}">${esc(name)}</button><span class="sep">›</span>`)).join('');
  await layoutMap();
}

/** The folder to zoom into for rectangle i: itself, or its parent folder. */
function zoomTarget(i) {
  if (map.kind[i] === K_REST) return map.id[i];
  if (map.kind[i] === K_DIR && map.id[i] !== map.root) return map.id[i];
  for (let j = i - 1; j >= 0; j--) {
    if (map.kind[j] === K_DIR && map.depth[j] < map.depth[i]) return map.id[j];
  }
  return -1;
}

/** Expands the tree down to node `id` and selects it. */
async function revealInTree(id) {
  const chain = await invoke('ancestors', { id });
  for (const [aid] of chain.slice(0, -1)) {
    const i = visible.findIndex((v) => v.row.id === aid);
    if (i < 0) return;
    if (!expanded.has(aid)) await expand(i);
  }
  const i = visible.findIndex((v) => v.row.id === id);
  if (i >= 0) {
    const sc = $('scroller');
    sc.scrollTop = Math.max(0, i * ROW_H - sc.clientHeight / 2);
    select(i);
  }
}

// ---------- search ----------

const PAGE = 200;   // results fetched per request
const DAY = 86400;

let view = 'tree';  // 'tree', or 'results' while a filter is on
let grouped = true; // results grouped by folder (else one list by size)
let search = null;  // { generation, count, bytes, items, pages, sel, anc }
let searchSeq = 0;
let treeScroll = 0;

function readFilter() {
  const num = (id) => ($(id).value === '' ? null : Number($(id).value));
  const age = num('f-age');
  const now = Math.floor(Date.now() / 1000);
  return {
    name: $('f-name').value,
    extensions: $('f-ext').value.split(/[\s,;]+/).filter(Boolean),
    minSize: num('f-min'),
    maxSize: num('f-max'),
    // Negative: changed within the last N days; positive: not for N days.
    modifiedAfter: age !== null && age < 0 ? now + age * DAY : null,
    modifiedBefore: age !== null && age > 0 ? now - age * DAY : null,
    category: num('f-cat'),
    folders: $('f-folders').checked,
  };
}

/** Same rule as Filter::is_empty in src/search.rs. */
function filterIsEmpty(f) {
  return !f.name.trim() && f.extensions.length === 0 && f.minSize === null && f.maxSize === null
    && f.modifiedAfter === null && f.modifiedBefore === null && f.category === null;
}

function resetFilters() {
  for (const id of ['f-name', 'f-ext', 'f-min', 'f-max', 'f-age', 'f-cat']) $(id).value = '';
  $('f-folders').checked = false;
}

async function applyFilters() {
  if (!summary) return;
  const f = readFilter();
  const seq = ++searchSeq;
  const r = await invoke('search', { filter: f });
  if (seq !== searchSeq) return; // the user kept typing
  const active = !filterIsEmpty(f);
  for (const li of $('cats').children) li.classList.toggle('on', li.dataset.id === $('f-cat').value);
  $('f-clear').hidden = !active;
  if (!active) {
    search = null;
    $('f-summary').textContent = '';
    setView(view === 'results' ? 'tree' : view);
  } else {
    search = { generation: r.generation, count: r.count, bytes: r.bytes, items: [], pages: new Set(), sel: -1, anc: [], marked: new Set(), anchor: -1 };
    const groups = newTree(r.root);
    if (shownTree === 'groups') ({ cache, expanded, visible, selected, marked, anchorId } = groups);
    else otherTree = groups;
    const what = r.count === 1 ? 'match' : 'matches';
    $('f-summary').textContent = r.count === 0
      ? 'Nothing matches these filters.'
      : `${fmtInt(r.count)} ${what} · ${human(r.bytes)} in files · ${r.millis} ms`;
    if (view === 'results') $('scroller').scrollTop = 0;
    setView('results');
  }
  layoutMap();
}

let filterTimer = 0;
function applyFiltersSoon() {
  clearTimeout(filterTimer);
  filterTimer = setTimeout(applyFilters, 200);
}

function setView(v) {
  if (v === 'results' && !search) v = 'tree';
  const prev = view;
  if (prev === 'tree' && v !== 'tree') treeScroll = $('scroller').scrollTop;
  view = v;
  // Overview, History and Duplicates are panels; Tree and Results are the list.
  const panel = v === 'overview' || v === 'history' || v === 'dupes' || v === 'types';
  $('overview').hidden = v !== 'overview';
  $('history').hidden = v !== 'history';
  $('dupes').hidden = v !== 'dupes';
  $('types').hidden = v !== 'types';
  $('thead').hidden = panel;
  $('scroller').hidden = panel;
  if (prev !== v && !panel) $('scroller').scrollTop = v === 'tree' ? treeScroll : 0;
  for (const b of $('viewbar').querySelectorAll('[data-view]')) {
    b.classList.toggle('on', b.dataset.view === v);
    b.setAttribute('aria-selected', String(b.dataset.view === v));
  }
  $('viewbar').querySelector('[data-view=results]').hidden = !search;
  if (!panel) useTree(v === 'results' && grouped ? 'groups' : 'tree');
  $('thead').classList.toggle('results', flat());
  renderHead();
  // Grouped results open with the scanned folder's matches showing.
  if (shownTree === 'groups' && visible.length === 1 && !expanded.has(visible[0].row.id)) expand(0);
  if (v === 'overview') renderOverview();
  else if (v === 'history') renderHistory();
  else if (v === 'dupes') renderDupes();
  else if (v === 'types') renderTypes();
  else render();
  drawOverlay();
}

function loadPage(p) {
  const s = search;
  if (s.pages.has(p)) return;
  s.pages.add(p);
  invoke('search_page', { generation: s.generation, offset: p * PAGE, limit: PAGE })
    .then((items) => {
      if (search !== s) return;
      items.forEach((it, k) => { s.items[p * PAGE + k] = it; });
      renderSoon();
    })
    .catch(() => {}); // a newer search replaced this one
}

function resultHtml(i) {
  const it = search.items[i];
  if (!it) {
    loadPage(Math.floor(i / PAGE));
    return `<div class="row" data-i="${i}"><div class="name-cell"><span class="label">…</span></div></div>`;
  }
  const r = it.row;
  const pct = summary.root.size > 0 ? (r.size * 100) / summary.root.size : 0;
  const cat = cats.get(r.category);
  const cls = ['row', r.kind];
  if (search.marked.has(i)) cls.push('sel');
  if (basketIds.has(r.id)) cls.push('inbasket');
  if (i === search.sel && (search.marked.size > 1 || !search.marked.has(i))) cls.push('cur');
  if (r.flags & FLAG.DUP) cls.push('dup');
  const catCell = cat ? `<span class="swatch" style="background:${colorOf(cat)}"></span>${esc(cat.name)}` : '';
  return `<div class="${cls.join(' ')}" data-i="${i}" role="option" title="${esc(it.path)}">
    <div class="name-cell" style="padding-left:8px">${riskHtml(r)}<span class="label">${esc(r.name)}</span>${basketIds.has(r.id) ? '<span class="tag basket">in basket</span>' : ''}</div>
    <div class="folder-cell">${folderLinks(it)}</div>
    <div class="num">${human(r.size)}</div>
    <div class="pct"><div class="bar"><span style="width:${Math.min(100, pct)}%"></span></div><span class="val">${pct.toFixed(1)}%</span></div>
    <div class="num">${human(r.alloc)}</div>
    <div class="num">${r.kind === 'dir' ? fmtInt(r.files) : ''}</div>
    <div class="num">${fmtDate(r.modified)}</div>
    <div class="cat-cell">${catCell}</div>
  </div>`;
}

function renderResults() {
  const sc = $('scroller');
  const n = search.count;
  $('spacer').style.height = `${n * ROW_H}px`;
  const rows = $('rows');
  if (n === 0) {
    rows.style.transform = '';
    rows.innerHTML = '<div class="no-results">Nothing matches these filters.</div>';
    return;
  }
  const first = Math.max(0, Math.floor(sc.scrollTop / ROW_H) - OVERSCAN);
  const last = Math.min(n, Math.ceil((sc.scrollTop + sc.clientHeight) / ROW_H) + OVERSCAN);
  rows.style.transform = `translateY(${first * ROW_H}px)`;
  let html = '';
  for (let i = first; i < last; i++) html += resultHtml(i);
  rows.innerHTML = html;
}

/** Moves the cursor to result i; `how` as for `select`. */
/**
 * A result's folder as links, one per folder from the scanned one down;
 * data-k is the folder's depth below the scanned folder (0 = it).
 */
function folderLinks(it) {
  const root = summary.root.name;
  const base = root.replace(/\\$/, '');
  const parent = it.path.slice(0, Math.max(0, it.path.length - it.row.name.length)).replace(/\\$/, '');
  if (!parent.startsWith(base)) return esc(parent);
  const names = [root, ...parent.slice(base.length).split('\\').filter(Boolean)];
  return names.map((n, k) => `<span class="crumb" data-k="${k}" title="Show in the tree">${esc(n)}</span>`)
    .reduce((html, link, k) => html + (k && !names[k - 1].endsWith('\\') ? '\\' : '') + link, '');
}

/** Shows the folder at depth k above result i in the tree. */
async function showFolderInTree(i, k) {
  const it = search?.items[i];
  if (!it) return;
  const chain = await invoke('ancestors', { id: it.row.id });
  const id = chain[k]?.[0];
  if (id === undefined) return;
  setView('tree');
  await revealInTree(id);
  $('scroller').focus();
}

/** Moves the cursor to result i; `how` as for `select`. */
async function selectResult(i, how = 'one') {
  const s = search;
  if (!s || i < 0 || i >= s.count) return;
  if (how === 'toggle') {
    if (s.marked.has(i)) s.marked.delete(i);
    else s.marked.add(i);
    s.anchor = i;
  } else if (how === 'range' || how === 'addRange') {
    const a = s.anchor >= 0 ? s.anchor : Math.max(0, s.sel);
    if (how === 'range') s.marked = new Set();
    for (let k = Math.min(a, i); k <= Math.max(a, i); k++) s.marked.add(k);
  } else if (how !== 'keep') {
    s.marked = new Set([i]);
    s.anchor = i;
  }
  s.sel = i;
  s.anc = [];
  ensureVisible(i);
  render();
  drawOverlay();
  if (s.marked.size > 1) return showMany(s.marked.size);
  const it = s.items[i];
  if (!it) return;
  $('selpath').textContent = it.path;
  $('reveal').hidden = false;
  // Its ancestors let the treemap outline the nearest block that is drawn.
  const chain = await invoke('ancestors', { id: it.row.id });
  if (search !== s || s.sel !== i) return;
  s.anc = chain.map(([id]) => id).reverse();
  drawOverlay();
}

/** Treemap rectangle of the selected result, or of its nearest drawn ancestor. */
function resultRect() {
  for (const id of search?.anc ?? []) {
    const r = map.index.get(id);
    if (r !== undefined) return r;
  }
  return -1;
}

function showInTree(i) {
  const id = search?.items[i]?.row.id;
  if (id === undefined) return;
  setView('tree');
  revealInTree(id);
}

function onResultsKey(e) {
  const s = search;
  if (!s || s.count === 0) return;
  const page = Math.max(1, Math.floor($('scroller').clientHeight / ROW_H) - 1);
  const at = Math.max(0, s.sel);
  const how = selectHow(e, true);
  switch (e.key) {
    case 'ArrowDown': selectResult(Math.min(s.count - 1, s.sel + 1), how); break;
    case 'ArrowUp': selectResult(Math.max(0, at - 1), how); break;
    case 'PageDown': selectResult(Math.min(s.count - 1, at + page), how); break;
    case 'PageUp': selectResult(Math.max(0, at - page), how); break;
    case 'Home': selectResult(0, how); break;
    case 'End': selectResult(s.count - 1, how); break;
    case ' ': if (!e.ctrlKey) return; selectResult(at, 'toggle'); break;
    case 'a': case 'A':
      if (!e.ctrlKey) return;
      s.marked = new Set(Array.from({ length: s.count }, (_, k) => k));
      selectResult(at, 'keep');
      break;
    case 'Enter': showInTree(at); break;
    case 'b': case 'B': case 'Delete': basketToggleSelection(); break;
    default: return;
  }
  e.preventDefault();
}

/** Id of the selected node in whichever list is showing. */
function currentId() {
  return flat() ? search?.items[search.sel]?.row.id : visible[selected]?.row.id;
}

/** Ids of every selected row in the list showing (the cursor's if none). */
async function selectionIds() {
  if (!flat()) {
    const ids = marked.size ? [...marked] : [currentId()];
    // A folder in grouped results stands for its matches, not all it holds.
    if (shownTree !== 'groups') return ids;
    return invoke('search_matches_in', { generation: search.generation, ids });
  }
  const s = search;
  if (!s) return [];
  if (!s.marked.size) return [currentId()];
  // Results load a page at a time, so ask for the ids of whole ranges.
  const idx = [...s.marked].sort((a, b) => a - b);
  const ranges = [];
  for (const k of idx) {
    const last = ranges[ranges.length - 1];
    if (last && last[1] === k) last[1] = k + 1;
    else ranges.push([k, k + 1]);
  }
  return invoke('search_ids', { generation: s.generation, ranges });
}

/** B or Delete: the selected rows go in the basket, or out if all are in. */
async function basketToggleSelection() {
  try {
    await basketToggle(await selectionIds());
  } catch (e) {
    setStatus(String(e), true);
  }
}

// ---------- cleanup basket ----------

const EMPTY_BASKET = { items: [], count: 0, bytes: 0, blocked: 0, danger: 0 };
let basket = EMPTY_BASKET;
let basketIds = new Set();

function setBasket(v) {
  basket = v;
  basketIds = new Set(v.items.map((it) => it.id));
  renderBasket();
  render();
  drawOverlay();
}

/**
 * Adds the node (or a list of nodes) to the basket. A node that's in is
 * taken out; a list is taken out only if every node in it is in.
 */
async function basketToggle(id) {
  if (!summary) return;
  if (Array.isArray(id)) {
    const ids = id.filter((x) => x !== undefined && x !== summary.root.id);
    if (ids.length !== 1 || id.length !== 1) return basketToggleMany(ids, ids.length < id.length);
    id = ids[0];
  }
  if (id === undefined) return;
  if (id === summary.root.id) {
    setStatus("The scanned folder itself can't go in the basket; pick what's inside it.");
    return;
  }
  const removing = basketIds.has(id);
  try {
    setBasket(await invoke(removing ? 'basket_remove' : 'basket_add', { ids: [id] }));
  } catch (e) {
    setStatus(String(e), true);
    return;
  }
  if (removing) setStatus('Taken out of the cleanup basket.');
  else if (basketIds.has(id)) setStatus(`Added ${basket.items.find((it) => it.id === id).name} to the cleanup basket.`);
  else if (isDriveRoot(id)) setStatus("A whole drive can't go in the basket; pick what's inside it.");
  else setStatus('Already in the basket, as part of a folder.');
}

async function basketToggleMany(ids, skippedRoot) {
  const rootNote = skippedRoot ? " The scanned folder itself can't go in." : '';
  if (!ids.length) {
    if (skippedRoot) setStatus("The scanned folder itself can't go in the basket; pick what's inside it.");
    return;
  }
  // Adding, unless that changes nothing (everything is in already, some
  // perhaps as part of a selected folder): then they come out.
  const before = basket.items.map((it) => it.id).join();
  let removed = 0;
  try {
    if (!ids.every((x) => basketIds.has(x))) setBasket(await invoke('basket_add', { ids }));
    if (basket.items.map((it) => it.id).join() === before) {
      removed = ids.filter((x) => basketIds.has(x)).length;
      setBasket(await invoke('basket_remove', { ids }));
    }
  } catch (e) {
    setStatus(String(e), true);
    return;
  }
  if (removed) {
    setStatus(`Took ${plural(removed, 'item')} out of the cleanup basket.${rootNote}`);
    return;
  }
  const inside = ids.filter((x) => !basketIds.has(x)).length;
  const insideNote = inside ? ` ${fmtInt(inside)} of them ${inside === 1 ? 'is' : 'are'} inside folders in the basket.` : '';
  setStatus(`Added ${plural(ids.length, 'item')} to the cleanup basket.${insideNote}${rootNote}`);
}

const BASKET_SIDE = 100;  // rows in the side panel
const BASKET_BIG = 1000;  // rows in the big view and the delete dialog

function renderBasket() {
  const b = basket;
  $('basket-count').textContent = b.items.length ? `(${b.items.length})` : '';
  $('basket-empty').hidden = b.items.length > 0;
  $('basket-foot').hidden = b.items.length === 0;
  // Drawing tens of thousands of rows is slow; the big view has more.
  const more = b.items.length - BASKET_SIDE;
  $('basket-items').innerHTML = b.items.slice(0, BASKET_SIDE).map((it) => `<li class="lvl-${it.level}" data-id="${it.id}" title="${esc(it.path)}">
      <div class="b-head"><span class="b-dot"></span><span class="b-name">${esc(it.name)}</span>
        <span class="b-size">${human(it.size)}</span><button class="b-x" data-remove="${it.id}" title="Take out of the basket" aria-label="Take ${esc(it.name)} out of the basket">×</button></div>
      ${it.reason ? `<div class="b-reason">${esc(it.reason)}</div>` : ''}
    </li>`).join('') + (more > 0 ? `<li class="b-more" data-open-basket>…and ${fmtInt(more)} more. Open the basket to see them.</li>` : '');
  $('basket-total').textContent = b.count
    ? `${plural(b.count, 'item')} · ${human(b.bytes)}`
    : 'Nothing here can be deleted.';
  $('basket-delete').disabled = b.count === 0;
  if (basketOpen) {
    const body = $('modal-body');
    const top = body.scrollTop;
    body.innerHTML = basketBody();
    body.scrollTop = top;
    $('modal-actions').children[2].disabled = b.count === 0;
  }
}

// Context menu on tree rows, results and treemap blocks.
let menuReturn = null; // where focus goes back to when the menu closes

/** Shows `actions` ([label, fn] pairs) as a menu at a screen point. */
function openMenuAt(x, y, actions) {
  const menu = $('menu');
  menuReturn = document.activeElement;
  menu.innerHTML = actions.map(([label], k) => (label === '-' ? '<hr>'
    : `<button role="menuitem" data-k="${k}">${esc(label)}</button>`)).join('');
  menu.onclick = (ev) => {
    const k = ev.target.closest('[data-k]')?.dataset.k;
    closeMenu();
    if (k !== undefined) actions[Number(k)][1]();
  };
  menu.hidden = false;
  menu.style.left = `${Math.max(0, Math.min(x, window.innerWidth - menu.offsetWidth - 4))}px`;
  menu.style.top = `${Math.max(0, Math.min(y, window.innerHeight - menu.offsetHeight - 4))}px`;
  menu.querySelector('button').focus();
}

function openMenu(e, id, { zoomId, showInTree, many, allIn, matches, dir } = {}) {
  e.preventDefault();
  if (id === undefined) return;
  const what = many > 1 ? ` ${fmtInt(many)} items` : '';
  let toggle;
  if (matches) toggle = [allIn ? `Take${what} out of cleanup basket` : 'Add the matching files to cleanup basket', basketToggleSelection];
  else if (many) toggle = [allIn ? `Take${what} out of cleanup basket` : `Add${what} to cleanup basket`, basketToggleSelection];
  else toggle = [basketIds.has(id) ? 'Take out of cleanup basket' : 'Add to cleanup basket', () => basketToggle(id)];
  const act = (action) => () => itemAction(id, action);
  const actions = [
    toggle,
    '-',
    [dir ? 'Open in Explorer' : 'Open', act('open')],
    ['Show in Explorer', () => invoke('reveal', { id }).catch((err) => setStatus(String(err), true))],
    ['Open in Terminal', act('terminal')],
    ['Copy path', () => copyPath(id)],
    ['Properties', act('properties')],
    '-',
  ].map((a) => (a === '-' ? ['-'] : a));
  if (dir) actions.push(['Rescan this folder', () => rescanFolder(id)]);
  if (showInTree) actions.push(['Show in tree', () => { setView('tree'); revealInTree(id); }]);
  if (zoomId !== undefined && zoomId >= 0) actions.push(['Zoom treemap here', () => zoomTo(zoomId)]);
  if (actions[actions.length - 1][0] === '-') actions.pop();
  openMenuAt(e.clientX, e.clientY, actions);
}

function closeMenu() {
  const menu = $('menu');
  if (menu.hidden) return;
  menu.hidden = true;
  menuReturn?.focus?.();
  menuReturn = null;
}

/** The context menu for row i of the list showing, at (x, y). */
function rowMenu(i, x, y) {
  const at = { preventDefault() {}, clientX: x, clientY: y };
  // Right-clicking a selected row keeps the selection, as in Explorer.
  if (flat()) {
    const keep = search?.marked.has(i) && search.marked.size > 1;
    selectResult(i, keep ? 'keep' : 'one');
    const r = search?.items[i]?.row;
    const many = keep ? search.marked.size : 0;
    const paths = basket.items.map((it) => it.path);
    const covered = (p) => paths.some((b) => p === b || p.startsWith(b.endsWith('\\') ? b : b + '\\'));
    const allIn = many > 0 && [...search.marked].every((k) => search.items[k] && covered(search.items[k].path));
    openMenu(at, r?.id, { showInTree: !many, many, allIn, dir: r?.kind === 'dir', zoomId: r?.kind === 'dir' ? r.id : undefined });
  } else {
    const r = visible[i].row;
    const keep = marked.has(r.id) && marked.size > 1;
    select(i, keep ? 'keep' : 'one');
    const n = keep ? marked.size : 1;
    const allIn = visible.every((v, k) => !marked.has(v.row.id) || treeRowInBasket(k));
    // In grouped results a folder that doesn't match stands for its matches.
    const matches = shownTree === 'groups' && !allIn && visible.some((v) => marked.has(v.row.id) && v.row.kind === 'dir' && !v.row.hit);
    openMenu(at, r.id, { many: n > 1 || shownTree === 'groups' ? n : 0, allIn, matches, dir: r.kind === 'dir', zoomId: r.kind === 'dir' ? r.id : undefined });
  }
}

/** Keys for the selected item: F5 rescans its folder, Ctrl+C copies its
 *  path, Alt+Enter shows its properties, Ctrl+Enter opens it. */
function itemKey(e) {
  const id = currentId();
  if (id === undefined) return false;
  if (e.key === 'F5') {
    // A file's folder in the tree; in the flat results only folders.
    let target = id;
    if (!flat() && visible[selected]?.row.kind !== 'dir') target = visible[parentIndex(selected)]?.row.id;
    else if (flat() && search.items[search.sel]?.row.kind !== 'dir') target = undefined;
    if (target !== undefined) rescanFolder(target);
  } else if (e.key.toLowerCase() === 'c' && e.ctrlKey && !e.shiftKey) {
    copyPath(id);
  } else if (e.key === 'Enter' && e.altKey) {
    itemAction(id, 'properties');
  } else if (e.key === 'Enter' && e.ctrlKey) {
    itemAction(id, 'open');
  } else {
    return false;
  }
  e.preventDefault();
  return true;
}

async function itemAction(id, action) {
  try {
    await invoke('item_action', { id, action });
  } catch (e) {
    setStatus(String(e), true);
  }
}

async function copyPath(id) {
  try {
    const path = await invoke('node_path', { id });
    await navigator.clipboard.writeText(path);
    setStatus(`Copied ${path}`);
  } catch (e) {
    setStatus(`Couldn't copy the path: ${e}`, true);
  }
}

// ----- rescanning one folder -----

let rescanning = null; // { id, path, before } while a folder is being rescanned

async function rescanFolder(id) {
  if (!summary) return;
  if (id === summary.root.id) {
    if (lastPath) startScan(lastPath);
    return;
  }
  let before = 0;
  try {
    before = (await invoke('node_details', { id })).row.size;
    rescanning = { id, path: await invoke('rescan_folder', { id }), before };
  } catch (e) {
    setStatus(String(e), true);
    return;
  }
  setScanning(true);
  setStatus(`Rescanning ${rescanning.path}…`);
}

listen('rescan-progress', (e) => {
  if (rescanning) setStatus(`Rescanning ${rescanning.path}… ${plural(e.payload.files, 'file')}, ${human(e.payload.bytes)}`);
});
listen('rescan-done', async () => {
  const r = rescanning;
  rescanning = null;
  setScanning(false);
  // Zoomed into something that was replaced: zoom to the rescanned folder.
  try {
    if ((await invoke('node_details', { id: map.root })).row.flags & FLAG.REMOVED) map.root = r.id;
  } catch { map.root = r.id; }
  await refreshAfterDelete();
  const now = (await invoke('node_details', { id: r.id })).row.size;
  setStatus(`Rescanned ${r.path}: ${human(now)} (${signed(now - r.before)} since the scan).`);
});
listen('rescan-error', (e) => {
  rescanning = null;
  setScanning(false);
  const cancelled = e.payload === 'scan cancelled';
  setStatus(cancelled ? 'Rescan cancelled.' : `Rescan failed: ${e.payload}`, !cancelled);
});

/** Menu key or Shift+F10 on the list: the menu under the selected row. */
function keyMenu(e) {
  if (!(e.key === 'ContextMenu' || (e.key === 'F10' && e.shiftKey))) return false;
  e.preventDefault();
  const i = flat() ? search?.sel : selected;
  if (i === undefined || i < 0) return true;
  const el = document.querySelector(`#rows .row[data-i="${i}"]`);
  const r = el?.getBoundingClientRect();
  if (r) rowMenu(i, r.left + 40, r.bottom);
  return true;
}

// ----- deleting -----

function modal(title, body, actions) {
  basketOpen = false;
  $('modal').querySelector('.modal-card').classList.remove('wide');
  $('modal-body').onclick = null;
  $('modal-body').onchange = null;
  $('modal-title').textContent = title;
  $('modal-body').innerHTML = body;
  $('modal-actions').innerHTML = actions.map(([label, cls], k) => `<button class="${cls || ''}" data-k="${k}">${label}</button>`).join('');
  $('modal-actions').onclick = (e) => {
    const k = e.target.closest('[data-k]')?.dataset.k;
    if (k !== undefined) actions[Number(k)][2]();
  };
  $('modal').hidden = false;
}

function closeModal() {
  $('modal').hidden = true;
  basketOpen = false;
}

// ----- the basket, opened big -----

let basketOpen = false;

function basketBody() {
  const b = basket;
  if (!b.items.length) return '<p class="note">The basket is empty. Right-click anything, or press B, to add it.</p>';
  const sum = b.count
    ? `<b>${plural(b.count, 'item')} · ${human(b.bytes)}</b> will be deleted when you confirm.`
    : 'Nothing here can be deleted.';
  const care = b.danger ? ` ${plural(b.danger, 'item')} ${b.danger === 1 ? 'needs' : 'need'} extra care (see the red notes).` : '';
  const item = (it) => `<li class="lvl-${it.level}">
      <div class="b-head"><span class="b-dot"></span><button class="link b-name" data-show="${it.id}" title="Show in the tree">${esc(it.name)}</button>
        <span class="b-size">${human(it.size)}</span><button class="b-x" data-remove="${it.id}" title="Take out of the basket" aria-label="Take ${esc(it.name)} out of the basket">×</button></div>
      <div class="b-path">${esc(it.path)}</div>
      ${it.reason ? `<div class="b-reason">${esc(it.reason)}</div>` : ''}
    </li>`;
  const more = b.items.length - BASKET_BIG;
  return `<p class="b-sum">${sum}${care}</p>
    <ul class="confirm-list big">${[...b.items].sort((x, y) => y.size - x.size).slice(0, BASKET_BIG).map(item).join('')}</ul>
    ${more > 0 ? `<p class="note">…and ${fmtInt(more)} smaller items.</p>` : ''}`;
}

/** The basket in a big dialog: every item with its path and notes, largest first. */
function openBasket() {
  modal('Cleanup basket', basketBody(), [
    ['Empty basket', '', async () => setBasket(await invoke('basket_clear'))],
    ['Close', '', closeModal],
    ['Review and delete…', 'danger-btn', openDeleteDialog],
  ]);
  basketOpen = true;
  $('modal').querySelector('.modal-card').classList.add('wide');
  $('modal-actions').children[2].disabled = basket.count === 0;
  $('modal-body').onclick = (e) => {
    const rm = e.target.closest('[data-remove]')?.dataset.remove;
    if (rm !== undefined) return basketToggle(Number(rm));
    const show = e.target.closest('[data-show]')?.dataset.show;
    if (show === undefined) return;
    closeModal();
    setView('tree');
    revealInTree(Number(show)).then(() => $('scroller').focus());
  };
}

function openDeleteDialog() {
  const b = basket;
  const go = b.items.filter((it) => it.level !== 'blocked');
  const kept = b.items.filter((it) => it.level === 'blocked');
  if (go.length === 0) return;
  const item = (it) => `<li class="lvl-${it.level}">
      <div class="b-head"><span class="b-dot"></span><span class="b-name">${esc(it.name)}</span><span class="b-size">${human(it.size)}</span></div>
      <div class="b-path">${esc(it.path)}</div>
      ${it.reason ? `<div class="b-reason">${esc(it.reason)}</div>` : ''}
    </li>`;
  const body = `
    <ul class="confirm-list">${go.slice(0, BASKET_BIG).map(item).join('')}</ul>
    ${go.length > BASKET_BIG ? `<p class="note">…and ${fmtInt(go.length - BASKET_BIG)} more.</p>` : ''}
    ${kept.length ? `<p class="note">Won't be deleted: ${kept.map((it) => esc(it.name)).join(', ')}.</p>` : ''}
    <fieldset class="method">
      <label><input type="radio" name="method" value="recycle" checked> <span>Move to the Recycle Bin
        <span class="note">(recommended: you can restore them; the space comes back when the bin is emptied)</span></span></label>
      <label><input type="radio" name="method" value="permanent"> <span>Delete permanently
        <span class="note">(can't be undone)</span></span></label>
    </fieldset>
    <label class="ack" id="ack-perm-row" hidden><input type="checkbox" id="ack-perm"> I understand these can't be recovered.</label>
    ${b.danger ? '<label class="ack danger"><input type="checkbox" id="ack-danger"> I understand some of these are system, program or OneDrive files.</label>' : ''}`;
  modal(`Delete ${plural(b.count, 'item')} (${human(b.bytes)})?`, body, [
    ['Cancel', '', closeModal],
    ['Move to Recycle Bin', 'danger-btn', startDelete],
  ]);
  const update = () => {
    const permanent = document.querySelector('input[name=method]:checked').value === 'permanent';
    $('ack-perm-row').hidden = !permanent;
    const ok = (!permanent || $('ack-perm').checked) && (!b.danger || $('ack-danger').checked);
    const goBtn = $('modal-actions').children[1];
    goBtn.textContent = permanent ? 'Delete permanently' : 'Move to Recycle Bin';
    goBtn.disabled = !ok;
  };
  $('modal-body').onchange = update;
  update();
}

async function startDelete() {
  const permanent = document.querySelector('input[name=method]:checked').value === 'permanent';
  const confirmedDanger = !!$('ack-danger')?.checked;
  try {
    await invoke('delete_basket', { permanent, confirmedDanger });
  } catch (e) {
    modal('Nothing was deleted', `<p>${esc(String(e))}</p>`, [['Close', '', closeModal]]);
    return;
  }
  setScanning(true);
  modal(permanent ? 'Deleting…' : 'Moving to the Recycle Bin…', '<p id="del-progress">Starting…</p>', [
    ['Stop after this item', '', () => invoke('cancel_scan')],
  ]);
}

listen('delete-progress', (e) => {
  const p = e.payload;
  const el = $('del-progress');
  if (el) el.textContent = `${p.done + 1} of ${p.total}: ${p.name}`;
});

listen('delete-done', async (e) => {
  const d = e.payload;
  setScanning(false);
  const verb = d.recycled ? 'Moved' : 'Deleted';
  const where = d.recycled ? ' to the Recycle Bin. Empty it to get the space back.' : '.';
  let body = `<p>${verb} ${plural(d.deleted, 'item')} (${human(d.bytes)})${d.deleted ? where : '.'}</p>`;
  if (d.cancelled) body += '<p class="note">Stopped before the end; the rest is still in the basket.</p>';
  if (d.failed.length) {
    body += `<p>${plural(d.failed.length, 'item')} couldn't be deleted and stayed in the basket:</p>
      <ul class="fail-list">${d.failed.map((f) => `<li>${esc(f.path)}: ${esc(f.error)}</li>`).join('')}</ul>`;
  }
  modal(d.failed.length ? 'Done, with problems' : 'Done', body, [['Close', 'primary', closeModal]]);
  await refreshAfterDelete();
});

/** Reloads totals, tree, basket, search and treemap after a delete. */
async function refreshAfterDelete() {
  useTree('tree');
  summary = await invoke('summary');
  cats = new Map(summary.categories.map((c) => [c.id, c]));
  fileTypes = null;
  renderSide();
  const kept = expanded;
  const [selId, oldIndex] = [visible[selected]?.row.id, selected];
  cache = new Map();
  for (const id of kept) {
    try {
      const kids = await invoke('children', { id });
      sortRows(kids);
      cache.set(id, kids);
    } catch {
      // gone with its parent
    }
  }
  expanded = new Set([...kept].filter((id) => cache.has(id)));
  visible = [{ row: summary.root, depth: 0, parentSize: summary.root.size }];
  rebuild();
  // Keep the selection; if it's gone, select what took its place.
  const at = visible.findIndex((v) => v.row.id === selId);
  selected = at >= 0 ? at : Math.min(oldIndex, visible.length - 1);
  const shown = new Set(visible.map((v) => v.row.id));
  marked = new Set([...marked].filter((id) => shown.has(id)));
  if (!marked.size) marked.add(visible[selected].row.id);
  map.details = new Map();
  setBasket(await invoke('basket_view'));
  if (search) await applyFilters();
  else await layoutMap();
  if (view === 'dupes') renderDupes();
  if (view === 'types') renderTypes();
  if (view === 'tree' || shownTree === 'groups') select(selected);
}

// ---------- overview, export, start screen ----------

async function renderOverview() {
  const ov = $('overview');
  let o;
  try {
    o = await invoke('overview');
  } catch (e) {
    ov.textContent = String(e);
    return;
  }
  if (view !== 'overview') return;
  const total = summary.root.size;
  const item = (d) => `<li data-id="${d.row.id}" title="${esc(d.path)}">
      <span class="o-name">${esc(d.row.name)}</span><span class="o-size">${human(d.row.size)}</span>
      <span class="o-path">${esc(d.path)}</span></li>`;
  const top = [...summary.categories].filter((c) => c.files > 0).sort((a, b) => b.size - a.size).slice(0, 6);
  const catRows = top.map((c) => {
    const pct = total > 0 ? (c.size * 100) / total : 0;
    return `<li data-cat="${c.id}" title="Show only ${esc(c.name)}">
      <span class="swatch" style="background:${colorOf(c)}"></span><span class="o-name">${esc(c.name)}</span>
      <span class="o-size">${human(c.size)}</span>
      <div class="cat-bar"><span style="width:${pct}%;background:${colorOf(c)}"></span></div></li>`;
  }).join('');
  // Bytes in "safe" categories, including what isn't suggested (system or OneDrive folders).
  const safeTotal = summary.categories.filter((c) => c.cleanup === 'safe').reduce((t, c) => t + c.size, 0);
  const safe = o.safeCount
    ? `<div class="ov-big">${human(o.safeBytes)}</div>
       <p class="note">in ${plural(o.safeCount, 'place')} in categories that are usually safe to clean, such as caches and temporary files. System and OneDrive folders are left out.</p>
       <ul class="ov-list">${o.safeItems.map(item).join('')}</ul>
       <button class="primary" id="ov-add-safe">${o.safeCount === 1 ? 'Add it' : `Add all ${fmtInt(o.safeCount)}`} to the basket for review</button>`
    : safeTotal > 0
      ? `<p class="note">${human(safeTotal)} is in categories that are usually safe to clean, but all of it is in system or OneDrive folders, so none is suggested here. Windows' Disk Cleanup is the safer way to clear system caches.</p>`
      : '<p class="note">Nothing here is in a category that is usually safe to clean.</p>';
  ov.innerHTML = `<div class="ov-grid">
      <section class="ov-card"><h3>Where the space goes</h3><ul class="ov-list ov-cats">${catRows}</ul></section>
      <section class="ov-card"><h3>Usually safe to clean</h3>${safe}</section>
      <section class="ov-card wide"><h3>Largest files</h3>
        <ul class="ov-list">${o.largestFiles.map(item).join('') || '<li>No files.</li>'}</ul></section>
      <section class="ov-card wide" id="ov-clean"><h3>Windows cleanup</h3><p class="note">Checking…</p></section>
    </div>`;
  renderCleanup();
}

// ---------- file types ----------

let fileTypes = null; // totals per extension, loaded when the view first opens
let typesShown = 200;

async function renderTypes() {
  const el = $('types');
  if (!fileTypes) {
    el.innerHTML = '<p class="note">Adding up…</p>';
    try {
      fileTypes = await invoke('file_types');
    } catch (e) {
      el.textContent = String(e);
      return;
    }
  }
  if (view !== 'types') return;
  const total = fileTypes.reduce((t, x) => t + x.size, 0) || 1;
  const row = (t) => {
    const cat = cats.get(t.category);
    const color = colorOf(cat);
    const pct = (t.size * 100) / total;
    const name = t.ext ? `.${esc(t.ext)}` : '<i>no extension</i>';
    return `<li${t.ext ? ` data-ext="${esc(t.ext)}" title="Show only .${esc(t.ext)} files"` : ''}>
      <span class="swatch" style="background:${color}"></span><span class="t-ext">${name}</span>
      <span class="t-cat">${cat ? esc(cat.name) : ''}</span><span class="t-files">${plural(t.files, 'file')}</span>
      <span class="t-size">${human(t.size)}</span>
      <div class="cat-bar"><span style="width:${pct}%;background:${color}"></span></div></li>`;
  };
  const more = fileTypes.length - typesShown;
  el.innerHTML = `<section class="ov-card wide"><h3>File types</h3>
    <p class="note">${plural(fileTypes.length, 'kind')} of file, largest first, coloured by the category most of their bytes are in. Click one to see only those files.</p>
    <ul class="ov-list types-list">${fileTypes.slice(0, typesShown).map(row).join('')}</ul>
    ${more > 0 ? `<button id="types-more">Show ${fmtInt(Math.min(1000, more))} more</button>` : ''}</section>`;
}

// ---------- Windows cleanup ----------

/** The Windows cleanup card in the overview. */
async function renderCleanup() {
  let s;
  try {
    s = await invoke('cleanup_status');
  } catch (e) {
    s = null;
  }
  const el = $('ov-clean');
  if (!el || !s) return;
  const items = [
    ['recycle', 'Recycle Bin', s.recycleBytes ? `${human(s.recycleBytes)} in ${plural(s.recycleItems, 'item')}` : 'Empty',
      'Empty the Recycle Bin', !s.recycleBytes,
      'Deleted files wait here until it is emptied. Windows asks before emptying it.'],
    ['hibernation', 'Hibernation file', s.hiberfil ? `${human(s.hiberfil)} (hiberfil.sys)` : 'Hibernation is off',
      'Turn off hibernation', !s.hiberfil,
      'Windows keeps a file about as big as part of your memory for hibernating. Turning hibernation off removes it and also turns off Fast startup. Needs administrator rights; “powercfg /hibernate on” turns it back on.'],
    ['components', 'Windows components (WinSxS)', s.winsxsSize ? human(s.winsxsSize) : `Scan ${s.systemDrive}\\ to see its size`,
      'Clean up components', false,
      'Removes old versions of Windows components that updates replaced, with Windows’ own DISM tool. Takes a few minutes in a window of its own. Needs administrator rights.'],
  ];
  if (s.windowsOld) {
    items.push(['windows-old', 'Previous Windows installation', s.windowsOldSize ? human(s.windowsOldSize) : 'Windows.old',
      'Open storage settings', false,
      'Kept after a Windows upgrade so you can go back. Remove it in Windows’ storage settings, under Temporary files: “Previous Windows installation(s)”.']);
  }
  el.innerHTML = `<h3>Windows cleanup</h3>
    <p class="note">Often the biggest wins. Windows does these itself, so nothing breaks; never delete these files by hand.</p>
    <ul class="clean-list">${items.map(([k, name, size, label, off, why]) => `<li>
      <div class="c-head"><span class="o-name">${name}</span><span class="o-size">${size}</span>
        <button data-clean="${k}"${off ? ' disabled' : ''}>${label}</button></div>
      <div class="note">${why}</div></li>`).join('')}</ul>`;
}

const CLEANUP_ASK = {
  hibernation: ['Turn off hibernation?', 'Windows deletes hiberfil.sys and Fast startup stops working. Windows asks for administrator rights next.', 'Turn off'],
  components: ['Clean up Windows components?', 'Windows’ DISM tool runs in a window of its own for a few minutes. Windows asks for administrator rights next.', 'Start'],
};

async function startCleanup(what) {
  const go = async () => {
    closeModal();
    let done;
    try {
      done = await invoke('run_cleanup', { what });
    } catch (e) {
      setStatus(String(e), true);
      return;
    }
    const said = {
      recycle: done ? 'Recycle Bin emptied. Rescan to see the space come back in the map.' : 'The Recycle Bin was not emptied.',
      hibernation: 'Turning hibernation off; hiberfil.sys goes away in a moment.',
      components: 'Windows’ component cleanup is running in its own window. Rescan when it has finished.',
      'windows-old': 'Opened Windows’ storage settings.',
    };
    setStatus(said[what] ?? '');
    renderCleanup();
    if (what === 'hibernation') setTimeout(renderCleanup, 3000);
  };
  const ask = CLEANUP_ASK[what];
  if (!ask) return go();
  modal(ask[0], `<p>${ask[1]}</p>`, [['Cancel', '', closeModal], [ask[2], 'primary', go]]);
}

/** What Export saves: the search results, or the selected folder (a file's folder). */
function exportTarget() {
  if (view === 'results' && search) return { search: true, id: 0, what: 'the search results' };
  let i = view === 'tree' ? Math.max(0, selected) : 0;
  if (i > 0 && visible[i].row.kind !== 'dir') i = parentIndex(i);
  const row = visible[Math.max(0, i)].row;
  return { search: false, id: row.id, what: `“${row.name}”` };
}

async function doExport(format, t) {
  try {
    const r = await invoke('export', { format, search: t.search, id: t.id });
    if (r) setStatus(`Saved ${plural(r.rows, 'row')} to ${r.path} in ${r.millis} ms.`);
  } catch (e) {
    setStatus(`Export failed: ${e}`, true);
  }
}

// Per-viewer settings that survive restarts. Storage may be unavailable.
const store = {
  get(k) {
    // Settings saved before the rename used the "diskmap." prefix.
    try { return localStorage.getItem(`bytescape.${k}`) ?? localStorage.getItem(`diskmap.${k}`); } catch { return null; }
  },
  set(k, v) {
    try { localStorage.setItem(`bytescape.${k}`, String(v)); } catch { /* not saved */ }
  },
};

/** Start screen: drives to scan and the last folder. */
// ---------- logo, splash, start screen, scanning ----------

// The app icon as SVG: a dark plate with five tiles, like a small treemap.
const LOGO_SVG = `<svg class="logo" viewBox="0 0 100 100" role="img" aria-label="Bytescape">
  <rect class="plate" x="0" y="0" width="100" height="100" rx="19.5" fill="#1e2028"/>
  <rect class="tile t1" x="11.3" y="11.3" width="43.2" height="77.4" rx="3.5" fill="#7c5ce6"/>
  <rect class="tile t2" x="58.5" y="11.3" width="30.2" height="40.8" rx="3.5" fill="#38aaa0"/>
  <rect class="tile t3" x="58.5" y="56" width="14" height="32.7" rx="3.5" fill="#e8a03c"/>
  <rect class="tile t4" x="76.3" y="56" width="12.4" height="14.8" rx="3.5" fill="#e25c78"/>
  <rect class="tile t5" x="76.3" y="74.7" width="12.4" height="14" rx="3.5" fill="#6ebe5a"/>
</svg>`;
for (const slot of document.querySelectorAll('.logo-slot')) slot.innerHTML = LOGO_SVG;

const DRIVE_ICON = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"><rect x="3" y="6" width="18" height="12" rx="2.5"/><circle cx="16.5" cy="12" r="1.2" fill="currentColor"/><path d="M6.5 12h5"/></svg>';
const FOLDER_ICON = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"><path d="M3 7.5A1.5 1.5 0 0 1 4.5 6H9l2 2h8.5A1.5 1.5 0 0 1 21 9.5v8A1.5 1.5 0 0 1 19.5 19h-15A1.5 1.5 0 0 1 3 17.5z"/></svg>';

/** Hides the splash after its animation (or at once on click). */
function endSplash() {
  const sp = $('splash');
  if (!sp || sp.classList.contains('done')) return;
  sp.classList.add('done');
  setTimeout(() => { sp.hidden = true; }, 400);
}
const reduceMotion = matchMedia('(prefers-reduced-motion: reduce)').matches;
setTimeout(endSplash, reduceMotion ? 300 : 1700);

// ---------- updates ----------

let update = null; // { version, current, notes, date } while one is available

/** Asks GitHub for a newer release. Quiet unless `manual`. */
async function checkUpdate(manual) {
  if (manual) setStatus('Checking for updates…');
  try {
    update = await invoke('check_update');
  } catch (e) {
    if (manual) setStatus(`Couldn't check for updates: ${e}`, true);
    return;
  }
  $('update').hidden = !update;
  if (update) {
    $('update').textContent = `Update to ${update.version}`;
    if (manual) {
      setStatus(`Bytescape ${update.version} is available.`);
      openUpdate();
    }
  } else if (manual) {
    setStatus('Bytescape is up to date.');
  }
}

function openUpdate() {
  if (!update) return;
  const when = update.date ? `, released ${esc(update.date)}` : '';
  modal(`Update to Bytescape ${update.version}?`, `
    <p>You have ${esc(update.current)}; version ${esc(update.version)} is out${when}.</p>
    ${update.notes ? `<div class="update-notes">${esc(update.notes)}</div>` : ''}
    <p class="note">Bytescape closes, the update installs with a small progress window, and Bytescape starts again. Your settings, scan history and category rules stay. The download is checked against Bytescape's update key before it runs.</p>`,
  [['Later', '', closeModal], ['Install and restart', 'primary', installUpdate]]);
}

async function installUpdate() {
  closeModal();
  setStatus('Downloading the update…');
  try {
    await invoke('install_update'); // on success the app closes for the installer
  } catch (e) {
    setStatus(`The update didn't install: ${e}`, true);
  }
}

listen('update-progress', (e) => {
  const { done, total } = e.payload;
  setStatus(total ? `Downloading the update… ${Math.round((done * 100) / total)}%` : `Downloading the update… ${human(done)}`);
});
$('update').addEventListener('click', openUpdate);
$('check-updates').addEventListener('click', () => checkUpdate(true));
let appVersion = '';
invoke('app_version').then((v) => { appVersion = v; $('app-version').textContent = v; }).catch(() => {});
// A while after starting, so the first scan isn't slowed down.
if (store.get('autoUpdate') !== 'false') setTimeout(() => checkUpdate(false), 4000);

// ---------- settings and about ----------

const excludeList = () => (store.get('exclude') ?? '').split('\n').map((s) => s.trim()).filter(Boolean);

function applyTheme(theme) {
  const root = document.documentElement;
  if (theme === 'light' || theme === 'dark') root.dataset.theme = theme;
  else delete root.dataset.theme;
  if (map.n) {
    drawMap();
    drawOverlay();
  }
}

async function openSettings() {
  const theme = store.get('theme') ?? 'system';
  const [shell, rules] = await Promise.all([
    invoke('shell_menu_state').catch(() => false),
    invoke('rules_info').catch(() => null),
  ]);
  const radio = (v, label) => `<label><input type="radio" name="theme" value="${v}"${theme === v ? ' checked' : ''}> ${label}</label>`;
  modal('Settings', `
    <section class="set"><h3>Appearance</h3>
      ${radio('system', 'Same as Windows')}${radio('light', 'Light')}${radio('dark', 'Dark')}</section>
    <section class="set"><h3>Folders to leave out</h3>
      <p class="note">One per line. A name, like <code>node_modules</code>, leaves out every folder called that; a full path, like <code>D:\\Backups</code>, just that folder. They show in the list but aren't read. Applies from the next scan.</p>
      <textarea id="set-exclude" rows="4" spellcheck="false">${esc(excludeList().join('\n'))}</textarea></section>
    <section class="set"><h3>Search results</h3>
      <label><input type="checkbox" id="set-group"${grouped ? ' checked' : ''}> Group by folder (otherwise one list, largest first)</label></section>
    <section class="set"><h3>Explorer</h3>
      <label><input type="checkbox" id="set-shell"${shell ? ' checked' : ''}> Show “Scan with Bytescape” when right-clicking a folder or drive</label></section>
    <section class="set"><h3>Updates</h3>
      <div class="row-of"><label><input type="checkbox" id="set-auto"${store.get('autoUpdate') !== 'false' ? ' checked' : ''}> Check when Bytescape starts</label>
        <button id="set-check">Check now</button></div></section>
    <section class="set"><h3>Category rules</h3>
      <p class="note">Your own rules ${rules ? `go in <code>${esc(rules[0])}</code>` : 'file'}. They add to the built-in ones.</p>
      <div class="row-of"><button id="set-rules-open">${rules?.[1] ? 'Open rules file' : 'Create rules file'}</button>
        <button id="set-rules-reload"${summary ? '' : ' disabled'}>Reload rules</button><span class="note" id="set-rules-said"></span></div></section>`,
  [['About', '', openAbout], ['Close', 'primary', closeModal]]);
  $('modal').querySelector('.modal-card').classList.add('wide');
  $('modal-body').onchange = async (e) => {
    const t = e.target;
    if (t.name === 'theme') {
      store.set('theme', t.value);
      applyTheme(t.value);
    } else if (t.id === 'set-exclude') {
      store.set('exclude', t.value);
      invoke('set_exclude', { exclude: excludeList() });
    } else if (t.id === 'set-group') {
      $('f-group').checked = t.checked;
      $('f-group').dispatchEvent(new Event('change'));
    } else if (t.id === 'set-shell') {
      try {
        t.checked = await invoke('set_shell_menu', { on: t.checked });
      } catch (err) {
        setStatus(String(err), true);
      }
    } else if (t.id === 'set-auto') {
      store.set('autoUpdate', t.checked);
    }
  };
  $('modal-body').onclick = async (e) => {
    const id = e.target.closest('button')?.id;
    if (id === 'set-check') {
      closeModal();
      checkUpdate(true);
    } else if (id === 'set-rules-open') {
      invoke('open_rules').catch((err) => setStatus(String(err), true));
    } else if (id === 'set-rules-reload') {
      let problem;
      try {
        problem = await invoke('reload_rules');
      } catch (err) {
        $('set-rules-said').textContent = String(err);
        return;
      }
      $('set-rules-said').textContent = problem ? `Problem in the file, built-in rules used: ${problem}` : 'Reloaded.';
      await refreshAfterDelete();
    }
  };
}

function openAbout() {
  const link = (path, label) => `<button class="link" data-link="${path}">${label}</button>`;
  modal('About Bytescape', `
    <div class="about-head"><div class="logo-slot"></div><div><b>Bytescape ${esc(appVersion)}</b>
      <p class="note">A disk usage analyzer for Windows.</p></div></div>
    <p>© 2026 Raul Alves. Free and open source under the MIT or Apache 2.0 licence, at your choice.</p>
    <div class="about-links">
      ${link('', 'Project page')}
      ${link('/releases', 'Releases and what’s new')}
      ${link('/issues', 'Report a problem or ask for a feature')}
      ${link('/blob/main/docs/THIRD_PARTY_LICENSES.md', 'Licences of the libraries it uses')}
    </div>`,
  [['Check for updates', '', () => { closeModal(); checkUpdate(true); }], ['Close', 'primary', closeModal]]);
  for (const slot of $('modal-body').querySelectorAll('.logo-slot')) slot.innerHTML = LOGO_SVG;
  $('modal-body').onclick = (e) => {
    const path = e.target.closest('[data-link]')?.dataset.link;
    if (path === undefined) return;
    invoke('open_link', { url: `https://github.com/raullopesalves/bytescape${path}` }).catch((err) => setStatus(String(err), true));
  };
}

applyTheme(store.get('theme'));
$('splash').addEventListener('click', endSplash);

/** Recently scanned folders, newest first: [{ path, at }]. */
function recentList() {
  try {
    const list = JSON.parse(store.get('recent') || '[]');
    if (Array.isArray(list) && list.length) return list;
  } catch {
    // ignore a damaged list
  }
  const last = store.get('lastPath');
  return last ? [{ path: last, at: 0 }] : [];
}

function rememberRecent(path) {
  const key = path.toLowerCase();
  const list = [{ path, at: Math.floor(Date.now() / 1000) }]
    .concat(recentList().filter((r) => r.path.toLowerCase() !== key))
    .slice(0, 6);
  store.set('recent', JSON.stringify(list));
}

/** Shows the start screen: drives, standard folders, recent scans. */
async function showStart() {
  $('scanning').hidden = true;
  $('main').hidden = true;
  $('statusbar').hidden = true;
  $('start').hidden = false;
  $('start-back').hidden = !summary;
  const [drives, places] = await Promise.all([
    invoke('drives').catch(() => []),
    invoke('places').catch(() => []),
  ]);
  const local = drives.filter((d) => d.kind !== 'network');
  if (local.length > 1) {
    // Every local drive at once, under one root.
    const total = local.reduce((t, d) => t + d.total, 0);
    const free = local.reduce((t, d) => t + d.free, 0);
    drives.push({ path: ALL_DRIVES, label: 'All drives', total, free, kind: 'all' });
  }
  $('drives').innerHTML = drives.map((d) => {
    const used = d.total ? ((d.total - d.free) * 100) / d.total : 0;
    const name = d.kind === 'all' ? 'All drives'
      : d.label ? `${esc(d.label)} (${esc(d.path.replace(/\\$/, ''))})` : esc(d.path);
    const kind = { network: 'Network', removable: 'Removable', all: `${local.length} drives` }[d.kind] ?? '';
    return `<button class="drive" data-path="${esc(d.path)}">
        <span class="d-name">${DRIVE_ICON}${name}${kind ? `<span class="d-kind">${kind}</span>` : ''}</span>
        <span class="d-bar"><span class="${used > 90 ? 'full' : ''}" style="width:${used}%"></span></span>
        <span class="d-free">${human(d.free)} free of ${human(d.total)}</span>
      </button>`;
  }).join('') || '<p class="note">No drives found. Browse for a folder instead.</p>';
  $('places').innerHTML = places.map((p) => `<button class="place" data-path="${esc(p.path)}" title="${esc(p.path)}">${FOLDER_ICON}${esc(p.name)}</button>`).join('');
  const recent = recentList();
  $('recent-section').hidden = !recent.length;
  $('recent').innerHTML = recent.map((r) => `<button data-path="${esc(r.path)}" title="Scan ${esc(pathLabel(r.path))} again">
      <span class="r-path">${esc(pathLabel(r.path))}</span><span class="r-when">${r.at ? when(r.at) : ''}</span></button>`).join('');
}

function backToResults() {
  if (!summary) return;
  $('start').hidden = true;
  $('main').hidden = false;
  $('statusbar').hidden = false;
  renderSoon();
  layoutMap();
}

/** The scanning screen, reset for a new scan of `path`. */
function showScanning(path) {
  $('start').hidden = true;
  $('main').hidden = true;
  $('statusbar').hidden = true;
  $('scanning').hidden = false;
  $('scan-path').textContent = pathLabel(path);
  $('scan-stage').hidden = true;
  $('ss-files').textContent = '0';
  $('ss-dirs').textContent = '0';
  $('ss-bytes').textContent = '0 B';
  $('ss-time').textContent = 'Starting…';
}

// ---------- scan history ----------

let histCompare = null; // id of the past scan being compared with

const signed = (n) => `${n > 0 ? '+' : n < 0 ? '−' : '±'}${human(Math.abs(n))}`;
const when = (t) => new Date(t * 1000).toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' });

async function renderHistory() {
  const el = $('history');
  let list;
  try {
    list = await invoke('history_list');
  } catch (e) {
    el.textContent = String(e);
    return;
  }
  if (view !== 'history') return;
  if (!list.available) {
    el.innerHTML = '<p class="note">Scan history is not available: its database could not be opened.</p>';
    return;
  }
  const now = summary.root.size;
  const past = list.scans.filter((s) => !s.current);
  if (past.length && !past.some((p) => p.id === histCompare)) histCompare = past[0].id;

  const scans = list.scans.map((s) => `<li data-scan="${s.id}" class="${s.current ? 'current' : ''}${s.id === histCompare ? ' on' : ''}">
      <span class="o-name">${when(s.taken)}${s.current ? ' · this scan' : ''}</span>
      <span class="o-size">${human(s.size)}</span>
      <span class="o-path">${plural(s.files, 'file')} · ${s.method === 'mft' ? 'file table' : 'folder by folder'}${s.current ? '' : ` · ${signed(now - s.size)} since`}</span>
    </li>`).join('');

  let compare = '<p class="note">This is the first scan of this folder in the history. Scan it again later to see what changed.</p>';
  if (past.length) {
    let c;
    try {
      c = await invoke('history_compare', { id: histCompare });
    } catch (e) {
      c = null;
      compare = `<p class="note">${esc(String(e))}</p>`;
    }
    if (view !== 'history') return;
    if (c) compare = comparisonHtml(c);
  }

  el.innerHTML = `<div class="ov-grid">
      <section class="ov-card"><h3>Scans of this folder</h3><ul class="ov-list hist-scans">${scans}</ul>
        <p class="note">History takes ${human(list.bytes)} for ${plural(list.totalScans, 'scan')} of all folders.
          <button class="link" id="hist-clear">Clear history</button></p></section>
      <section class="ov-card hist-compare">${compare}</section>
    </div>`;
}

function comparisonHtml(c) {
  const total = c.newTotal - c.oldTotal;
  const biggest = Math.max(1, ...c.changes.map((x) => Math.abs(x.new - x.old)));
  const rows = c.changes.map((x) => {
    const d = x.new - x.old;
    const cls = d >= 0 ? 'grew' : 'shrank';
    const tag = x.wasMissing
      ? `<span class="tag" title="New, or under ${human(c.threshold)} last time">new</span>`
      : x.node === null ? '<span class="tag">gone</span>' : '';
    return `<li class="${cls}"${x.node !== null ? ` data-node="${x.node}"` : ''} title="${esc(x.path)}">
        <span class="o-name">${esc(x.name)}${tag}</span><span class="o-size delta">${signed(d)}</span>
        <span class="o-path">${esc(x.path)} · ${human(x.old)} → ${human(x.new)}</span>
        <div class="cat-bar"><span style="width:${(Math.abs(d) * 100) / biggest}%"></span></div>
      </li>`;
  }).join('') || `<li class="current"><span class="o-name">No folder changed by ${human(c.threshold)} or more.</span></li>`;
  const cats = c.categories.map((x) => `<li class="${x.new >= x.old ? 'grew' : 'shrank'} current">
      <span class="o-name">${esc(x.name)}</span><span class="o-size delta">${signed(x.new - x.old)}</span>
      <span class="o-path">${human(x.old)} → ${human(x.new)}</span></li>`).join('');
  return `<h3>Since ${when(c.taken)}: <span class="${total >= 0 ? 'grew' : 'shrank'}">${signed(total)}</span></h3>
    <p class="note">${human(c.oldTotal)} then, ${human(c.newTotal)} now. Folders that changed most; changes under ${human(c.threshold)} aren't listed.</p>
    <ul class="ov-list hist-changes">${rows}</ul>
    ${cats ? `<h3>By category</h3><ul class="ov-list">${cats}</ul>` : ''}`;
}

// ---------- duplicate files ----------

let dupesRunning = false;
let dupesShown = 100;          // groups listed; "Show more" adds to it
let dupesKeep = new Map();     // group key -> id of the copy to keep
let dupesMin = 1048576;

const groupKey = (g) => g.files.map((f) => f.id).sort((a, b) => a - b).join(',');

/** The copy to keep: the user's choice, else the oldest (likely the original). */
function keptCopy(g) {
  const chosen = dupesKeep.get(groupKey(g));
  if (chosen !== undefined && g.files.some((f) => f.id === chosen)) return chosen;
  return [...g.files].sort((a, b) => a.modified - b.modified || a.id - b.id)[0].id;
}

async function renderDupes() {
  const el = $('dupes');
  if (dupesRunning) {
    if (!$('dupes-progress')) {
      el.innerHTML = `<div class="ov-grid"><section class="ov-card wide"><h3>Looking for duplicate files…</h3>
        <p id="dupes-progress" class="note">Starting…</p>
        <p class="note">Files are read in the background at low priority. The toolbar's Cancel stops the search.</p></section></div>`;
    }
    return;
  }
  let v;
  try {
    v = await invoke('dupes_view', { limit: dupesShown });
  } catch (e) {
    el.textContent = String(e);
    return;
  }
  if (view !== 'dupes') return;
  const sizes = [[102400, '100 KB'], [1048576, '1 MB'], [10485760, '10 MB'], [104857600, '100 MB']];
  const picker = `<label class="note">Files of at least <select id="dupes-min">${sizes
    .map(([b, t]) => `<option value="${b}"${b === dupesMin ? ' selected' : ''}>${t}</option>`).join('')}</select></label>`;
  if (!v.done) {
    el.innerHTML = `<div class="ov-grid"><section class="ov-card wide"><h3>Duplicate files</h3>
      <p class="note">Finds files with exactly the same contents. Only files that share a size are read, and mostly just
        their first and last 16 KB. Online-only files are skipped, and links to the same file don't count as copies.</p>
      <div class="dupes-start">${picker}<button class="primary" id="dupes-go">Find duplicates</button></div></section></div>`;
    return;
  }
  const pct = v.candidateBytes ? ((v.bytesRead * 100) / v.candidateBytes).toFixed(1) : '0';
  const skipped = v.changed + v.unreadable
    ? ` ${plural(v.changed + v.unreadable, 'file')} changed or couldn't be read and ${v.changed + v.unreadable === 1 ? 'was' : 'were'} left out.` : '';
  const groups = v.groups.map((g, gi) => {
    const keep = keptCopy(g);
    const rows = g.files.map((f) => `<label class="dup-file${f.id === keep ? ' keep' : ''}">
        <input type="radio" name="keep-${gi}" value="${f.id}" data-key="${groupKey(g)}"${f.id === keep ? ' checked' : ''}>
        <span class="o-name">${esc(f.name)}${basketIds.has(f.id) ? '<span class="tag basket">in basket</span>' : ''}</span>
        <span class="dup-meta">${fmtDate(f.modified)}${f.category ? ` · ${esc(f.category)}` : ''}</span>
        <button class="link dup-show" data-id="${f.id}" title="Show in the tree">${esc(f.path)}</button>
      </label>`).join('');
    return `<section class="dup-group" data-g="${gi}">
        <div class="dup-head"><span>${g.files.length} copies of ${human(g.size)} · ${human(g.wasted)} wasted</span>
          <button class="link" data-basket-group="${gi}">Put the other copies in the basket</button></div>${rows}</section>`;
  }).join('');
  el.innerHTML = `<div class="ov-grid"><section class="ov-card wide">
      <h3>${v.totalGroups ? `${human(v.totalWasted)} in ${plural(v.totalGroups, 'group')} of identical files` : 'No duplicate files found'}</h3>
      <p class="note">Checked ${plural(v.candidates, 'file')} of ${human(v.minSize)} or more (${human(v.candidateBytes)}),
        reading ${human(v.bytesRead)} of them (${pct}%) in ${(v.millis / 1000).toFixed(1)} s.${skipped}</p>
      <div class="dupes-start">${v.totalGroups ? '<button class="primary" id="dupes-basket-all">Put all extra copies in the basket</button>' : ''}
        ${picker}<button id="dupes-go">Search again</button></div>
      ${v.totalGroups ? '<p class="note">For each group the copy marked "keep" stays; the others go to the cleanup basket for review. Nothing is deleted here.</p>' : ''}
      <div class="dup-groups">${groups}</div>
      ${v.groups.length < v.totalGroups ? `<button id="dupes-more">Show more (${fmtInt(v.totalGroups - v.groups.length)} more groups)</button>` : ''}
    </section></div>`;
}

async function startDupes() {
  dupesMin = Number($('dupes-min')?.value || dupesMin);
  try {
    await invoke('dupes_start', { minSize: dupesMin });
  } catch (e) {
    setStatus(String(e), true);
    return;
  }
  dupesRunning = true;
  dupesShown = 100;
  setScanning(true);
  $('dupes').innerHTML = '';
  renderDupes();
}

/** Puts every copy except the kept one of each group in the basket. */
async function basketExtras(groups) {
  const ids = groups.flatMap((g) => {
    const keep = keptCopy(g);
    return g.files.map((f) => f.id).filter((id) => id !== keep);
  });
  if (!ids.length) return;
  setBasket(await invoke('basket_add', { ids }));
  setStatus(`The cleanup basket now has ${plural(basket.items.length, 'item')}. Review it before deleting anything.`);
  renderDupes();
}

listen('dupes-progress', (e) => {
  const p = e.payload;
  const el = $('dupes-progress');
  if (!el) return;
  const what = p.stage === 3 ? 'Comparing whole files' : 'Comparing the ends of files';
  el.textContent = `${what}: ${fmtInt(p.filesDone)} of ${fmtInt(p.filesTotal)} · ${human(p.bytesRead)} read · ${(p.elapsedMs / 1000).toFixed(0)} s`;
});

listen('dupes-done', (e) => {
  dupesRunning = false;
  setScanning(false);
  setStatus(e.payload.cancelled ? 'Duplicate search stopped.' : 'Duplicate search finished.');
  if (view === 'dupes') {
    $('dupes').innerHTML = '';
    renderDupes();
  }
});

// ---------- wiring ----------

$('pick').addEventListener('click', showStart);
$('start-back').addEventListener('click', backToResults);
$('scan-cancel').addEventListener('click', () => invoke('cancel_scan'));
$('start').addEventListener('click', (e) => {
  const path = e.target.closest('[data-path]')?.dataset.path;
  if (path) startScan(path);
});
$('pick-empty').addEventListener('click', pick);
$('path-form').addEventListener('submit', (e) => {
  e.preventDefault();
  const path = $('path-in').value.trim().replace(/^"(.*)"$/, '$1');
  if (path) startScan(path);
});
$('settings').addEventListener('click', openSettings);
$('start-settings').addEventListener('click', openSettings);
$('start-about').addEventListener('click', openAbout);
$('rescan').addEventListener('click', () => lastPath && startScan(lastPath));
$('cancel').addEventListener('click', () => invoke('cancel_scan'));
$('elevate').addEventListener('click', () => {
  setStatus('Waiting for Windows to allow administrator rights…');
  invoke('relaunch_elevated', { path: lastPath }).catch((e) => setStatus(String(e), true));
});
$('reveal').addEventListener('click', () => {
  const id = currentId();
  if (id !== undefined) invoke('reveal', { id }).catch((e) => setStatus(String(e), true));
});

$('thead').addEventListener('click', (e) => {
  if (e.target.closest('[data-grip]')) return;
  const key = e.target.closest('[data-key]')?.dataset.key;
  if (key && !flat() && (view === 'tree' || view === 'results')) setSort(key);
});

$('rows').addEventListener('click', (e) => {
  const el = e.target.closest('.row');
  if (!el) return;
  const i = Number(el.dataset.i);
  const how = selectHow(e, false);
  const crumb = e.target.closest('.crumb');
  if (crumb && how === 'one') return showFolderInTree(i, Number(crumb.dataset.k));
  if (flat()) return selectResult(i, how);
  if (how === 'one' && e.target.closest('.twisty')) toggle(i);
  select(i, how);
});
$('rows').addEventListener('dblclick', (e) => {
  const el = e.target.closest('.row');
  if (!el) return;
  const i = Number(el.dataset.i);
  if (flat()) showInTree(i);
  else if (shownTree === 'groups' && visible[i]?.row.kind !== 'dir') {
    const id = visible[i].row.id;
    setView('tree');
    revealInTree(id);
  } else if (!e.target.closest('.twisty')) toggle(i);
});

for (const id of ['f-name', 'f-ext']) $(id).addEventListener('input', applyFiltersSoon);
for (const id of ['f-min', 'f-max', 'f-age', 'f-cat', 'f-folders']) $(id).addEventListener('change', applyFilters);
$('f-name').addEventListener('keydown', (e) => {
  if (e.key === 'Enter') applyFilters();
  if (e.key === 'ArrowDown') { e.preventDefault(); $('scroller').focus(); if (flat()) selectResult(Math.max(0, search?.sel ?? 0)); }
});
$('f-clear').addEventListener('click', () => {
  resetFilters();
  applyFilters();
});
$('viewbar').addEventListener('click', (e) => {
  const v = e.target.closest('[data-view]')?.dataset.view;
  if (v) setView(v);
});
$('basket-items').addEventListener('click', (e) => {
  const rm = e.target.closest('[data-remove]')?.dataset.remove;
  if (rm !== undefined) {
    basketToggle(Number(rm));
    return;
  }
  const id = e.target.closest('li[data-id]')?.dataset.id;
  if (id !== undefined) {
    setView('tree');
    revealInTree(Number(id)).then(() => $('scroller').focus());
  }
});
$('basket-delete').addEventListener('click', openDeleteDialog);
$('basket-items').addEventListener('click', (e) => {
  if (e.target.closest('[data-open-basket]')) openBasket();
});
$('basket-open').addEventListener('click', openBasket);
$('basket-open').addEventListener('keydown', (e) => {
  if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); openBasket(); }
});
$('basket-total').addEventListener('click', openBasket);
$('basket-clear').addEventListener('click', async () => setBasket(await invoke('basket_clear')));

$('rows').addEventListener('contextmenu', (e) => {
  const el = e.target.closest('.row');
  if (!el) return;
  e.preventDefault();
  rowMenu(Number(el.dataset.i), e.clientX, e.clientY);
});
$('menu').addEventListener('keydown', (e) => {
  const items = [...$('menu').querySelectorAll('button')];
  const at = items.indexOf(document.activeElement);
  const go = { ArrowDown: at + 1, ArrowUp: at - 1, Home: 0, End: items.length - 1 }[e.key];
  if (go !== undefined) {
    e.preventDefault();
    items[(go + items.length) % items.length].focus();
  } else if (e.key === 'Tab') {
    e.preventDefault();
    closeMenu();
  }
});
document.addEventListener('mousedown', (e) => {
  if (!$('menu').contains(e.target)) closeMenu();
});
document.addEventListener('keydown', (e) => {
  if (e.key !== 'Escape') return;
  closeMenu();
  // Not while a delete is running (the toolbar's Cancel button shows then).
  if (!$('modal').hidden && $('cancel').hidden) closeModal();
});
// The browser's own menu is no use here, except in text fields.
document.addEventListener('contextmenu', (e) => {
  if (!e.target.closest('input')) e.preventDefault();
});

$('cats').addEventListener('click', (e) => {
  const li = e.target.closest('li[data-id]');
  if (!li) return;
  $('f-cat').value = $('f-cat').value === li.dataset.id ? '' : li.dataset.id;
  applyFilters();
});

$('scroller').addEventListener('scroll', () => {
  $('thead').scrollLeft = $('scroller').scrollLeft;
  renderSoon();
});
// Resizing columns.
$('thead').addEventListener('pointerdown', (e) => {
  const grip = e.target.closest('[data-grip]');
  if (!grip || e.button !== 0) return;
  e.preventDefault();
  const k = grip.dataset.grip;
  const start = grip.parentElement.getBoundingClientRect().width;
  const x0 = e.clientX;
  grip.setPointerCapture(e.pointerId);
  grip.classList.add('drag');
  const move = (ev) => {
    colWidths[k] = Math.max(MIN_COL, Math.round(start + ev.clientX - x0));
    applyColumns();
  };
  const done = () => {
    grip.removeEventListener('pointermove', move);
    grip.removeEventListener('pointerup', done);
    grip.removeEventListener('pointercancel', done);
    grip.classList.remove('drag');
    store.set('columns', JSON.stringify(colWidths));
  };
  grip.addEventListener('pointermove', move);
  grip.addEventListener('pointerup', done);
  grip.addEventListener('pointercancel', done);
});
$('thead').addEventListener('dblclick', (e) => {
  const k = e.target.closest('[data-grip]')?.dataset.grip;
  if (!k) return;
  delete colWidths[k];
  applyColumns();
  store.set('columns', JSON.stringify(colWidths));
});
try {
  const saved = JSON.parse(store.get('columns') ?? '{}');
  for (const k of Object.keys(COLS)) if (Number.isFinite(saved[k])) colWidths[k] = Math.max(MIN_COL, saved[k]);
} catch { /* defaults */ }
grouped = store.get('grouped') !== 'false';
$('f-group').checked = grouped;
$('f-group').addEventListener('change', () => {
  grouped = $('f-group').checked;
  store.set('grouped', grouped);
  if (view === 'results') {
    $('scroller').scrollTop = 0;
    setView('results');
  }
});
renderHead();
$('scroller').addEventListener('keydown', onKey);
new ResizeObserver(renderSoon).observe($('scroller'));

const overlay = $('map-overlay');
overlay.addEventListener('mousemove', (e) => {
  const { i, cx, cy } = eventRect(e);
  if (i !== map.hover) {
    map.hover = i;
    drawOverlay();
  }
  showTip(i, cx, cy);
});
overlay.addEventListener('mouseleave', () => {
  map.hover = -1;
  drawOverlay();
  showTip(-1);
});
overlay.addEventListener('click', (e) => {
  const { i } = eventRect(e);
  if (i < 0) return;
  setView('tree');
  revealInTree(map.id[i]);
});
overlay.addEventListener('contextmenu', (e) => {
  const { i } = eventRect(e);
  if (i < 0) return;
  showTip(-1);
  openMenu(e, map.id[i], { showInTree: true, dir: map.kind[i] === K_DIR, zoomId: zoomTarget(i) });
});
overlay.addEventListener('dblclick', (e) => {
  const { i } = eventRect(e);
  const id = i >= 0 ? zoomTarget(i) : -1;
  if (id >= 0 && id !== map.root) {
    showTip(-1);
    zoomTo(id);
  }
});
$('crumbs').addEventListener('click', (e) => {
  const id = e.target.closest('button[data-id]')?.dataset.id;
  if (id !== undefined) zoomTo(Number(id));
});

let relayout = 0;
new ResizeObserver(() => {
  clearTimeout(relayout);
  relayout = setTimeout(layoutMap, 120);
}).observe($('map-wrap'));
matchMedia('(prefers-color-scheme: dark)').addEventListener('change', () => {
  drawMap();
  drawOverlay();
});
// Moving to a screen with other scaling changes devicePixelRatio but often not
// the map's size in CSS pixels, so the resize observer doesn't see it. A
// once-a-second check is simple and dependable (media query change events
// were not, after the first change).
setInterval(() => {
  if (summary && map.dpr && window.devicePixelRatio !== map.dpr) layoutMap();
}, 1000);

// Drag the splitter to share the height between the tree and the treemap.
$('splitter').addEventListener('pointerdown', (e) => {
  const split = e.currentTarget;
  split.setPointerCapture(e.pointerId);
  split.classList.add('dragging');
  const right = $('right').getBoundingClientRect();
  const move = (ev) => {
    const h = Math.min(right.height - 160, Math.max(120, right.bottom - ev.clientY));
    $('map-pane').style.height = `${h}px`;
  };
  const up = () => {
    store.set('mapHeight', parseInt($('map-pane').style.height, 10));
    split.classList.remove('dragging');
    split.removeEventListener('pointermove', move);
    split.removeEventListener('pointerup', up);
  };
  split.addEventListener('pointermove', move);
  split.addEventListener('pointerup', up);
});

$('types').addEventListener('click', (e) => {
  if (e.target.closest('#types-more')) {
    typesShown += 1000;
    renderTypes();
    return;
  }
  const ext = e.target.closest('[data-ext]')?.dataset.ext;
  if (ext === undefined) return;
  resetFilters();
  $('f-ext').value = ext;
  applyFilters();
});
$('overview').addEventListener('click', async (e) => {
  const clean = e.target.closest('[data-clean]')?.dataset.clean;
  if (clean) return startCleanup(clean);
  if (e.target.closest('#ov-add-safe')) {
    setBasket(await invoke('basket_add_suggested'));
    setStatus(`The cleanup basket now has ${plural(basket.items.length, 'item')}. Review it before deleting anything.`);
    return;
  }
  const cat = e.target.closest('[data-cat]')?.dataset.cat;
  if (cat !== undefined) {
    $('f-cat').value = cat;
    applyFilters();
    return;
  }
  const id = e.target.closest('[data-id]')?.dataset.id;
  if (id !== undefined) {
    setView('tree');
    await revealInTree(Number(id));
    $('scroller').focus();
  }
});
$('export').addEventListener('click', (e) => {
  const t = exportTarget();
  const r = e.currentTarget.getBoundingClientRect();
  openMenuAt(r.left, r.bottom + 4, [
    [`Export ${t.what} as CSV…`, () => doExport('csv', t)],
    [`Export ${t.what} as JSON…`, () => doExport('json', t)],
  ]);
});
$('history').addEventListener('click', (e) => {
  if (e.target.closest('#hist-clear')) {
    modal('Clear scan history?', '<p>This deletes the saved snapshots of every folder. Your files are not touched.</p>', [
      ['Cancel', '', closeModal],
      ['Clear history', 'danger-btn', async () => {
        closeModal();
        await invoke('history_clear');
        histCompare = null;
        renderHistory();
      }],
    ]);
    return;
  }
  const scan = e.target.closest('li[data-scan]:not(.current)')?.dataset.scan;
  if (scan !== undefined) {
    histCompare = Number(scan);
    renderHistory();
    return;
  }
  const node = e.target.closest('li[data-node]')?.dataset.node;
  if (node !== undefined) {
    setView('tree');
    revealInTree(Number(node)).then(() => $('scroller').focus());
  }
});
listen('history-saved', () => {
  if (view === 'history') renderHistory();
});
$('dupes').addEventListener('click', async (e) => {
  if (e.target.closest('#dupes-go')) return startDupes();
  if (e.target.closest('#dupes-more')) {
    dupesShown += 100;
    return renderDupes();
  }
  if (e.target.closest('#dupes-basket-all')) {
    const all = await invoke('dupes_view', { limit: 1e9 });
    return basketExtras(all.groups);
  }
  const gi = e.target.closest('[data-basket-group]')?.dataset.basketGroup;
  if (gi !== undefined) {
    const v = await invoke('dupes_view', { limit: dupesShown });
    return basketExtras([v.groups[Number(gi)]]);
  }
  const show = e.target.closest('.dup-show');
  if (show) {
    e.preventDefault();
    setView('tree');
    revealInTree(Number(show.dataset.id)).then(() => $('scroller').focus());
  }
});
$('dupes').addEventListener('change', (e) => {
  const r = e.target.closest('input[type=radio][data-key]');
  if (!r) return;
  dupesKeep.set(r.dataset.key, Number(r.value));
  renderDupes();
});

const savedMap = Number(store.get('mapHeight'));
if (savedMap >= 120) $('map-pane').style.height = `${Math.min(savedMap, window.innerHeight - 320)}px`;

markSortHeader();
// Settings the scanner needs, before the first scan starts.
invoke('set_exclude', { exclude: excludeList() }).catch(() => {});
invoke('initial_path').then((p) => {
  if (p) startScan(p);
  else showStart();
});
