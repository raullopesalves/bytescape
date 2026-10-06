// Stand-in for the Tauri API, so ui/ runs in a plain browser with a small fake
// tree. Used by ui-test/index.html; see docs/HANDOFF.md ("Testing the UI").
//
// Tree: X:\demo -> big.bin (1000 B, Caches), b/ (c/ -> deep.txt, b.txt),
//       a/ (Documents; a1.txt, a2.txt, a3.txt), z/ (empty)
const N = (id, parent, name, kind, size, children = 0, category = 0) => ({
  id, parent, name, kind, size, alloc: size, files: kind === 'file' ? 1 : 3,
  modified: 1790000000, category, flags: 0, children,
});
const nodes = [
  N(0, 0, 'X:\\demo', 'dir', 1800, 4),
  N(1, 0, 'a', 'dir', 300, 3, 1), N(2, 0, 'b', 'dir', 500, 2), N(3, 0, 'big.bin', 'file', 1000, 0, 2), N(4, 0, 'z', 'dir', 0),
  N(5, 2, 'c', 'dir', 400, 1),
  N(10, 1, 'a1.txt', 'file', 100, 0, 1), N(11, 1, 'a2.txt', 'file', 150, 0, 1), N(12, 1, 'a3.txt', 'file', 50, 0, 1),
  N(13, 2, 'b.txt', 'file', 100), N(14, 5, 'deep.txt', 'file', 400),
];
const byId = new Map(nodes.map((n) => [n.id, n]));
const kidsOf = (id) => nodes.filter((n) => n.parent === id && n.id !== id).sort((x, y) => x.name.localeCompare(y.name));
const categories = [
  { id: 1, key: 'documents', name: 'Documents', color: 'blue', cleanup: 'never', files: 3, size: 300, alloc: 300 },
  { id: 2, key: 'caches', name: 'Caches and temp', color: 'lime', cleanup: 'safe', files: 1, size: 1000, alloc: 1000 },
];
const handlers = {};
let dupesDone = false;
const mockHistory = [
  { id: 3, taken: 1791300000, size: 1800 },
  { id: 2, taken: 1790700000, size: 1500 },
  { id: 1, taken: 1789000000, size: 900 },
];

// Search: name substring, extensions, min size and category, like src/search.rs.
let searchState = null; // { generation, ids, marks }
let generation = 0;
function runSearch(f) {
  generation += 1;
  const empty = !f.name.trim() && f.extensions.length === 0 && f.minSize === null && f.maxSize === null
    && f.modifiedAfter === null && f.modifiedBefore === null && f.category === null;
  if (empty) { searchState = null; return { generation, count: 0, bytes: 0, millis: 0 }; }
  const exts = f.extensions.map((e) => '.' + e.replace(/^\./, '').toLowerCase());
  const hits = nodes.filter((n) => n.id !== 0
    && (n.kind === 'file' || (n.kind === 'dir' && f.folders && exts.length === 0))
    && (!f.name.trim() || n.name.toLowerCase().includes(f.name.trim().toLowerCase()))
    && (exts.length === 0 || exts.some((e) => n.name.toLowerCase().endsWith(e)))
    && (f.minSize === null || n.size >= f.minSize)
    && (f.category === null || n.category === f.category))
    .sort((a, b) => b.size - a.size);
  const marks = new Map(); // id -> 1 (match) | 2 (match below)
  for (const h of hits) {
    marks.set(h.id, (marks.get(h.id) ?? 0) | 1);
    for (let n = h; n.id !== 0;) { n = byId.get(n.parent); marks.set(n.id, (marks.get(n.id) ?? 0) | 2); }
  }
  searchState = { generation, ids: hits.map((h) => h.id), marks };
  const bytes = hits.filter((h) => h.kind === 'file').reduce((t, h) => t + h.size, 0);
  return { generation, count: hits.length, bytes, millis: 0 };
}

// Cleanup basket. Fake assessments: the root is blocked, a folder named "c"
// counts as a system folder, Documents is "never", Caches is "safe".
// Deleting "b.txt" fails, to show the error report.
let basketItems = [];
const below = (id, folder) => { for (let n = byId.get(id); n.id !== 0;) { n = byId.get(n.parent); if (n.id === folder) return true; } return false; };
function assess(n) {
  if (n.id === 0) return ['blocked', 'This is the folder you scanned. Pick what is inside it instead.'];
  if (n.name === 'c') return ['danger', 'Part of Windows or an installed program. Deleting it can break Windows or apps.'];
  if (n.category === 1) return ['warn', 'Documents, which is usually kept.'];
  if (n.category === 2) return ['safe', 'Usually safe to clean.'];
  return ['ok', null];
}
function basketView() {
  const v = { items: [], count: 0, bytes: 0, blocked: 0, danger: 0 };
  for (const id of basketItems) {
    const n = byId.get(id);
    const [level, reason] = assess(n);
    if (level === 'blocked') v.blocked += 1; else { v.count += 1; v.bytes += n.size; }
    if (level === 'danger') v.danger += 1;
    v.items.push({ id, name: n.name, path: chain(id).map(([, name]) => name).join('\\'), kind: n.kind, size: n.size, level, reason });
  }
  return v;
}
function removeNode(id) {
  const n = byId.get(id);
  const files = n.kind === 'file' ? 1 : n.files;
  for (let p = byId.get(n.parent); ; p = byId.get(p.parent)) {
    p.size -= n.size; p.alloc -= n.size; p.files -= files;
    if (p.id === 0) break;
  }
  byId.get(n.parent).children -= 1;
  const gone = new Set([id]);
  for (let changed = true; changed;) {
    changed = false;
    for (const m of nodes) if (m.id !== 0 && !gone.has(m.id) && gone.has(m.parent)) { gone.add(m.id); changed = true; }
  }
  for (let k = nodes.length - 1; k >= 0; k--) if (gone.has(nodes[k].id)) { byId.delete(nodes[k].id); nodes.splice(k, 1); }
}
async function deleteBasket({ permanent, confirmedDanger }) {
  const jobs = basketItems.filter((id) => assess(byId.get(id))[0] !== 'blocked');
  if (!confirmedDanger && jobs.some((id) => assess(byId.get(id))[0] === 'danger')) throw new Error('Some items are system, program or OneDrive files; confirm those first.');
  if (jobs.length === 0) throw new Error('Nothing in the basket can be deleted.');
  setTimeout(async () => {
    const done = { deleted: 0, bytes: 0, recycled: !permanent, failed: [], cancelled: false };
    for (const [k, id] of jobs.entries()) {
      const n = byId.get(id);
      handlers['delete-progress']?.({ payload: { done: k, total: jobs.length, name: n.name } });
      await new Promise((r) => setTimeout(r, 30));
      if (n.name === 'b.txt') { done.failed.push({ path: chain(id).map(([, x]) => x).join('\\'), error: "It's in use by another program." }); continue; }
      done.deleted += 1; done.bytes += n.size;
      removeNode(id);
      basketItems = basketItems.filter((b) => b !== id);
    }
    basketItems = basketItems.filter((b) => byId.has(b));
    searchState = null;
    handlers['delete-done']?.({ payload: done });
  }, 20);
  return null;
}

function chain(id) {
  const out = [];
  for (let n = byId.get(id); ; n = byId.get(n.parent)) {
    out.unshift([n.id, n.name]);
    if (n.id === 0) return out;
  }
}

// Slice-and-dice layout, packed like src/treemap.rs (24 bytes per rectangle).
function treemap({ id, width, height, pad }) {
  const recs = [];
  const place = (n, x, y, w, h, depth) => {
    recs.push([x, y, w, h, n.id, n.category, depth, n.kind === 'dir' ? 1 : 0]);
    if (n.kind !== 'dir' || n.size === 0) return;
    const kids = kidsOf(n.id).filter((k) => k.size > 0).sort((p, q) => q.size - p.size);
    let off = 0;
    const [ix, iy, iw, ih] = [x + pad, y + pad, w - 2 * pad, h - 2 * pad];
    for (const k of kids) {
      const f = k.size / n.size;
      if (depth % 2 === 0) place(k, ix + off * iw, iy, f * iw, ih, depth + 1);
      else place(k, ix, iy + off * ih, iw, f * ih, depth + 1);
      off += f;
    }
  };
  place(byId.get(id), 0, 0, width, height, 0);
  const buf = new ArrayBuffer(recs.length * 24);
  const dv = new DataView(buf);
  recs.forEach(([x, y, w, h, nid, cat, depth, kind], i) => {
    const o = i * 24;
    [x, y, w, h].forEach((v, k) => dv.setFloat32(o + 4 * k, v, true));
    dv.setUint32(o + 16, nid, true);
    dv.setUint16(o + 20, cat, true);
    dv.setUint8(o + 22, depth);
    const mark = searchState?.marks.get(nid) ?? 0;
    const lit = !searchState || (kind === 0 ? (mark & 1) !== 0 : mark !== 0);
    dv.setUint8(o + 23, kind | (lit ? 0 : 0x80));
  });
  return buf;
}

window.__TAURI__ = {
  core: {
    invoke: async (cmd, args) => {
      switch (cmd) {
        // index.html?start shows the start screen instead of scanning at once.
        case 'initial_path': return location.search.includes('start') ? null : 'X:\\demo';
        // A short scan with progress; ?slowscan makes it last 4 s (for looking at the scanning screen).
        case 'start_scan': {
          const ms = location.search.includes('slowscan') ? 4000 : 300;
          for (let t = 50; t < ms; t += 100) {
            setTimeout(() => handlers['scan-progress']?.({ payload: { files: Math.round(t * 37.3), dirs: Math.round(t * 4.1), bytes: t * 7.3e6, elapsedMs: t } }), t);
          }
          setTimeout(() => handlers['scan-done']?.({ payload: null }), ms);
          return null;
        }
        case 'places': return [
          { name: 'Home', path: 'C:\\Users\\demo' }, { name: 'Desktop', path: 'C:\\Users\\demo\\Desktop' },
          { name: 'Documents', path: 'C:\\Users\\demo\\Documents' }, { name: 'Downloads', path: 'C:\\Users\\demo\\Downloads' },
          { name: 'Pictures', path: 'C:\\Users\\demo\\Pictures' }, { name: 'Videos', path: 'C:\\Users\\demo\\Videos' },
        ];
        case 'summary': return {
          root: byId.get(0), files: 7, dirs: 4, links: 0, errors: 0,
          hardlinkDups: 0, scanMs: 12, categoriseMs: 1, categories,
          method: 'walk', mftSkipped: 'reading the MFT needs administrator rights', adminWouldHelp: true,
          rulesFile: 'C:\\config\\categories.toml', rulesError: null,
        };
        case 'children': return structuredClone(kidsOf(args.id));
        case 'node_path': return chain(args.id).map(([, name]) => name).join('\\');
        case 'node_details': return { row: structuredClone(byId.get(args.id)), path: chain(args.id).map(([, name]) => name).join('\\') };
        case 'ancestors': return chain(args.id);
        case 'treemap': return treemap(args);
        case 'search': return runSearch(args.filter);
        case 'clear_search': searchState = null; return null;
        case 'search_page': {
          if (!searchState || searchState.generation !== args.generation) throw new Error('That search is out of date.');
          return searchState.ids.slice(args.offset, args.offset + args.limit)
            .map((id) => ({ row: structuredClone(byId.get(id)), path: chain(id).map(([, name]) => name).join('\\') }));
        }
        case 'search_ids': {
          if (!searchState || searchState.generation !== args.generation) throw new Error('That search is out of date.');
          return args.ranges.flatMap(([a, b]) => searchState.ids.slice(a, b));
        }
        case 'basket_view': return basketView();
        case 'basket_add':
          for (const id of args.ids) {
            if (id === 0 || basketItems.includes(id) || basketItems.some((b) => below(id, b))) continue;
            basketItems = basketItems.filter((b) => !below(b, id)).concat(id);
          }
          return basketView();
        case 'basket_remove': basketItems = basketItems.filter((b) => !args.ids.includes(b)); return basketView();
        case 'basket_clear': basketItems = []; return basketView();
        case 'delete_basket': return deleteBasket(args);
        case 'cancel_scan': return null;
        case 'relaunch_elevated': throw new Error('Not started: administrator rights were not given.');
        // History: this scan plus two older ones of X:\demo.
        case 'history_list': return {
          available: true, bytes: 48 * 1024, totalScans: mockHistory.length,
          scans: mockHistory.map((h, k) => ({ id: h.id, taken: h.taken, method: 'walk', size: h.size, files: 7, current: k === 0 })),
        };
        case 'history_compare': {
          const h = mockHistory.find((x) => x.id === args.id);
          if (!h) throw new Error('That scan is no longer in the history.');
          return {
            taken: h.taken, oldTotal: h.size, newTotal: byId.get(0).size, threshold: 100, millis: 0,
            changes: [
              { path: 'X:\\demo\\big.bin-folder', name: 'b', node: 2, old: 100, new: 500, wasMissing: false },
              { path: 'X:\\demo\\old-stuff', name: 'old-stuff', node: null, old: 300, new: 0, wasMissing: false },
              { path: 'X:\\demo\\a', name: 'a', node: 1, old: 0, new: 300, wasMissing: true },
            ],
            categories: [{ key: 'documents', name: 'Documents', old: 100, new: 300 }],
          };
        }
        case 'history_clear': mockHistory.splice(1); return null;
        // Duplicates: a2.txt and deep.txt are copies (a2.txt is older).
        case 'dupes_start':
          setTimeout(() => handlers['dupes-progress']?.({ payload: { stage: 2, filesDone: 1, filesTotal: 2, bytesRead: 300, elapsedMs: 10 } }), 10);
          setTimeout(() => { dupesDone = true; handlers['dupes-done']?.({ payload: { cancelled: false } }); }, 60);
          return null;
        case 'dupes_view': {
          if (!dupesDone) return { done: false, groups: [], totalGroups: 0, totalWasted: 0 };
          const file = (id, modified) => ({ id, name: byId.get(id).name, path: chain(id).map(([, x]) => x).join('\\'), modified, category: null });
          const files = [11, 14].filter((id) => byId.has(id)).map((id) => file(id, id === 11 ? 1780000000 : 1790000000));
          const groups = files.length > 1 ? [{ size: 150, wasted: 150, files }] : [];
          return { done: true, groups, totalGroups: groups.length, totalWasted: groups.length ? 150 : 0, candidates: 7,
            candidateBytes: 1800, bytesRead: 300, changed: 0, unreadable: 0, millis: 50, minSize: args?.minSize ?? 1048576 };
        }
        // Overview: "big.bin" (Caches, safe) is the one suggestion.
        case 'overview': {
          const d = (id) => ({ row: structuredClone(byId.get(id)), path: chain(id).map(([, x]) => x).join('\\') });
          const safe = nodes.filter((n) => n.category === 2 && n.kind === 'file');
          const files = nodes.filter((n) => n.kind === 'file').sort((a, b) => b.size - a.size).slice(0, 10);
          return { safeCount: safe.length, safeBytes: safe.reduce((t, n) => t + n.size, 0), safeItems: safe.map((n) => d(n.id)), largestFiles: files.map((n) => d(n.id)) };
        }
        case 'basket_add_suggested':
          for (const n of nodes.filter((m) => m.category === 2 && m.kind === 'file')) if (!basketItems.includes(n.id)) basketItems.push(n.id);
          return basketView();
        case 'export': return { path: `C:\\exports\\${args.search ? 'search results' : byId.get(args.id).name}.${args.format}`, rows: 3, millis: 1 };
        case 'drives': return [
          { path: 'C:\\', label: 'Windows', total: 512e9, free: 120e9 },
          { path: 'D:\\', label: '', total: 2e12, free: 1.5e12 },
        ];
        default: return null;
      }
    },
  },
  event: { listen: async (name, cb) => { handlers[name] = cb; } },
};
