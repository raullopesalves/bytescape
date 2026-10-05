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
  map.details = new Map();
  zoomTo(root.id);
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
  drawOverlay();
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
    map.kind[i] = dv.getUint8(o + 23);
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

function drawMap() {
  const ctx = $('map').getContext('2d', { alpha: false });
  const frames = [cssVar('--map-frame-a'), cssVar('--map-frame-b')];
  const rest = cssVar('--map-rest');
  const other = cssVar('--map-other');
  ctx.fillStyle = cssVar('--map-bg');
  ctx.fillRect(0, 0, ctx.canvas.width, ctx.canvas.height);

  for (let i = 0; i < map.n; i++) {
    const x = map.x[i], y = map.y[i], w = map.w[i], h = map.h[i];
    const kind = map.kind[i];
    if (kind === K_DIR && !isCollapsed(i)) {
      ctx.fillStyle = frames[map.depth[i] % 2];
      ctx.fillRect(x, y, w, h);
      continue;
    }
    // Leaves: files, folders too small to open, and "smaller items" blocks.
    // Leave a 1 px seam on the right and bottom so neighbours stay apart.
    const gw = w >= 3 ? w - 1 : w, gh = h >= 3 ? h - 1 : h;
    const cat = kind <= K_REST ? cats.get(map.cat[i]) : undefined;
    ctx.fillStyle = cat ? colorOf(cat) : other;
    ctx.fillRect(x, y, gw, gh);
    if (kind === K_REST) {
      ctx.fillStyle = rest;
      ctx.fillRect(x, y, gw, gh);
    } else if (w > 8 && h > 8) {
      // Soft light from the top left gives each block some depth.
      const g = ctx.createLinearGradient(x, y, x + gw, y + gh);
      g.addColorStop(0, 'rgba(255,255,255,0.22)');
      g.addColorStop(0.5, 'rgba(255,255,255,0)');
      g.addColorStop(1, 'rgba(0,0,0,0.22)');
      ctx.fillStyle = g;
      ctx.fillRect(x, y, gw, gh);
    }
  }
}

/** Rectangle of the tree's selected node, or of its nearest drawn ancestor. */
function selectedRect() {
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
  if (i >= 0) revealInTree(map.id[i]);
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
    split.classList.remove('dragging');
    split.removeEventListener('pointermove', move);
    split.removeEventListener('pointerup', up);
  };
  split.addEventListener('pointermove', move);
  split.addEventListener('pointerup', up);
});

markSortHeader();
invoke('initial_path').then((p) => p && startScan(p));
