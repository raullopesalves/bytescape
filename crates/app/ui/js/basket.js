'use strict';

// The cleanup basket, the right-click menu, rescanning a folder, deleting.

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
