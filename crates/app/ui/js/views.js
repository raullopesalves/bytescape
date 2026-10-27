'use strict';

// Overview, export, file types, Windows cleanup, start and scanning screens.

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
      <section class="ov-card totals"><h3>This scan</h3>${totalsHtml()}</section>
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
