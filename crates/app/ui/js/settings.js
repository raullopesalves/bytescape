'use strict';

// Updates, Settings and About.

// ---------- updates ----------

let update = null; // { version, current, notes, date } while one is available

/** Asks GitHub for a newer release. Quiet unless `manual`. */
async function checkUpdate(manual) {
  if (manual) setStatus('Checking for updates…');
  try {
    update = await invoke('check_update');
  } catch (e) {
    if (manual) setStatus(`Couldn't check for updates: ${e}`, true);
    return;
  }
  $('update').hidden = !update;
  if (update) {
    $('update').textContent = `Update to ${update.version}`;
    if (manual) {
      setStatus(`Bytescape ${update.version} is available.`);
      openUpdate();
    }
  } else if (manual) {
    setStatus('Bytescape is up to date.');
  }
}

function openUpdate() {
  if (!update) return;
  const when = update.date ? `, released ${esc(update.date)}` : '';
  modal(`Update to Bytescape ${update.version}?`, `
    <p>You have ${esc(update.current)}; version ${esc(update.version)} is out${when}.</p>
    ${update.notes ? `<div class="update-notes">${esc(update.notes)}</div>` : ''}
    <p class="note">Bytescape closes, the update installs with a small progress window, and Bytescape starts again. Your settings, scan history and category rules stay. The download is checked against Bytescape's update key before it runs.</p>`,
  [['Later', '', closeModal], ['Install and restart', 'primary', installUpdate]]);
}

async function installUpdate() {
  closeModal();
  setStatus('Downloading the update…');
  try {
    await invoke('install_update'); // on success the app closes for the installer
  } catch (e) {
    setStatus(`The update didn't install: ${e}`, true);
  }
}

listen('update-progress', (e) => {
  const { done, total } = e.payload;
  setStatus(total ? `Downloading the update… ${Math.round((done * 100) / total)}%` : `Downloading the update… ${human(done)}`);
});
$('update').addEventListener('click', openUpdate);
$('check-updates').addEventListener('click', () => checkUpdate(true));
let appVersion = '';
invoke('app_version').then((v) => { appVersion = v; $('app-version').textContent = v; }).catch(() => {});
// A while after starting, so the first scan isn't slowed down.
if (store.get('autoUpdate') !== 'false') setTimeout(() => checkUpdate(false), 4000);

// ---------- settings and about ----------

const excludeList = () => (store.get('exclude') ?? '').split('\n').map((s) => s.trim()).filter(Boolean);

function applyTheme(theme) {
  const root = document.documentElement;
  if (theme === 'light' || theme === 'dark') root.dataset.theme = theme;
  else delete root.dataset.theme;
  if (map.n) {
    drawMap();
    drawOverlay();
  }
}

async function openSettings() {
  const theme = store.get('theme') ?? 'system';
  const [shell, rules] = await Promise.all([
    invoke('shell_menu_state').catch(() => false),
    invoke('rules_info').catch(() => null),
  ]);
  const radio = (v, label) => `<label><input type="radio" name="theme" value="${v}"${theme === v ? ' checked' : ''}> ${label}</label>`;
  modal('Settings', `
    <section class="set"><h3>Appearance</h3>
      ${radio('system', 'Same as Windows')}${radio('light', 'Light')}${radio('dark', 'Dark')}</section>
    <section class="set"><h3>Folders to leave out</h3>
      <p class="note">One per line. A name, like <code>node_modules</code>, leaves out every folder called that; a full path, like <code>D:\\Backups</code>, just that folder. They show in the list but aren't read. Applies from the next scan.</p>
      <textarea id="set-exclude" rows="4" spellcheck="false">${esc(excludeList().join('\n'))}</textarea></section>
    <section class="set"><h3>Search results</h3>
      <label><input type="checkbox" id="set-group"${grouped ? ' checked' : ''}> Group by folder (otherwise one list, largest first)</label></section>
    <section class="set"><h3>Explorer</h3>
      <label><input type="checkbox" id="set-shell"${shell ? ' checked' : ''}> Show “Scan with Bytescape” when right-clicking a folder or drive</label></section>
    <section class="set"><h3>Updates</h3>
      <div class="row-of"><label><input type="checkbox" id="set-auto"${store.get('autoUpdate') !== 'false' ? ' checked' : ''}> Check when Bytescape starts</label>
        <button id="set-check">Check now</button></div>
      <p class="note">This only asks GitHub for the newest version number; nothing about your files or PC is sent.</p></section>
    <section class="set"><h3>Category rules</h3>
      <p class="note">Your own rules ${rules ? `go in <code>${esc(rules[0])}</code>` : 'file'}. They add to the built-in ones.</p>
      <div class="row-of"><button id="set-rules-open">${rules?.[1] ? 'Open rules file' : 'Create rules file'}</button>
        <button id="set-rules-reload"${summary ? '' : ' disabled'}>Reload rules</button><span class="note" id="set-rules-said"></span></div></section>`,
  [['Command line', '', openCli], ['Keyboard shortcuts', '', openShortcuts], ['About', '', openAbout], ['Close', 'primary', closeModal]]);
  $('modal').querySelector('.modal-card').classList.add('wide');
  $('modal-body').onchange = async (e) => {
    const t = e.target;
    if (t.name === 'theme') {
      store.set('theme', t.value);
      applyTheme(t.value);
    } else if (t.id === 'set-exclude') {
      store.set('exclude', t.value);
      invoke('set_exclude', { exclude: excludeList() });
    } else if (t.id === 'set-group') {
      $('f-group').checked = t.checked;
      $('f-group').dispatchEvent(new Event('change'));
    } else if (t.id === 'set-shell') {
      try {
        t.checked = await invoke('set_shell_menu', { on: t.checked });
      } catch (err) {
        setStatus(String(err), true);
      }
    } else if (t.id === 'set-auto') {
      store.set('autoUpdate', t.checked);
    }
  };
  $('modal-body').onclick = async (e) => {
    const id = e.target.closest('button')?.id;
    if (id === 'set-check') {
      closeModal();
      checkUpdate(true);
    } else if (id === 'set-rules-open') {
      invoke('open_rules').catch((err) => setStatus(String(err), true));
    } else if (id === 'set-rules-reload') {
      let problem;
      try {
        problem = await invoke('reload_rules');
      } catch (err) {
        $('set-rules-said').textContent = String(err);
        return;
      }
      $('set-rules-said').textContent = problem ? `Problem in the file, built-in rules used: ${problem}` : 'Reloaded.';
      await refreshAfterDelete();
    }
  };
}

const SHORTCUTS = [
  ['Lists (tree and results)', [
    ['↑ ↓, Page Up/Down, Home, End', 'Move'],
    ['→ ←', 'Open or close a folder'],
    ['Enter', 'Open or close a folder; in the list by size, show it in the tree'],
    ['Ctrl+click, Ctrl+Space', 'Add a row to the selection, or take it out'],
    ['Shift+click, Shift+arrows', 'Select a range'],
    ['Ctrl+arrows', 'Move without changing the selection'],
    ['Ctrl+A', 'Select everything in the list'],
  ]],
  ['The selected items', [
    ['B or Delete', 'Put in the cleanup basket, or take out'],
    ['F5', 'Rescan the folder (a file’s folder)'],
    ['Ctrl+C', 'Copy the path'],
    ['Ctrl+Enter', 'Open'],
    ['Alt+Enter', 'Properties'],
    ['Menu key, Shift+F10', 'The right-click menu'],
  ]],
  ['Treemap', [
    ['Click', 'Show in the tree'],
    ['Double-click', 'Zoom in (the path above the map zooms out)'],
    ['Right-click', 'The menu'],
  ]],
  ['Anywhere', [
    ['F1', 'This list'],
    ['Esc', 'Close a menu or dialog'],
  ]],
];

function openShortcuts() {
  modal('Keyboard shortcuts', SHORTCUTS.map(([title, rows]) => `<section class="set"><h3>${title}</h3>
    <table class="keys">${rows.map(([k, what]) => `<tr><th>${k.split(', ').map((x) => `<kbd>${esc(x)}</kbd>`).join(' ')}</th><td>${esc(what)}</td></tr>`).join('')}</table></section>`).join(''),
  [['Close', 'primary', closeModal]]);
  $('modal').querySelector('.modal-card').classList.add('wide');
}

document.addEventListener('keydown', (e) => {
  if (e.key === 'F1') {
    e.preventDefault();
    openShortcuts();
  }
});

// The command-line guide: where bytescape-cli is, examples to copy, the options.
const CLI_EXAMPLES = [
  ['Scan a folder or drive: totals, categories, the largest folders and files, file types', 'C:\\Windows'],
  ['List the 25 largest of each', '--top 25 D:\\'],
  ['Save the tree as JSON, three folder levels deep', '--json scan.json --depth 3 D:\\'],
  ['Every local drive, leaving out node_modules folders', '--all-drives --exclude node_modules'],
  ['Every option', '--help'],
];
const CLI_OPTIONS = [
  ['--top N', 'How many folders, files and types to list (15)'],
  ['--json FILE', 'Save the tree as JSON'],
  ['--depth N', 'Folder levels in the JSON (4)'],
  ['--min-size BYTES', 'Leave smaller entries out of the JSON (1 MB)'],
  ['--all-drives', 'Scan every local drive instead of a path'],
  ['--exclude NAME_OR_PATH', 'Leave out a folder by name or full path; repeat for more'],
  ['--rules FILE', 'Your own category rules, after the built-in ones'],
  ['--method auto|walk|mft', 'How to read the drive; auto reads a whole drive\'s file table when run as administrator'],
  ['--threads N', 'Worker threads (one per CPU)'],
  ['--no-hardlinks', 'Count hardlinked files every time'],
  ['--show-errors', 'List everything that couldn\'t be read'],
];

async function openCli() {
  const path = await invoke('cli_path').catch(() => null);
  const run = path ? `& "${path}"` : 'bytescape-cli';
  const copyLine = (text) => `<div class="cli-line"><code>${esc(text)}</code><button data-copy="${esc(text)}">Copy</button></div>`;
  modal('Command line', `
    <p class="note">bytescape-cli scans from a terminal with the same scanner and category rules as the app, and prints what it found. It's installed with Bytescape and updates with it.</p>
    <section class="set"><h3>Where it is</h3>
      ${path ? copyLine(path) : '<p class="note">It isn\'t next to the app, so this copy of Bytescape may not have been installed with it. Download bytescape-cli from the <button class="link" data-link="/releases">releases page</button>.</p>'}
    </section>
    <section class="set"><h3>Try it in PowerShell</h3>
      ${CLI_EXAMPLES.map(([what, args]) => `<p class="note">${esc(what)}</p>${copyLine(`${run} ${args}`)}`).join('')}
      <p class="note">Run PowerShell as administrator to scan a whole drive several times faster.</p>
    </section>
    <section class="set"><h3>Options</h3>
      <table class="keys">${CLI_OPTIONS.map(([k, what]) => `<tr><th><code>${esc(k)}</code></th><td>${esc(what)}</td></tr>`).join('')}</table>
    </section>`,
  [['Close', 'primary', closeModal]]);
  $('modal').querySelector('.modal-card').classList.add('wide');
  $('modal-body').onclick = async (e) => {
    const copy = e.target.closest('[data-copy]');
    if (copy) {
      try {
        await navigator.clipboard.writeText(copy.dataset.copy);
        copy.textContent = 'Copied';
        setTimeout(() => { copy.textContent = 'Copy'; }, 1500);
      } catch (err) {
        setStatus(`Couldn't copy: ${err}`, true);
      }
      return;
    }
    const link = e.target.closest('[data-link]')?.dataset.link;
    if (link !== undefined) invoke('open_link', { url: `https://github.com/raullopesalves/bytescape${link}` }).catch((err) => setStatus(String(err), true));
  };
}

function openAbout() {
  const link = (path, label) => `<button class="link" data-link="${path}">${label}</button>`;
  modal('About Bytescape', `
    <div class="about-head"><div class="logo-slot"></div><div><b>Bytescape ${esc(appVersion)}</b>
      <p class="note">A disk usage analyzer for Windows.</p></div></div>
    <p>© 2026 Raul Alves. Free and open source under the MIT or Apache 2.0 licence, at your choice.</p>
    <div class="about-links">
      ${link('', 'Project page')}
      ${link('/releases', 'Releases and what’s new')}
      ${link('/issues', 'Report a problem or ask for a feature')}
      ${link('/blob/main/docs/THIRD_PARTY_LICENSES.md', 'Licences of the libraries it uses')}
      <button class="link" id="about-log">Show the log file</button>
    </div>
    <p class="note">Bytescape keeps a small log of scans, deletes and errors on this PC, to help find problems. Nothing is sent anywhere; checking for updates only asks GitHub for the newest version number.</p>`,
  [['Check for updates', '', () => { closeModal(); checkUpdate(true); }], ['Close', 'primary', closeModal]]);
  for (const slot of $('modal-body').querySelectorAll('.logo-slot')) slot.innerHTML = LOGO_SVG;
  $('modal-body').onclick = (e) => {
    if (e.target.closest('#about-log')) {
      invoke('open_log').catch((err) => setStatus(String(err), true));
      return;
    }
    const path = e.target.closest('[data-link]')?.dataset.link;
    if (path === undefined) return;
    invoke('open_link', { url: `https://github.com/raullopesalves/bytescape${path}` }).catch((err) => setStatus(String(err), true));
  };
}

applyTheme(store.get('theme'));
$('splash').addEventListener('click', endSplash);

/** Recently scanned folders, newest first: [{ path, at }]. */
function recentList() {
  try {
    const list = JSON.parse(store.get('recent') || '[]');
    if (Array.isArray(list) && list.length) return list;
  } catch {
    // ignore a damaged list
  }
  const last = store.get('lastPath');
  return last ? [{ path: last, at: 0 }] : [];
}

function rememberRecent(path) {
  const key = path.toLowerCase();
  const list = [{ path, at: Math.floor(Date.now() / 1000) }]
    .concat(recentList().filter((r) => r.path.toLowerCase() !== key))
    .slice(0, 6);
  store.set('recent', JSON.stringify(list));
}

/** "Windows (C:)", or the path when the drive has no label. */
function driveName(d) {
  return d.label ? `${esc(d.label)} (${esc(d.path.replace(/\\$/, ''))})` : esc(d.path);
}

/** Shows the start screen: drives, standard folders, recent scans. */
async function showStart() {
  $('scanning').hidden = true;
  $('main').hidden = true;
  $('statusbar').hidden = true;
  $('start').hidden = false;
  $('start-back').hidden = !summary;
  const [drives, places] = await Promise.all([
    invoke('drives').catch(() => []),
    invoke('places').catch(() => []),
  ]);
  const local = drives.filter((d) => d.kind !== 'network');
  if (local.length > 1) {
    // Every local drive at once, under one root.
    const total = local.reduce((t, d) => t + d.total, 0);
    const free = local.reduce((t, d) => t + d.free, 0);
    drives.push({ path: ALL_DRIVES, label: 'All drives', total, free, kind: 'all' });
  }
  $('drives').innerHTML = drives.map((d) => {
    const used = d.total ? ((d.total - d.free) * 100) / d.total : 0;
    const name = d.kind === 'all' ? 'All drives' : driveName(d);
    const kind = { network: 'Network', removable: 'Removable', all: `${local.length} drives` }[d.kind] ?? '';
    return `<button class="drive" data-path="${esc(d.path)}">
        <span class="d-name">${DRIVE_ICON}${name}${kind ? `<span class="d-kind">${kind}</span>` : ''}</span>
        <span class="d-bar"><span class="${used > 90 ? 'full' : ''}" style="width:${used}%"></span></span>
        <span class="d-free">${human(d.free)} free of ${human(d.total)}</span>
      </button>`;
  }).join('') || '<p class="note">No drives found. Browse for a folder instead.</p>';
  $('places').innerHTML = places.map((p) => `<button class="place" data-path="${esc(p.path)}" title="${esc(p.path)}">${FOLDER_ICON}${esc(p.name)}</button>`).join('');
  const recent = recentList();
  $('recent-section').hidden = !recent.length;
  $('recent').innerHTML = recent.map((r) => `<button data-path="${esc(r.path)}" title="Scan ${esc(pathLabel(r.path))} again">
      <span class="r-path">${esc(pathLabel(r.path))}</span><span class="r-when">${r.at ? when(r.at) : ''}</span></button>`).join('');
}

function backToResults() {
  if (!summary) return;
  $('start').hidden = true;
  $('main').hidden = false;
  $('statusbar').hidden = false;
  renderSoon();
  layoutMap();
}

/** The scanning screen, reset for a new scan of `path`. */
function showScanning(path) {
  $('start').hidden = true;
  $('main').hidden = true;
  $('statusbar').hidden = true;
  $('scanning').hidden = false;
  $('scan-path').textContent = pathLabel(path);
  $('scan-stage').hidden = true;
  $('ss-files').textContent = '0';
  $('ss-dirs').textContent = '0';
  $('ss-bytes').textContent = '0 B';
  $('ss-time').textContent = 'Starting…';
}
