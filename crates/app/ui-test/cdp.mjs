// Drives a page over the Chrome DevTools Protocol (WebView2 or headless Edge).
// See docs/HANDOFF.md ("Testing the UI"). Port: CDP_PORT (default 9222).
// usage: node cdp.mjs eval "<js>" | shot <file.png> | key <Key> [count] | click <x> <y> [double]
//        | send <Method> '<json params>'   (any protocol call, e.g. Emulation.setEmulatedMedia)
import { writeFileSync } from 'node:fs';
const targets = await (await fetch(`http://127.0.0.1:${process.env.CDP_PORT || 9222}/json`)).json();
const page = targets.find(t => t.type === 'page' && /^(file|http)/.test(t.url));
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise(r => ws.addEventListener('open', r, { once: true }));
let n = 0; const pending = new Map();
ws.addEventListener('message', e => { const m = JSON.parse(e.data); if (pending.has(m.id)) { pending.get(m.id)(m); pending.delete(m.id); } });
const send = (method, params = {}) => new Promise(r => { const id = ++n; pending.set(id, r); ws.send(JSON.stringify({ id, method, params })); });
const [cmd, a, b, c] = process.argv.slice(2);
if (cmd === 'eval') {
  const r = await send('Runtime.evaluate', { expression: a, awaitPromise: true, returnByValue: true });
  console.log(JSON.stringify(r.result?.result?.value ?? r.result, null, 1));
} else if (cmd === 'shot') {
  const r = await send('Page.captureScreenshot', { format: 'png' });
  writeFileSync(a, Buffer.from(r.result.data, 'base64')); console.log('saved', a);
} else if (cmd === 'key') {
  const codes = { ArrowDown: 40, ArrowUp: 38, ArrowLeft: 37, ArrowRight: 39, Enter: 13, End: 35, Home: 36, PageDown: 34 };
  for (let i = 0; i < Number(b || 1); i++) for (const type of ['keyDown', 'keyUp'])
    await send('Input.dispatchKeyEvent', { type, key: a, code: a, windowsVirtualKeyCode: codes[a] });
  console.log('ok');
} else if (cmd === 'send') {
  console.log(JSON.stringify((await send(a, b ? JSON.parse(b) : {})).result ?? {}));
} else if (cmd === 'click') {
  const clickCount = c ? 2 : 1;
  for (let k = 1; k <= clickCount; k++) for (const type of ['mousePressed', 'mouseReleased'])
    await send('Input.dispatchMouseEvent', { type, x: Number(a), y: Number(b), button: 'left', clickCount: k });
  console.log('ok');
}
ws.close();
