'use strict';

// The folder tree: rows, selection, keys, columns.

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
  name: { label: 'Name', w: 150, flex: 1, sort: 'name' },
  folder: { label: 'Folder', w: 160, flex: 1.3 },
  size: { label: 'Size', w: 84, num: true, sort: 'size' },
  pct: { label: '% of parent', w: 100 },
  alloc: { label: 'On disk', w: 84, num: true, sort: 'alloc' },
  files: { label: 'Files', w: 76, num: true, sort: 'files' },
  modified: { label: 'Modified', w: 92, num: true, sort: 'modified' },
  category: { label: 'Category', w: 120, sort: 'category' },
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
    <div class="name-cell" style="padding-left:${4 + depth * 16}px"><span class="twisty">${twisty}</span>${riskHtml(r)}${iconOf(r)}<span class="label" title="${esc(r.name)}">${esc(r.name)}</span>${hits}${tagHtml}</div>
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
  showSelected(visible[i].row, visible[i].parentSize);
  const risk = visible[i].row.risk;
  try {
    const path = await invoke('node_path', { id });
    if (visible[selected]?.row.id === id) {
      $('selpath').textContent = risk ? `${path}  ·  ${risk.reason}` : path;
      $('reveal').hidden = false;
      showAddress(id);
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

/** The address bar: the selected item's folders, each a link into the tree. */
async function showAddress(id) {
  syncBasketButton();
  const chain = await invoke('ancestors', { id }).catch(() => []);
  if (currentId() !== id) return;
  $('address').innerHTML = chain.map(([aid, name], k) => (k === chain.length - 1
    ? `<span class="here" title="${esc(name)}">${esc(name)}</span>`
    : `<button data-id="${aid}" title="${esc(name)}">${esc(name)}</button><span class="sep">›</span>`)).join('');
}

/** The details pane: what the selected item is, and its main actions. */
function showSelected(r, parentSize) {
  const pct = parentSize > 0 ? (r.size * 100) / parentSize : 0;
  const cat = cats.get(r.category);
  const facts = [
    `${pct.toFixed(1)}% of ${flat() ? 'everything scanned' : 'its folder'}`,
    r.kind === 'dir' ? plural(r.files, 'file') : '',
    `${human(r.alloc)} on disk`,
    r.modified ? `modified ${fmtDate(r.modified)}` : '',
  ].filter(Boolean).join(' · ');
  $('selinfo').innerHTML = `<div class="si-name" title="${esc(r.name)}">${iconOf(r)}<span>${esc(r.name)}</span></div>
    <div class="si-size">${human(r.size)}</div>
    <div class="si-facts">${facts}</div>
    ${cat ? `<div class="si-cat"><span class="swatch" style="background:${colorOf(cat)}"></span>${esc(cat.name)}</div>` : ''}
    <div class="si-acts"><button data-act="open">${r.kind === 'dir' ? 'Open in Explorer' : 'Open'}</button><button data-act="properties">Properties</button></div>`;
}

/** Status bar and details pane for a selection of several rows. */
function showMany(n, bytes) {
  const what = `${fmtInt(n)} items selected`;
  $('selpath').textContent = `${what}${bytes === undefined ? '' : ` · ${human(bytes)}`}`;
  $('reveal').hidden = true;
  $('selinfo').innerHTML = `<div class="si-name"><span>${what}</span></div>${bytes === undefined ? '' : `<div class="si-size">${human(bytes)}</div>`}`;
  syncBasketButton();
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
