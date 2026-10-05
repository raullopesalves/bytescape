'use strict';

// Tauri's JS API, exposed because tauri.conf.json sets withGlobalTauri.
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const ROW_H = 26; // must match .row height in style.css
const OVERSCAN = 10;

// Node flags, as in scanner::flags.
const FLAG = { DUP: 1, ONLINE: 4, COMPRESSED: 8, SPARSE: 16, READ_ERROR: 32 };

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
let selected = -1;         // index into `visible`
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

async function startScan(path) {
  try {
    await invoke('start_scan', { path });
  } catch (e) {
    setStatus(String(e), true);
    return;
  }
  lastPath = path;
  summary = null;
  $('main').hidden = true;
  $('statusbar').hidden = true;
  $('empty').hidden = false;
  $('empty').querySelector('h1').textContent = 'Scanning…';
  $('empty').querySelector('p').textContent = path;
  $('pick-empty').hidden = true;
  setScanning(true);
  setStatus(`Scanning ${path}…`);
}

function setScanning(on) {
  $('cancel').hidden = !on;
  $('pick').disabled = on;
  $('rescan').hidden = on || !lastPath;
}

listen('scan-progress', (e) => {
  const p = e.payload;
  setStatus(`Scanning… ${plural(p.files, 'file')} · ${plural(p.dirs, 'folder')} · ${human(p.bytes)} · ${(p.elapsedMs / 1000).toFixed(1)} s`);
});

listen('scan-done', async () => {
  setScanning(false);
  await loadResult();
});

listen('scan-error', (e) => {
  setScanning(false);
  const cancelled = e.payload === 'scan cancelled';
  setStatus(cancelled ? 'Scan cancelled.' : `Scan failed: ${e.payload}`, !cancelled);
  $('empty').querySelector('h1').textContent = 'See what takes up space';
  $('empty').querySelector('p').textContent = 'Pick a folder or a whole drive. diskmap scans it and groups everything by category.';
  $('pick-empty').hidden = false;
});

async function loadResult() {
  summary = await invoke('summary');
  cats = new Map(summary.categories.map((c) => [c.id, c]));
  cache = new Map();
  expanded = new Set();
  const root = summary.root;
  visible = [{ row: root, depth: 0, parentSize: root.size }];
  selected = 0;

  renderSide();
  $('empty').hidden = true;
  $('main').hidden = false;
  $('statusbar').hidden = false;
  $('scroller').scrollTop = 0;
  setStatus(`Scanned in ${(summary.scanMs / 1000).toFixed(1)} s, categorised in ${summary.categoriseMs} ms`);
  await expand(0);
  select(0);
  $('scroller').focus();
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
  $('totals').innerHTML = `
    <div class="root-name">${esc(r.name)}</div>
    <div class="big-size">${human(r.size)}</div>
    <dl>${rows.map(([k, v]) => `<dt>${k}</dt><dd>${v}</dd>`).join('')}</dl>`;

  const list = [...s.categories].filter((c) => c.files > 0).sort((a, b) => b.size - a.size);
  $('cats').innerHTML = list.map((c) => {
    const pct = r.size > 0 ? (c.size * 100) / r.size : 0;
    const color = colorOf(c);
    const safe = c.cleanup === 'safe' ? '<span class="safe">usually safe to clean</span>' : '';
    return `<li>
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
    kids = await invoke('children', { id: r.id });
    sortRows(kids);
    cache.set(r.id, kids);
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
  while (j < visible.length && visible[j].depth > v.depth) {
    expanded.delete(visible[j].row.id);
    j++;
  }
  expanded.delete(v.row.id);
  visible.splice(i + 1, j - i - 1);
  if (selected > i && selected < j) selected = i;
  else if (selected >= j) selected -= j - i - 1;
  render();
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
  if (i === selected) cls.push('sel');
  if (r.flags & FLAG.DUP) { cls.push('dup'); tags.push('hardlink, counted once'); }
  if (r.flags & FLAG.ONLINE) { cls.push('online'); tags.push('online-only'); }
  if (r.flags & FLAG.COMPRESSED) tags.push('compressed');
  if (r.flags & FLAG.READ_ERROR) { cls.push('readerr'); tags.push('partly unreadable'); }
  if (r.kind === 'link') tags.push('link, not followed');
  if (r.kind === 'error') tags.push('unreadable');

  const open = expanded.has(r.id);
  const twisty = r.kind === 'dir' && r.children > 0 ? (open ? '▾' : '▸') : '';
  const cat = cats.get(r.category);
  const catCell = cat
    ? `<span class="swatch" style="background:${colorOf(cat)}"></span>${esc(cat.name)}`
    : '';
  const tagHtml = tags.length ? `<span class="tag">${tags.join(', ')}</span>` : '';

  return `<div class="${cls.join(' ')}" data-i="${i}" role="treeitem" aria-level="${depth + 1}"${r.kind === 'dir' && r.children > 0 ? ` aria-expanded="${open}"` : ''}>
    <div class="name-cell" style="padding-left:${4 + depth * 16}px"><span class="twisty">${twisty}</span><span class="label" title="${esc(r.name)}">${esc(r.name)}</span>${tagHtml}</div>
    <div class="num">${human(r.size)}</div>
    <div class="pct"><div class="bar"><span style="width:${Math.min(100, pct)}%"></span></div><span class="val">${pct.toFixed(1)}%</span></div>
    <div class="num">${human(r.alloc)}</div>
    <div class="num">${r.kind === 'dir' ? fmtInt(r.files) : ''}</div>
    <div class="num">${fmtDate(r.modified)}</div>
    <div class="cat-cell">${catCell}</div>
  </div>`;
}

function render() {
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

async function select(i) {
  if (i < 0 || i >= visible.length) return;
  selected = i;
  ensureVisible(i);
  render();
  const id = visible[i].row.id;
  try {
    const path = await invoke('node_path', { id });
    if (visible[selected]?.row.id === id) {
      $('selpath').textContent = path;
      $('reveal').hidden = false;
    }
  } catch (e) {
    $('selpath').textContent = String(e);
  }
}

function parentIndex(i) {
  const d = visible[i].depth;
  for (let j = i - 1; j >= 0; j--) if (visible[j].depth < d) return j;
  return -1;
}

function onKey(e) {
  if (selected < 0) return;
  const page = Math.max(1, Math.floor($('scroller').clientHeight / ROW_H) - 1);
  const v = visible[selected];
  switch (e.key) {
    case 'ArrowDown': select(Math.min(visible.length - 1, selected + 1)); break;
    case 'ArrowUp': select(Math.max(0, selected - 1)); break;
    case 'PageDown': select(Math.min(visible.length - 1, selected + page)); break;
    case 'PageUp': select(Math.max(0, selected - page)); break;
    case 'Home': select(0); break;
    case 'End': select(visible.length - 1); break;
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
    default: return;
  }
  e.preventDefault();
}

// ---------- wiring ----------

$('pick').addEventListener('click', pick);
$('pick-empty').addEventListener('click', pick);
$('rescan').addEventListener('click', () => lastPath && startScan(lastPath));
$('cancel').addEventListener('click', () => invoke('cancel_scan'));
$('reveal').addEventListener('click', () => {
  const v = visible[selected];
  if (v) invoke('reveal', { id: v.row.id }).catch((e) => setStatus(String(e), true));
});

$('thead').addEventListener('click', (e) => {
  const key = e.target.closest('[data-key]')?.dataset.key;
  if (key) setSort(key);
});

$('rows').addEventListener('click', (e) => {
  const el = e.target.closest('.row');
  if (!el) return;
  const i = Number(el.dataset.i);
  if (e.target.closest('.twisty')) toggle(i);
  select(i);
});
$('rows').addEventListener('dblclick', (e) => {
  const el = e.target.closest('.row');
  if (el && !e.target.closest('.twisty')) toggle(Number(el.dataset.i));
});

$('scroller').addEventListener('scroll', renderSoon);
$('scroller').addEventListener('keydown', onKey);
window.addEventListener('resize', renderSoon);

markSortHeader();
invoke('initial_path').then((p) => p && startScan(p));
