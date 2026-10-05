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
        case 'initial_path': return 'X:\\demo';
        case 'start_scan': setTimeout(() => handlers['scan-done']?.({ payload: null }), 50); return null;
        case 'summary': return {
          root: byId.get(0), files: 7, dirs: 4, links: 0, errors: 0,
          hardlinkDups: 0, scanMs: 12, categoriseMs: 1, categories,
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
        default: return null;
      }
    },
  },
  event: { listen: async (name, cb) => { handlers[name] = cb; } },
};
