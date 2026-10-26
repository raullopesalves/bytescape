'use strict';

// Event wiring and startup.

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
$('basket-add').addEventListener('click', basketToggleSelection);
$('nav-drives').addEventListener('click', (e) => {
  const path = e.target.closest('[data-path]')?.dataset.path;
  if (path) startScan(path);
});
$('address').addEventListener('click', (e) => {
  const id = e.target.closest('button[data-id]')?.dataset.id;
  if (id === undefined) return;
  setView('tree');
  revealInTree(Number(id)).then(() => $('scroller').focus());
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

// Drag the splitter to share the width between the list and the details pane.
$('splitter').addEventListener('pointerdown', (e) => {
  const split = e.currentTarget;
  split.setPointerCapture(e.pointerId);
  split.classList.add('dragging');
  const main = $('main').getBoundingClientRect();
  const move = (ev) => {
    const w = Math.min(main.width - 520, Math.max(240, main.right - ev.clientX));
    $('details').style.width = `${w}px`;
  };
  const up = () => {
    store.set('detailsWidth', parseInt($('details').style.width, 10));
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

const savedDetails = Number(store.get('detailsWidth'));
if (savedDetails >= 240) $('details').style.width = `${Math.min(savedDetails, window.innerWidth - 600)}px`;

markSortHeader();
// Settings the scanner needs, before the first scan starts.
invoke('set_exclude', { exclude: excludeList() }).catch(() => {});
invoke('initial_path').then((p) => {
  if (p) startScan(p);
  else showStart();
});
