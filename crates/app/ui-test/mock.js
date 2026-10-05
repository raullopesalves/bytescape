// Stand-in for the Tauri API, so ui/ runs in a plain browser with a small fake
// tree. Used by ui-test/index.html; see docs/HANDOFF.md ("Testing the UI").
//
// Tree: X:\demo -> big.bin (1000 B, Caches), b/ (c/ -> deep.txt, b.txt),
//       a/ (Documents; a1.txt, a2.txt, a3.txt), z/ (empty)
const N = (id, name, kind, size, children = 0, category = 0) => ({
  id, name, kind, size, alloc: size, files: kind === 'file' ? 1 : 3,
  modified: 1790000000, category, flags: 0, children,
});
const kids = {
  0: [N(1, 'a', 'dir', 300, 3, 1), N(2, 'b', 'dir', 500, 2), N(3, 'big.bin', 'file', 1000, 0, 2), N(4, 'z', 'dir', 0, 0)],
  1: [N(10, 'a1.txt', 'file', 100), N(11, 'a2.txt', 'file', 150), N(12, 'a3.txt', 'file', 50)],
  2: [N(5, 'c', 'dir', 400, 1), N(13, 'b.txt', 'file', 100)],
  5: [N(14, 'deep.txt', 'file', 400)],
};
const categories = [
  { id: 1, key: 'documents', name: 'Documents', color: 'blue', cleanup: 'never', files: 3, size: 300, alloc: 300 },
  { id: 2, key: 'caches', name: 'Caches and temp', color: 'lime', cleanup: 'safe', files: 1, size: 1000, alloc: 1000 },
];
const handlers = {};

window.__TAURI__ = {
  core: {
    invoke: async (cmd, args) => {
      switch (cmd) {
        case 'initial_path': return 'X:\\demo';
        case 'start_scan': setTimeout(() => handlers['scan-done']?.({ payload: null }), 50); return null;
        case 'summary': return {
          root: N(0, 'X:\\demo', 'dir', 1800, 4), files: 7, dirs: 4, links: 0, errors: 0,
          hardlinkDups: 0, scanMs: 12, categoriseMs: 1, categories,
          rulesFile: 'C:\\config\\categories.toml', rulesError: null,
        };
        case 'children': return structuredClone(kids[args.id] ?? []);
        case 'node_path': return `path of node ${args.id}`;
        default: return null;
      }
    },
  },
  event: { listen: async (name, cb) => { handlers[name] = cb; } },
};
