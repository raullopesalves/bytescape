'use strict';

// The treemap: layout requests, cushion drawing, hover and clicks.

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
    minArea: 40 * dpr * dpr, // merge items under ~40 square CSS pixels into "smaller items"
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
  loadLabels();
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
  // Open folders are frames behind their contents; below two levels the
  // cushions show the nesting, and more frames are only noise.
  const deepest = map.depth[0] + 2;
  for (let i = 0; i < map.n; i++) {
    if (map.kind[i] === K_DIR && !isCollapsed(i) && map.depth[i] <= deepest) {
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
  drawLabels(ctx, dpr);
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

// Names and sizes on the map's first-level blocks, where they fit.
const LABEL = { w: 72, h: 40 }; // CSS pixels a block needs for its label

function labelRects() {
  const out = [];
  const want = map.depth[0] + 1;
  for (let i = 1; i < map.n; i++) {
    if (map.depth[i] === want && map.kind[i] !== K_REST && map.w[i] >= LABEL.w * map.dpr && map.h[i] >= LABEL.h * map.dpr) out.push(i);
  }
  return out;
}

/** Fetches the names the labels need, then draws them. */
async function loadLabels() {
  const seq = map.seq;
  await Promise.all(labelRects().map((i) => details(map.id[i]).catch(() => null)));
  if (seq === map.seq) drawOverlay();
}

/** `text`, cut short with an ellipsis to fit `width`. */
function fitText(ctx, text, width) {
  if (ctx.measureText(text).width <= width) return text;
  let n = text.length;
  while (n > 1 && ctx.measureText(`${text.slice(0, n)}…`).width > width) n--;
  return `${text.slice(0, n)}…`;
}

function drawLabels(ctx, dpr) {
  const font = '"Segoe UI Variable Text", "Segoe UI", system-ui, sans-serif';
  ctx.textBaseline = 'alphabetic';
  for (const i of labelRects()) {
    const d = map.details.get(map.id[i]);
    if (!d) continue;
    const pad = 6 * dpr;
    const room = map.w[i] - 4 * dpr - 2 * pad;
    ctx.font = `600 ${12 * dpr}px ${font}`;
    const name = fitText(ctx, d.row.name, room);
    const nw = ctx.measureText(name).width;
    ctx.font = `${11 * dpr}px ${font}`;
    const size = human(d.row.size);
    const sw = ctx.measureText(size).width;
    const x = map.x[i] + 3 * dpr, y = map.y[i] + 3 * dpr;
    ctx.fillStyle = 'rgba(0, 0, 0, 0.55)';
    ctx.beginPath();
    ctx.roundRect(x, y, Math.max(nw, sw) + 2 * pad, 34 * dpr, 4 * dpr);
    ctx.fill();
    ctx.fillStyle = '#ffffff';
    ctx.font = `600 ${12 * dpr}px ${font}`;
    ctx.fillText(name, x + pad, y + 15 * dpr);
    ctx.fillStyle = 'rgba(255, 255, 255, 0.8)';
    ctx.font = `${11 * dpr}px ${font}`;
    ctx.fillText(size, x + pad, y + 28 * dpr);
  }
}

/** The block at the map's first level that holds rectangle i (ancestors come first). */
function topBlock(i) {
  const want = map.depth[0] + 1;
  for (let j = i; j > 0; j--) if (map.depth[j] === want) return j;
  return i;
}

/** The map shows the selected folder, or the folder a selected file is in. */
let followTimer = 0;
function followSelection(id) {
  clearTimeout(followTimer);
  // Waits a moment, so arrowing through the tree doesn't lay out every row.
  followTimer = setTimeout(() => {
    if (summary && id !== map.root) zoomTo(id);
  }, 150);
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
