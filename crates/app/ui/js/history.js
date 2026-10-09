'use strict';

// Scan history and duplicate files.

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
