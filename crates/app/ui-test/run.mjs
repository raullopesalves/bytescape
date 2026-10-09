// UI tests: opens ui-test/index.html (the real UI with mock.js standing in
// for the Rust side) in headless Edge and checks the main flows.
// usage: node crates/app/ui-test/run.mjs   (EDGE=path\to\msedge.exe to override)
// Needs Node 22+ (WebSocket) and Microsoft Edge. Exits 1 if a check fails.
import { spawn } from 'node:child_process';
import { existsSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const EDGE = process.env.EDGE ?? [
  'C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe',
  'C:\\Program Files\\Microsoft\\Edge\\Application\\msedge.exe',
].find(existsSync);
if (!EDGE) {
  console.error('Microsoft Edge not found; set EDGE.');
  process.exit(1);
}
const PORT = 9300 + Math.floor(Math.random() * 500);
const page = pathToFileURL(fileURLToPath(new URL('./index.html', import.meta.url))).href;
const profile = mkdtempSync(join(tmpdir(), 'bytescape-ui-'));
const edge = spawn(EDGE, [
  '--headless=new', `--remote-debugging-port=${PORT}`, '--allow-file-access-from-files',
  '--window-size=1280,820', `--user-data-dir=${profile}`, '--no-first-run', page,
], { stdio: 'ignore' });

const wait = (ms) => new Promise((r) => setTimeout(r, ms));
let ws;
for (let k = 0; k < 50 && !ws; k++) {
  await wait(200);
  try {
    const targets = await (await fetch(`http://127.0.0.1:${PORT}/json`)).json();
    const t = targets.find((x) => x.type === 'page' && x.url.startsWith('file'));
    if (t) ws = new WebSocket(t.webSocketDebuggerUrl);
  } catch { /* not up yet */ }
}
if (!ws) {
  console.error('Edge did not start.');
  edge.kill();
  process.exit(1);
}
await new Promise((r) => ws.addEventListener('open', r, { once: true }));
let n = 0;
const pending = new Map();
ws.addEventListener('message', (e) => {
  const m = JSON.parse(e.data);
  pending.get(m.id)?.(m);
  pending.delete(m.id);
});
const send = (method, params = {}) => new Promise((r) => {
  const id = ++n;
  pending.set(id, r);
  ws.send(JSON.stringify({ id, method, params }));
});
/** Runs `body` as an async function in the page and returns its value. */
async function js(body) {
  const r = await send('Runtime.evaluate', { expression: `(async () => { ${body} })()`, awaitPromise: true, returnByValue: true });
  if (r.result?.exceptionDetails) throw new Error(r.result.exceptionDetails.exception?.description ?? 'page error');
  return r.result?.result?.value;
}
async function load(query = '') {
  await js(`localStorage.clear(); location.href = ${JSON.stringify(page + query)}`).catch(() => {});
  await wait(400);
  for (let k = 0; k < 40; k++) {
    if (await js('return !!(window.summary || (typeof summary !== "undefined" && summary))').catch(() => false)) break;
    await wait(150);
  }
  await wait(2000); // splash
}
async function mouse(x, y, { button = 'left', modifiers = 0 } = {}) {
  for (const type of ['mousePressed', 'mouseReleased']) {
    await send('Input.dispatchMouseEvent', { type, x, y, button, clickCount: 1, modifiers });
  }
  await wait(80);
}
async function clickRow(i, modifiers = 0, button = 'left') {
  const [x, y] = await js(`const b = document.querySelector('#rows .row[data-i="${i}"] .label').getBoundingClientRect(); return [b.left + 4, b.top + 8]`);
  await mouse(x, y, { button, modifiers });
}
const CTRL = 2, SHIFT = 8;
const rowNames = () => js(`return [...document.querySelectorAll('#rows .row')].map(e => e.querySelector('.label')?.textContent)`);
const selNames = () => js(`return [...document.querySelectorAll('#rows .row.sel .label')].map(e => e.textContent)`);

let failed = 0;
function check(name, got, want) {
  const ok = JSON.stringify(got) === JSON.stringify(want);
  if (!ok) failed++;
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${name}${ok ? '' : `\n     got  ${JSON.stringify(got)}\n     want ${JSON.stringify(want)}`}`);
}
async function test(name, fn) {
  try {
    await fn();
  } catch (e) {
    failed++;
    console.log(`FAIL ${name}: ${e.message}`);
  }
}

await load();

await test('tree', async () => {
  await js(`setView('tree'); for (let k = 0; k < 3; k++) for (let i = visible.length - 1; i > 0; i--) await expand(i)`);
  check('tree rows', await rowNames(), ['X:\\demo', 'big.bin', 'b', 'c', 'deep.txt', 'b.txt', 'a', 'a2.txt', 'a1.txt', 'a3.txt', 'z']);
  check('flagged rows', await js(`return [...document.querySelectorAll('#rows .row')].filter(r => r.querySelector('.risk')).map(r => r.querySelector('.label').textContent)`), ['c', 'deep.txt']);
});

await test('multi-select and the basket', async () => {
  await clickRow(1);
  await clickRow(3, CTRL);
  await clickRow(5, SHIFT);
  check('ctrl, then shift', await selNames(), ['c', 'deep.txt', 'b.txt']);
  await js(`await basketToggleSelection()`);
  check('B adds them', await js(`return basket.items.map(b => b.name)`), ['c', 'b.txt']);
  await js(`await basketToggleSelection()`);
  check('B again takes them out', await js(`return basket.items.length`), 0);
  await js(`$('scroller').focus()`);
  await send('Input.dispatchKeyEvent', { type: 'rawKeyDown', key: 'a', code: 'KeyA', windowsVirtualKeyCode: 65, modifiers: CTRL });
  await wait(100);
  check('ctrl+A', (await selNames()).length, 11);
});

await test('right-click menu', async () => {
  await clickRow(2);
  await clickRow(2, 0, 'right');
  check('folder menu', await js(`return [...$('menu').querySelectorAll('button')].map(b => b.textContent)`),
    ['Add to cleanup basket', 'Open in Explorer', 'Show in Explorer', 'Open in Terminal', 'Copy path', 'Properties', 'Rescan this folder', 'Zoom treemap here']);
  await js(`closeMenu()`);
});

await test('rescan a folder', async () => {
  await js(`await select(visible.findIndex(v => v.row.name === 'b.txt')); $('scroller').dispatchEvent(new KeyboardEvent('keydown', { key: 'F5', bubbles: true }))`);
  await wait(400);
  check('status', await js(`return $('status').textContent`), 'Rescanned X:\\demo\\b: 500 B (±0 B since the scan).');
  check('selection kept', await js(`return visible[selected].row.name`), 'b.txt');
});

await test('grouped results', async () => {
  await js(`document.querySelector('#cats li[data-id="1"]').click()`);
  await wait(400);
  check('Documents, grouped', await js(`return [...document.querySelectorAll('#rows .row .name-cell')].map(e => e.innerText.replace(/\\s+/g, ' ').trim())`), ['▾ X:\\demo (3)', '▸ a (3)']);
  await js(`await select(1); await basketToggleSelection()`);
  check('B on a folder takes its matches', await js(`return basket.items.map(b => b.name).sort()`), ['a1.txt', 'a2.txt', 'a3.txt']);
  await js(`setBasket(await invoke('basket_clear'))`);
});

await test('flat results and folder links', async () => {
  await js(`resetFilters(); $('f-name').value = 'txt'; $('f-group').checked = false; $('f-group').dispatchEvent(new Event('change')); await applyFilters()`);
  await wait(400);
  check('header', await js(`return [...$('thead').children].map(d => d.textContent)`), ['Name', 'Folder', 'Size', '% of all', 'On disk', 'Files', 'Modified', 'Category']);
  await js(`document.querySelector('#rows .row[data-i="0"] .crumb[data-k="2"]').click()`);
  await wait(400);
  check('folder link opens the tree', await js(`return [view, visible[selected].row.name]`), ['tree', 'c']);
  await js(`$('f-group').checked = true; $('f-group').dispatchEvent(new Event('change')); resetFilters(); await applyFilters()`);
});

await test('resizing a column', async () => {
  await js(`setView('tree')`);
  const [x, y] = await js(`const r = document.querySelector('[data-grip=size]').getBoundingClientRect(); return [r.left + 3, r.top + 10]`);
  await send('Input.dispatchMouseEvent', { type: 'mousePressed', x, y, button: 'left', clickCount: 1 });
  for (let dx = 10; dx <= 50; dx += 10) await send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: x + dx, y, button: 'left', buttons: 1 });
  await send('Input.dispatchMouseEvent', { type: 'mouseReleased', x: x + 50, y, button: 'left', clickCount: 1 });
  await wait(100);
  check('width saved', await js(`return JSON.parse(localStorage.getItem('bytescape.columns')).size`), 134);
  check('sort unchanged', await js(`return sort.key`), 'size');
});

await test('file types', async () => {
  await js(`setView('types')`);
  await wait(300);
  check('rows', await js(`return [...document.querySelectorAll('.types-list .t-ext')].map(e => e.textContent)`), ['.bin', '.txt', 'no extension']);
  await js(`document.querySelector('.types-list li[data-ext="txt"]').click()`);
  await wait(400);
  check('filters', await js(`return [view, $('f-ext').value]`), ['results', 'txt']);
  await js(`resetFilters(); await applyFilters()`);
});

await test('basket views', async () => {
  await js(`setBasket(await invoke('basket_add', { ids: [3, 10, 11] })); $('basket-open').click()`);
  check('big view', await js(`return [$('modal-title').textContent, [...$('modal-body').querySelectorAll('.b-name')].map(b => b.textContent)]`), ['Cleanup basket', ['big.bin', 'a2.txt', 'a1.txt']]);
  await js(`$('modal-body').querySelector('[data-remove="11"]').click()`);
  await wait(150);
  check('removed in place', await js(`return [...$('modal-body').querySelectorAll('.b-name')].map(b => b.textContent)`), ['big.bin', 'a1.txt']);
  await js(`closeModal(); setBasket(await invoke('basket_clear'))`);
});

await test('windows cleanup asks first', async () => {
  await js(`setView('overview')`);
  await wait(400);
  await js(`document.querySelector('[data-clean="hibernation"]').click()`);
  check('asks', await js(`return [$('modal').hidden, $('modal-title').textContent, mockCleanups.length]`), [false, 'Turn off hibernation?', 0]);
  await js(`closeModal()`);
  check('cancel runs nothing', await js(`return mockCleanups.length`), 0);
});

await test('settings', async () => {
  await js(`$('settings').click()`);
  await wait(200);
  await js(`const d = document.querySelector('input[name=theme][value=dark]'); d.checked = true; d.dispatchEvent(new Event('change', { bubbles: true }))`);
  check('dark theme', await js(`return getComputedStyle(document.body).backgroundColor`), 'rgb(23, 24, 28)');
  await js(`const t = $('set-exclude'); t.value = 'node_modules\\n  \\nD:/Backups'; t.dispatchEvent(new Event('change', { bubbles: true }))`);
  check('folders to leave out', await js(`return mockExclude`), ['node_modules', 'D:/Backups']);
  await js(`closeModal()`);
});

await test('shortcuts and about', async () => {
  await send('Input.dispatchKeyEvent', { type: 'rawKeyDown', key: 'F1', code: 'F1', windowsVirtualKeyCode: 112 });
  await wait(150);
  check('F1 lists the shortcuts', await js(`return [$('modal-title').textContent, document.querySelectorAll('table.keys tr').length > 15]`), ['Keyboard shortcuts', true]);
  await js(`closeModal(); openAbout(); $('about-log').click()`);
  check('about shows the log', await js(`return mockActions.includes('open_log')`), true);
  await js(`closeModal()`);
});

await test('window errors go to the log', async () => {
  await js(`setTimeout(() => { throw new Error('test error'); }, 0)`);
  await wait(200);
  check('logged', await js(`return mockLog.some((m) => m.includes('test error'))`), true);
});

await test('start screen', async () => {
  await js(`await showStart()`);
  check('drives', await js(`return [...document.querySelectorAll('#drives .drive')].map(b => b.dataset.path)`), ['C:\\', 'D:\\', 'Z:\\', '::all-drives']);
});

await load('?update');
await test('update button', async () => {
  await js(`await checkUpdate(false)`);
  check('shown', await js(`return [$('update').hidden, $('update').textContent]`), [false, 'Update to 0.2.0']);
});

ws.close();
edge.kill();
await wait(300);
try { rmSync(profile, { recursive: true, force: true }); } catch { /* Edge may still hold it */ }
console.log(failed ? `\n${failed} check(s) failed` : '\nall UI checks passed');
process.exit(failed ? 1 : 0);
