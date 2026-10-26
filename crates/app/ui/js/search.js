'use strict';

// Search and filters, flat and grouped results.

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
  showAddress(it.row.id);
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
