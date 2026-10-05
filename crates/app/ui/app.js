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
  store.set('lastPath', path);
  summary = null;
  $('main').hidden = true;
  $('statusbar').hidden = true;
  $('empty').hidden = false;
  $('empty').querySelector('h1').textContent = 'Scanning…';
  $('empty').querySelector('p').textContent = path;
  setStartIdle(false);
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
  setStartIdle(true);
  showStart();
});

async function loadResult() {
  summary = await invoke('summary');
  cats = new Map(summary.categories.map((c) => [c.id, c]));
  cache = new Map();
  expanded = new Set();
  const root = summary.root;
  visible = [{ row: root, depth: 0, parentSize: root.size }];
  selected = 0;
  search = null;
  basket = EMPTY_BASKET;
  basketIds = new Set();
  renderBasket();
  resetFilters();
  $('f-cat').innerHTML = '<option value="">All categories</option>' + [...summary.categories]
    .filter((c) => c.files > 0)
    .map((c) => `<option value="${c.id}">${esc(c.name)}</option>`).join('');
  $('f-summary').textContent = '';
  $('f-clear').hidden = true;
  view = 'tree';

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
  $('totals').innerHTML = `
    <div class="root-name">${esc(r.name)}</div>
    <div class="big-size">${human(r.size)}</div>
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
  const tagHtml = (tags.length ? `<span class="tag">${tags.join(', ')}</span>` : '')
    + (basketIds.has(r.id) ? '<span class="tag basket">in basket</span>' : '');

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
  if (view === 'results') return renderResults();
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
  if (keyMenu(e)) return;
  if (view === 'results') return onResultsKey(e);
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
    case 'b': case 'B': case 'Delete': basketToggle(v.row.id); break;
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

function drawMap() {
  const ctx = $('map').getContext('2d', { alpha: false });
  const frames = [cssVar('--map-frame-a'), cssVar('--map-frame-b')];
  const rest = cssVar('--map-rest');
  const dim = cssVar('--map-dim');
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
    if (map.dim[i]) {
      ctx.fillStyle = dim;
      ctx.fillRect(x, y, gw, gh);
    }
  }
}

/** Rectangle of the tree's selected node, or of its nearest drawn ancestor. */
function selectedRect() {
  if (view === 'results') return resultRect();
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
    if (r !== undefined && r > 0) outline(r, '#e0a030', 1.5);
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
    search = { generation: r.generation, count: r.count, bytes: r.bytes, items: [], pages: new Set(), sel: -1, anc: [] };
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
  const ov = v === 'overview';
  $('overview').hidden = !ov;
  $('thead').hidden = ov;
  $('scroller').hidden = ov;
  if (prev !== v && !ov) $('scroller').scrollTop = v === 'tree' ? treeScroll : 0;
  for (const b of $('viewbar').querySelectorAll('[data-view]')) {
    b.classList.toggle('on', b.dataset.view === v);
    b.setAttribute('aria-selected', String(b.dataset.view === v));
  }
  $('viewbar').querySelector('[data-view=results]').hidden = !search;
  $('thead').classList.toggle('results', v === 'results');
  $('thead').children[2].textContent = v === 'results' ? '% of all' : '% of parent';
  if (ov) renderOverview();
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
  const parent = it.path.slice(0, Math.max(0, it.path.length - r.name.length - 1));
  const pct = summary.root.size > 0 ? (r.size * 100) / summary.root.size : 0;
  const cat = cats.get(r.category);
  const cls = ['row', r.kind];
  if (i === search.sel) cls.push('sel');
  if (r.flags & FLAG.DUP) cls.push('dup');
  const catCell = cat ? `<span class="swatch" style="background:${colorOf(cat)}"></span>${esc(cat.name)}` : '';
  return `<div class="${cls.join(' ')}" data-i="${i}" role="option" title="${esc(it.path)}">
    <div class="name-cell" style="padding-left:8px"><span class="label">${esc(r.name)}</span><span class="tag">${esc(parent)}</span>${basketIds.has(r.id) ? '<span class="tag basket">in basket</span>' : ''}</div>
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

async function selectResult(i) {
  const s = search;
  if (!s || i < 0 || i >= s.count) return;
  s.sel = i;
  s.anc = [];
  ensureVisible(i);
  render();
  drawOverlay();
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
  switch (e.key) {
    case 'ArrowDown': selectResult(Math.min(s.count - 1, s.sel + 1)); break;
    case 'ArrowUp': selectResult(Math.max(0, at - 1)); break;
    case 'PageDown': selectResult(Math.min(s.count - 1, at + page)); break;
    case 'PageUp': selectResult(Math.max(0, at - page)); break;
    case 'Home': selectResult(0); break;
    case 'End': selectResult(s.count - 1); break;
    case 'Enter': showInTree(at); break;
    case 'b': case 'B': case 'Delete': basketToggle(currentId()); break;
    default: return;
  }
  e.preventDefault();
}

/** Id of the selected node in whichever list is showing. */
function currentId() {
  return view === 'results' ? search?.items[search.sel]?.row.id : visible[selected]?.row.id;
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

/** Adds the node to the basket, or takes it out if it's in. */
async function basketToggle(id) {
  if (id === undefined || !summary) return;
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
  else setStatus('Already in the basket, as part of a folder.');
}

function renderBasket() {
  const b = basket;
  $('basket-count').textContent = b.items.length ? `(${b.items.length})` : '';
  $('basket-empty').hidden = b.items.length > 0;
  $('basket-foot').hidden = b.items.length === 0;
  $('basket-items').innerHTML = b.items.map((it) => `<li class="lvl-${it.level}" data-id="${it.id}" title="${esc(it.path)}">
      <div class="b-head"><span class="b-dot"></span><span class="b-name">${esc(it.name)}</span>
        <span class="b-size">${human(it.size)}</span><button class="b-x" data-remove="${it.id}" title="Take out of the basket" aria-label="Take ${esc(it.name)} out of the basket">×</button></div>
      ${it.reason ? `<div class="b-reason">${esc(it.reason)}</div>` : ''}
    </li>`).join('');
  $('basket-total').textContent = b.count
    ? `${plural(b.count, 'item')} · ${human(b.bytes)}`
    : 'Nothing here can be deleted.';
  $('basket-delete').disabled = b.count === 0;
}

// Context menu on tree rows, results and treemap blocks.
let menuReturn = null; // where focus goes back to when the menu closes

/** Shows `actions` ([label, fn] pairs) as a menu at a screen point. */
function openMenuAt(x, y, actions) {
  const menu = $('menu');
  menuReturn = document.activeElement;
  menu.innerHTML = actions.map(([label], k) => `<button role="menuitem" data-k="${k}">${esc(label)}</button>`).join('');
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

function openMenu(e, id, { zoomId, showInTree } = {}) {
  e.preventDefault();
  if (id === undefined) return;
  const actions = [
    [basketIds.has(id) ? 'Take out of cleanup basket' : 'Add to cleanup basket', () => basketToggle(id)],
    ['Show in Explorer', () => invoke('reveal', { id }).catch((err) => setStatus(String(err), true))],
  ];
  if (showInTree) actions.push(['Show in tree', () => { setView('tree'); revealInTree(id); }]);
  if (zoomId !== undefined && zoomId >= 0) actions.push(['Zoom treemap here', () => zoomTo(zoomId)]);
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
  if (view === 'results') {
    selectResult(i);
    const r = search?.items[i]?.row;
    openMenu(at, r?.id, { showInTree: true, zoomId: r?.kind === 'dir' ? r.id : undefined });
  } else {
    select(i);
    const r = visible[i].row;
    openMenu(at, r.id, { zoomId: r.kind === 'dir' ? r.id : undefined });
  }
}

/** Menu key or Shift+F10 on the list: the menu under the selected row. */
function keyMenu(e) {
  if (!(e.key === 'ContextMenu' || (e.key === 'F10' && e.shiftKey))) return false;
  e.preventDefault();
  const i = view === 'results' ? search?.sel : selected;
  if (i === undefined || i < 0) return true;
  const el = document.querySelector(`#rows .row[data-i="${i}"]`);
  const r = el?.getBoundingClientRect();
  if (r) rowMenu(i, r.left + 40, r.bottom);
  return true;
}

// ----- deleting -----

function modal(title, body, actions) {
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
    <ul class="confirm-list">${go.map(item).join('')}</ul>
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
  summary = await invoke('summary');
  cats = new Map(summary.categories.map((c) => [c.id, c]));
  renderSide();
  const kept = expanded;
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
  map.details = new Map();
  setBasket(await invoke('basket_view'));
  if (search) await applyFilters();
  else await layoutMap();
  if (view === 'tree') select(selected);
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
  const safe = o.safeCount
    ? `<div class="ov-big">${human(o.safeBytes)}</div>
       <p class="note">in ${plural(o.safeCount, 'place')} in categories that are usually safe to clean, such as caches and temporary files. System and OneDrive folders are left out.</p>
       <ul class="ov-list">${o.safeItems.map(item).join('')}</ul>
       <button class="primary" id="ov-add-safe">${o.safeCount === 1 ? 'Add it' : `Add all ${fmtInt(o.safeCount)}`} to the basket for review</button>`
    : '<p class="note">Nothing here is in a category that is usually safe to clean.</p>';
  ov.innerHTML = `<div class="ov-grid">
      <section class="ov-card"><h3>Where the space goes</h3><ul class="ov-list ov-cats">${catRows}</ul></section>
      <section class="ov-card"><h3>Usually safe to clean</h3>${safe}</section>
      <section class="ov-card wide"><h3>Largest files</h3>
        <ul class="ov-list">${o.largestFiles.map(item).join('') || '<li>No files.</li>'}</ul></section>
    </div>`;
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
    try { return localStorage.getItem(`diskmap.${k}`); } catch { return null; }
  },
  set(k, v) {
    try { localStorage.setItem(`diskmap.${k}`, String(v)); } catch { /* not saved */ }
  },
};

/** Start screen: drives to scan and the last folder. */
async function showStart() {
  const last = store.get('lastPath');
  $('last-scan').hidden = !last;
  if (last) {
    $('last-scan').textContent = `Scan ${last} again`;
    $('last-scan').dataset.path = last;
  }
  let drives = [];
  try {
    drives = await invoke('drives');
  } catch {
    // no drive list; the folder picker still works
  }
  $('drives').innerHTML = drives.map((d) => {
    const pct = d.total ? ((d.total - d.free) * 100) / d.total : 0;
    const name = d.label ? `${esc(d.label)} (${esc(d.path.replace(/\\$/, ''))})` : esc(d.path);
    return `<button class="drive" data-path="${esc(d.path)}">
        <span class="d-top"><span>${name}</span><span class="d-free">${human(d.free)} free of ${human(d.total)}</span></span>
        <span class="cat-bar"><span style="width:${pct}%;background:var(--accent)"></span></span></button>`;
  }).join('');
}

function setStartIdle(idle) {
  $('pick-empty').hidden = !idle;
  $('drives').hidden = !idle;
  $('last-scan').hidden = !idle || !store.get('lastPath');
}

// ---------- wiring ----------

$('pick').addEventListener('click', pick);
$('pick-empty').addEventListener('click', pick);
$('rescan').addEventListener('click', () => lastPath && startScan(lastPath));
$('cancel').addEventListener('click', () => invoke('cancel_scan'));
$('reveal').addEventListener('click', () => {
  const id = currentId();
  if (id !== undefined) invoke('reveal', { id }).catch((e) => setStatus(String(e), true));
});

$('thead').addEventListener('click', (e) => {
  const key = e.target.closest('[data-key]')?.dataset.key;
  if (key && view === 'tree') setSort(key);
});

$('rows').addEventListener('click', (e) => {
  const el = e.target.closest('.row');
  if (!el) return;
  const i = Number(el.dataset.i);
  if (view === 'results') return selectResult(i);
  if (e.target.closest('.twisty')) toggle(i);
  select(i);
});
$('rows').addEventListener('dblclick', (e) => {
  const el = e.target.closest('.row');
  if (!el) return;
  if (view === 'results') showInTree(Number(el.dataset.i));
  else if (!e.target.closest('.twisty')) toggle(Number(el.dataset.i));
});

for (const id of ['f-name', 'f-ext']) $(id).addEventListener('input', applyFiltersSoon);
for (const id of ['f-min', 'f-max', 'f-age', 'f-cat', 'f-folders']) $(id).addEventListener('change', applyFilters);
$('f-name').addEventListener('keydown', (e) => {
  if (e.key === 'Enter') applyFilters();
  if (e.key === 'ArrowDown') { e.preventDefault(); $('scroller').focus(); if (view === 'results') selectResult(Math.max(0, search?.sel ?? 0)); }
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
  if (i < 0) return;
  setView('tree');
  revealInTree(map.id[i]);
});
overlay.addEventListener('contextmenu', (e) => {
  const { i } = eventRect(e);
  if (i < 0) return;
  showTip(-1);
  openMenu(e, map.id[i], { showInTree: true, zoomId: zoomTarget(i) });
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

$('overview').addEventListener('click', async (e) => {
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
$('drives').addEventListener('click', (e) => {
  const path = e.target.closest('[data-path]')?.dataset.path;
  if (path) startScan(path);
});
$('last-scan').addEventListener('click', (e) => startScan(e.currentTarget.dataset.path));

const savedMap = Number(store.get('mapHeight'));
if (savedMap >= 120) $('map-pane').style.height = `${Math.min(savedMap, window.innerHeight - 320)}px`;

markSortHeader();
invoke('initial_path').then((p) => {
  if (p) startScan(p);
  else showStart();
});
