const $ = (id) => document.getElementById(id);
const esc = (s) => String(s ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
let logTimer = null;
let logName = null;

/* ---------- Icons (inline SVG, stroke style) ---------- */

const ICONS = {
  play: '<path d="M7 4l13 8-13 8V4z"/>',
  stop: '<rect x="6" y="6" width="12" height="12" rx="1.5"/>',
  restart: '<path d="M20 12a8 8 0 1 1-2.6-5.9"/><path d="M20 4v5h-5"/>',
  refresh: '<path d="M20 12a8 8 0 1 1-2.6-5.9"/><path d="M20 4v5h-5"/>',
  folder: '<path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7z"/>',
  trash: '<path d="M4 7h16M9 7V4h6v3M6 7l1 13h10l1-13M10 11v6M14 11v6"/>',
  external: '<path d="M14 4h6v6M20 4l-9 9M18 14v5a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1h5"/>',
  globe: '<circle cx="12" cy="12" r="9"/><path d="M3 12h18M12 3c3.5 3.5 3.5 14.5 0 18M12 3c-3.5 3.5-3.5 14.5 0 18"/>',
  logs: '<path d="M7 3h8l4 4v14H7z"/><path d="M15 3v4h4M10 12h6M10 16h6"/>',
  database: '<ellipse cx="12" cy="6" rx="8" ry="3"/><path d="M4 6v6c0 1.7 3.6 3 8 3s8-1.3 8-3V6M4 12v6c0 1.7 3.6 3 8 3s8-1.3 8-3v-6"/>',
  shield: '<path d="M12 3l8 3v6c0 5-3.5 8-8 9-4.5-1-8-4-8-9V6l8-3z"/><path d="M9 12l2 2 4-4"/>',
  download: '<path d="M12 4v11M7 10l5 5 5-5M5 20h14"/>',
  upload: '<path d="M12 16V5M7 10l5-5 5 5M5 20h14"/>',
  plus: '<path d="M12 5v14M5 12h14"/>',
  x: '<path d="M6 6l12 12M18 6L6 18"/>',
  server: '<rect x="3" y="4" width="18" height="7" rx="2"/><rect x="3" y="13" width="18" height="7" rx="2"/><path d="M7 7.5h.01M7 16.5h.01"/>',
  cog: '<path d="M4 7h10M18 7h2M4 17h2M10 17h10"/><circle cx="16" cy="7" r="2"/><circle cx="8" cy="17" r="2"/>',
  archive: '<rect x="3" y="4" width="18" height="5" rx="1"/><path d="M5 9v10h14V9M10 13h4"/>',
  code: '<path d="M8 7l-5 5 5 5M16 7l5 5-5 5M14 4l-4 16"/>',
  branch: '<circle cx="6" cy="5" r="2"/><circle cx="6" cy="19" r="2"/><circle cx="18" cy="9" r="2"/><path d="M6 7v10M18 11c0 4-6 3-12 6"/>',
  undo: '<path d="M9 14L4 9l5-5"/><path d="M4 9h10a6 6 0 0 1 0 12h-3"/>',
};

function icon(name) {
  return `<svg class="icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${ICONS[name] || ''}</svg>`;
}

function hydrateIcons(root = document) {
  root.querySelectorAll('span[data-icon]').forEach((el) => { el.outerHTML = icon(el.dataset.icon); });
}

/** Icon-only button with a tooltip; `attrs` carries the data-* attributes used by the click handler. */
const ibtn = (attrs, name, title, tone = '') =>
  `<button ${attrs} title="${esc(title)}" aria-label="${esc(title)}" class="icon-btn ${tone}">${icon(name)}</button>`;

const ilink = (href, name, title, tone = '') =>
  `<a href="${esc(href)}" target="_blank" rel="noopener" title="${esc(title)}" aria-label="${esc(title)}" class="icon-btn ${tone}">${icon(name)}</a>`;

/* ---------- Tabs ---------- */

const TABS = ['projects', 'databases', 'software', 'services', 'system'];

function showTab(name) {
  if (!TABS.includes(name)) name = 'projects';
  document.querySelectorAll('[data-tab]').forEach((s) => s.classList.toggle('hidden', s.dataset.tab !== name));
  document.querySelectorAll('[data-tab-btn]').forEach((b) => b.setAttribute('aria-selected', String(b.dataset.tabBtn === name)));
  if (location.hash.slice(1) !== name) history.replaceState(null, '', '#' + name);
}

async function api(path, opts = {}) {
  const res = await fetch('/api' + path, {
    ...opts,
    headers: opts.body ? { 'Content-Type': 'application/json' } : undefined,
    body: opts.body ? JSON.stringify(opts.body) : undefined,
  });
  const text = await res.text();
  if (!res.ok) {
    let msg = text;
    try { msg = JSON.parse(text).error; } catch {}
    throw new Error(msg || res.statusText);
  }
  try { return JSON.parse(text); } catch { return text; }
}

/* ---------- Progress overlay ---------- */

const overlay = {
  busy: false,
  timer: null,
  start: 0,
  lines: [],

  open(title, message) {
    this.busy = true;
    this.lines = [];
    this.start = Date.now();
    $('ov-title').textContent = title;
    $('ov-message').textContent = message || 'Working…';
    $('ov-icon').innerHTML = '<span class="spinner"></span>';
    $('ov-bar-wrap').classList.remove('hidden');
    $('ov-bar').className = 'bar-indeterminate rounded bg-indigo-500';
    $('ov-bar').style.cssText = '';
    $('ov-log').textContent = '';
    $('ov-details').open = true;
    $('ov-copy').classList.add('hidden');
    $('ov-close').disabled = true;
    $('ov-close').textContent = 'Working…';
    $('ov-time').textContent = '0s';
    $('overlay').classList.remove('hidden');
    $('overlay').classList.add('flex');
    clearInterval(this.timer);
    this.timer = setInterval(() => {
      $('ov-time').textContent = formatElapsed(Date.now() - this.start);
    }, 500);
  },

  setLines(lines) {
    if (lines.length === this.lines.length && lines[lines.length - 1] === this.lines[this.lines.length - 1]) return;
    this.lines = lines;
    const view = $('ov-log');
    const stick = view.scrollTop + view.clientHeight >= view.scrollHeight - 20;
    view.textContent = lines.join('\n');
    if (stick) view.scrollTop = view.scrollHeight;
    const PHASE = '==> ';
    const phase = [...lines].reverse().find((l) => l.startsWith(PHASE));
    const last = [...lines].reverse().find((l) => l.trim() && !l.startsWith(PHASE) && !l.startsWith('#'));
    if (phase) {
      // Latest step as the headline, plus a short hint line when it is not noisy build output.
      $('ov-message').textContent = phase.slice(PHASE.length).trim();
    } else if (last) $('ov-message').textContent = last.trim();
  },

  finish(ok, message) {
    clearInterval(this.timer);
    this.busy = false;
    $('ov-time').textContent = formatElapsed(Date.now() - this.start);
    $('ov-icon').textContent = ok ? '✅' : '❌';
    $('ov-bar-wrap').classList.add('hidden');
    $('ov-message').textContent = message;
    $('ov-close').disabled = false;
    $('ov-close').textContent = 'Close';
    $('ov-copy').classList.toggle('hidden', ok || !this.lines.length);
    if (!ok) $('ov-details').open = true;
  },

  close() {
    if (this.busy) return;
    $('overlay').classList.add('hidden');
    $('overlay').classList.remove('flex');
  },
};

function formatElapsed(ms) {
  const s = Math.floor(ms / 1000);
  return s < 60 ? `${s}s` : `${Math.floor(s / 60)}m ${String(s % 60).padStart(2, '0')}s`;
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** Runs a state-changing API call as a job and shows its live progress in the overlay. */
async function runJob(title, path, opts = { method: 'POST' }) {
  if (overlay.busy) return;
  overlay.open(title, 'Starting…');
  try {
    const { job } = await api(path, opts);
    let failures = 0;
    for (;;) {
      await sleep(400);
      let j;
      try {
        j = await api(`/jobs/${job}`);
        failures = 0;
      } catch (e) {
        if (++failures > 10) throw new Error('Lost connection to the portal.');
        continue;
      }
      overlay.setLines(j.lines);
      if (j.status === 'queued') $('ov-message').textContent = 'Waiting for another task to finish…';
      if (j.status === 'done') {
        overlay.finish(true, 'Done');
        setTimeout(() => !overlay.busy && overlay.close(), 1200);
        break;
      }
      if (j.status === 'failed') {
        overlay.finish(false, j.error || 'Failed');
        break;
      }
    }
  } catch (e) {
    overlay.finish(false, e.message);
  }
  refresh();
}

$('ov-close').onclick = () => overlay.close();
$('ov-copy').onclick = () => navigator.clipboard.writeText(overlay.lines.join('\n'));
document.addEventListener('keydown', (e) => { if (e.key === 'Escape') overlay.close(); });

/* ---------- Dashboard ---------- */

function badge(on, yes, no) {
  return `<span class="rounded-full px-2 py-0.5 text-xs ${on ? 'bg-emerald-700' : 'bg-slate-600'}">${on ? yes : no}</span>`;
}

async function refresh() {
  try {
    const [st, projects, hosts] = await Promise.all([api('/status'), api('/projects'), api('/hosts').catch(() => null)]);

    const setup = $('setup');
    if (!st.docker_cli) {
      setup.innerHTML = '<b>Step 1: install Docker Desktop.</b> Download it from <a class="underline" target="_blank" href="https://www.docker.com/products/docker-desktop/">docker.com</a> (or run <code>winget install Docker.DockerDesktop</code>), start it and come back. The portal sets up everything else itself.';
    } else if (!st.docker) {
      setup.innerHTML = '<b>Docker Desktop is not running.</b> Start Docker Desktop and wait until it reports "running"; this page updates automatically.';
    } else if (!st.infra_ready) {
      setup.innerHTML = '<b>Almost ready:</b> the base services (proxy, MariaDB, phpMyAdmin) are not running yet. Click "Start services" (the first run downloads images and can take several minutes) or simply create a project, which starts everything automatically.';
    }
    setup.classList.toggle('hidden', st.docker_cli && st.docker && st.infra_ready);

    $('docker-text').textContent = st.docker ? 'Docker online' : 'Docker offline';
    $('docker-dot').className = 'h-2 w-2 rounded-full ' + (st.docker ? 'bg-emerald-400' : 'bg-red-500');
    $('btn-infra').classList.toggle('hidden', !st.services_down);
    if (!$('php-select').options.length) {
      $('php-select').innerHTML = st.php_versions.map((v) => `<option value="${esc(v)}" ${v === '8.5' ? 'selected' : ''}>${esc(v)}${phpNote(st, v)}</option>`).join('');
    }
    $('btn-ca').classList.toggle('hidden', !st.mkcert);

    // Do not rebuild the table while the user is picking a PHP version.
    if (!document.activeElement?.matches?.('select[data-php]')) $('projects').innerHTML = projects.length ? projects.map((p) => `
      <tr class="border-t border-slate-700">
        <td class="p-3 font-medium">${esc(p.name)}<div class="max-w-xs truncate font-mono text-xs font-normal text-slate-400" title="${esc(p.web_root)}">${esc(p.path)}</div>${p.git ? `<div class="max-w-xs truncate text-xs font-normal text-slate-400" title="${esc(p.git.remote || '')}">${icon('branch')} ${esc(p.git.branch || 'detached')}${p.git.remote ? ' · ' + esc(p.git.remote) : ''}</div>` : ''}${p.custom_path ? '<div class="text-xs font-normal text-slate-500">Custom folder (not copied)</div>' : ''}${p.db_name ? `<div class="text-xs font-normal text-slate-400">DB: ${esc(p.db_name)}</div>` : ''}</td>
        <td class="p-3"><a class="text-sky-400 hover:underline" target="_blank" href="${esc(p.url)}">${esc(p.domain)}</a></td>
        <td class="p-3"><select data-php="${esc(p.name)}" title="Change PHP version" class="rounded bg-slate-900 px-1 py-0.5 ring-1 ring-slate-600">${st.php_versions.map((v) => `<option value="${esc(v)}" ${v === p.php_version ? 'selected' : ''}>${esc(v)}${phpNote(st, v)}</option>`).join('')}</select></td>
        <td class="p-3">${p.ssl ? badge(p.ssl_ready, 'active', 'missing') : badge(false, '', 'off')}</td>
        <td class="p-3">${badge(p.enabled, 'running', 'stopped')}</td>
        <td class="whitespace-nowrap p-3 text-right">
          ${ilink(p.url, 'globe', 'Open site', 'info')}
          ${ibtn(`data-p="${esc(p.name)}" data-a="folder"`, 'folder', 'Open folder')}
          ${st.vscode ? ibtn(`data-p="${esc(p.name)}" data-a="vscode"`, 'code', 'Edit in VS Code', 'info') : ''}
          ${p.git ? ibtn(`data-p="${esc(p.name)}" data-a="git-pull"`, 'download', 'Git pull', 'info') : ''}
          ${ibtn(`data-p="${esc(p.name)}" data-a="dbl"`, 'database', p.db_name ? 'Database / import SQL' : 'Create & link database', 'info')}
          ${p.enabled
            ? ibtn(`data-p="${esc(p.name)}" data-a="restart"`, 'restart', 'Restart', 'warn') + ibtn(`data-p="${esc(p.name)}" data-a="stop"`, 'stop', 'Stop', 'bad')
            : ibtn(`data-p="${esc(p.name)}" data-a="start"`, 'play', 'Start', 'good')}
          ${ibtn(`data-p="${esc(p.name)}" data-a="delete"`, 'trash', 'Delete project', 'bad')}
        </td>
      </tr>`).join('') : '<tr><td colspan="6" class="p-6 text-center text-slate-400">No projects yet. Click "New project".</td></tr>';

    // Hide PHP containers that were never created to keep the grid tidy.
    $('containers').innerHTML = st.containers
      .filter((c) => !c.name.startsWith('ldp-php') || c.exists)
      .map((c) => `
      <div class="card p-4">
        <div class="mb-1 flex items-center justify-between"><span class="font-mono text-sm">${esc(c.name)}</span>${badge(c.running, 'running', esc(c.status))}</div>
        <div class="mb-3 truncate text-xs text-slate-400">${esc(c.image || '—')}</div>
        <div class="flex gap-1">
          ${c.running
            ? ibtn(`data-c="${esc(c.name)}" data-a="restart"`, 'restart', 'Restart', 'warn') + ibtn(`data-c="${esc(c.name)}" data-a="stop"`, 'stop', 'Stop', 'bad')
            : ibtn(`data-c="${esc(c.name)}" data-a="start"`, 'play', 'Start', 'good')}
          <span class="ml-auto">${ibtn(`data-c="${esc(c.name)}" data-a="logs"`, 'logs', 'View logs', 'info')}</span>
        </div>
      </div>`).join('') || '<p class="text-slate-400">No containers found. Click "Start services".</p>';

    if (hosts) {
      $('hosts-file').textContent = st.hosts_file;
      $('hosts').innerHTML = hosts.entries.map((e) => `<li class="flex items-center gap-3">${esc(e.ip)}\t${esc(e.domain)} ${ibtn(`data-h="${esc(e.domain)}"`, 'x', 'Remove entry', 'bad')}</li>`).join('') || '<li class="text-slate-500">(empty)</li>';
      $('btn-restore').disabled = !hosts.backup_exists;
    }

    const dbInfo = st.database;
    $('db-host').textContent = dbInfo.host;
    $('db-port').textContent = dbInfo.port;
    $('db-user').textContent = dbInfo.user;
    $('db-pass').textContent = dbInfo.password;
    $('btn-pma').href = dbInfo.phpmyadmin_url;
    $('db-warn').classList.toggle('hidden', !dbInfo.default_password);
    pmaUrl = dbInfo.phpmyadmin_url;
    await refreshDatabases(st.infra_ready);
    renderUpdates(await api('/updates').catch(() => null));
    renderBackups(await api('/backups').catch(() => null));
  } catch (e) { console.error(e); }
}

/* ---------- Configuration backups ---------- */

let promptShown = false;

function renderBackups(list) {
  if (!list) return;
  const last = list.last_backup_at;
  $('backup-status').textContent = last ? `Last backup: ${new Date(last * 1000).toLocaleString()}` : 'No backup yet.';
  $('backups').innerHTML = list.backups.length ? list.backups.map((b) => `
    <tr>
      <td class="p-3 font-mono text-xs">${esc(b.name)}</td>
      <td class="p-3">${esc(new Date(b.created_at * 1000).toLocaleString())}</td>
      <td class="p-3">${b.projects}</td>
      <td class="p-3">${formatBytes(b.size)}</td>
      <td class="whitespace-nowrap p-3 text-right">
        ${ilink(`/api/backups/${encodeURIComponent(b.name)}/download`, 'download', 'Download', 'info')}
        ${ibtn(`data-restore-backup="${esc(b.name)}"`, 'undo', 'Restore this backup', 'warn')}
        ${ibtn(`data-del-backup="${esc(b.name)}"`, 'trash', 'Delete backup', 'bad')}
      </td>
    </tr>`).join('') : '<tr><td colspan="5" class="p-6 text-center text-slate-400">No backups yet.</td></tr>';

  const snooze = Number(localStorage.getItem('ldp-backup-snooze-until') || 0);
  if (list.due && !promptShown && Date.now() > snooze && !overlay.busy) {
    promptShown = true;
    $('backup-prompt-text').textContent = last
      ? `Your newest backup is from ${new Date(last * 1000).toLocaleDateString()}, more than a month ago. Create a fresh one now?`
      : 'You have projects but no configuration backup yet. Create one now?';
    $('backup-prompt').showModal();
  }
}

function createBackup() { runJob('Creating configuration backup', '/backups'); }
$('btn-backup').onclick = createBackup;
$('backup-now').onclick = () => { $('backup-prompt').close(); createBackup(); };
$('backup-later').onclick = () => {
  localStorage.setItem('ldp-backup-snooze-until', String(Date.now() + 24 * 3600 * 1000));
  $('backup-prompt').close();
};
$('btn-import').onclick = () => $('import-file').click();
$('import-file').onchange = async (ev) => {
  const file = ev.target.files[0];
  ev.target.value = '';
  if (!file) return;
  const content = await file.text();
  runJob(`Importing backup "${file.name}"`, '/backups/import', { method: 'POST', body: { content } });
};

document.addEventListener('click', (ev) => {
  const r = ev.target.closest('button[data-restore-backup]');
  if (r) {
    const n = r.dataset.restoreBackup;
    if (confirm(`Restore "${n}"? Projects, certificates, hosts entries and Nginx configuration are rebuilt from it. A safety backup of the current state is made first.`)) runJob(`Restoring backup "${n}"`, `/backups/${encodeURIComponent(n)}/restore`);
    return;
  }
  const d = ev.target.closest('button[data-del-backup]');
  if (d && confirm(`Delete backup "${d.dataset.delBackup}"?`)) runJob('Deleting backup', `/backups/${encodeURIComponent(d.dataset.delBackup)}`, { method: 'DELETE' });
});

function renderUpdates(rep) {
  const banner = $('updates-banner');
  if (!rep) {
    $('software').innerHTML = '<tr><td colspan="5" class="p-6 text-center text-slate-400">No check yet. Start the services and click "Check for updates".</td></tr>';
    $('software-note').textContent = '';
    banner.classList.add('hidden');
    $('tab-badge').classList.add('hidden');
    return;
  }
  const label = {
    eol:  ['End of life', 'bg-red-700'],
    security_only: ['Security fixes only', 'bg-amber-700'],
    supported: ['Supported', 'bg-emerald-700'],
    unknown: ['Unknown', 'bg-slate-600'],
  };
  $('software').innerHTML = rep.components.map((c) => {
    const [text, cls] = label[c.support] || label.unknown;
    const upd = c.image_update === true ? '<span class="ml-1 rounded-full bg-sky-700 px-2 py-0.5 text-xs">Update available</span>'
      : c.image_update === false ? '<span class="ml-1 rounded-full bg-slate-600 px-2 py-0.5 text-xs">Up to date</span>' : '';
    return `<tr class="border-t border-slate-700">
      <td class="p-3 font-medium">${esc(c.name)}<div class="font-mono text-xs font-normal text-slate-400">${esc(c.image)}</div>${c.id.startsWith('php') && c.id !== 'phpmyadmin' ? `<div class="text-xs font-normal text-slate-400">${c.used_by?.length ? 'Used by: ' + esc(c.used_by.join(', ')) : 'Not used by any project'}</div>` : ''}</td>
      <td class="p-3 font-mono">${esc(c.installed || '—')}</td>
      <td class="p-3 font-mono">${esc(c.latest || '—')}</td>
      <td class="p-3"><span class="rounded-full px-2 py-0.5 text-xs ${cls}">${text}</span>${c.eol_date ? ` <span class="text-xs text-slate-400">until ${esc(c.eol_date)}</span>` : ''}${upd}</td>
      <td class="space-x-1 p-3 text-right">${c.image_update === false ? '' : ibtn(`data-upd="${esc(c.id)}"`, 'download', 'Update', 'info')}${c.removable ? ibtn(`data-rmphp="${esc(c.id.slice(3, 4) + '.' + c.id.slice(4))}"`, 'trash', 'Remove', 'bad') : ''}</td>
    </tr>`;
  }).join('') || '<tr><td colspan="5" class="p-6 text-center text-slate-400">No containers yet.</td></tr>';

  const when = new Date(rep.checked_at * 1000).toLocaleString();
  $('software-note').textContent = `Last checked ${when}.` + (rep.online ? '' : ' Some checks could not reach the internet, results may be incomplete.') + ' Checks run automatically once a day. Updating recreates containers; your files and databases are kept.';

  const eol = rep.components.filter((c) => c.support === 'eol');
  const upd = rep.components.filter((c) => c.image_update === true);
  const parts = [];
  if (eol.length) parts.push(`<b>End of life:</b> ${eol.map((c) => esc(c.name)).join(', ')} no longer receive security fixes. For PHP, switch your projects to a newer version.`);
  if (upd.length) parts.push(`<b>Updates available</b> for ${upd.map((c) => esc(c.name)).join(', ')}.`);
  banner.className = 'rounded-lg border p-4 text-sm ' + (eol.length ? 'border-red-600 bg-red-900/30' : 'border-amber-600 bg-amber-900/30');
  banner.innerHTML = parts.join('<br>') + (upd.length ? ' <button id="banner-update" class="ml-2 rounded bg-indigo-600 px-3 py-1 hover:bg-indigo-500">Update now</button>' : '');
  banner.classList.toggle('hidden', !parts.length);
  const attention = new Set([...eol, ...upd].map((c) => c.id)).size;
  $('tab-badge').textContent = attention;
  $('tab-badge').classList.toggle('hidden', !attention);
}

let pmaUrl = 'http://localhost:8080';

const formatBytes = (b) => (b < 1024 ? `${b} B` : b < 1048576 ? `${(b / 1024).toFixed(1)} KB` : `${(b / 1048576).toFixed(1)} MB`);

async function refreshDatabases(ready) {
  let dbs = null;
  if (ready) dbs = await api('/databases').catch(() => null);
  $('databases').innerHTML = dbs
    ? dbs.map((d) => `
      <tr class="border-t border-slate-700">
        <td class="p-3 font-mono">${esc(d.name)}</td>
        <td class="p-3">${esc(d.project || '—')}</td>
        <td class="p-3">${d.tables}</td>
        <td class="p-3">${formatBytes(d.bytes)}</td>
        <td class="space-x-1 p-3 text-right">
          ${ilink(`${pmaUrl}/index.php?route=/database/structure&db=${encodeURIComponent(d.name)}`, 'external', 'Open in phpMyAdmin', 'info')}
          ${ibtn(`data-db="${esc(d.name)}"`, 'trash', 'Drop database', 'bad')}
        </td>
      </tr>`).join('')
    : `<tr><td colspan="5" class="p-6 text-center text-slate-400">${ready ? 'Could not read databases yet (MariaDB may still be starting).' : 'Start the services to manage databases.'}</td></tr>`;
}

async function loadLogs() {
  if (!logName) return;
  try {
    const view = $('log-view');
    const stick = view.scrollTop + view.clientHeight >= view.scrollHeight - 20;
    view.textContent = (await api(`/containers/${logName}/logs?tail=300`)) || '(no logs)';
    if (stick) view.scrollTop = view.scrollHeight;
  } catch (e) { $('log-view').textContent = e.message; }
}

function openLogs(name) {
  logName = name;
  $('log-name').textContent = name;
  $('log-section').classList.remove('hidden');
  clearInterval(logTimer);
  loadLogs();
  logTimer = setInterval(loadLogs, 2000);
  $('log-section').scrollIntoView({ behavior: 'smooth' });
}

const cap = (s) => s.charAt(0).toUpperCase() + s.slice(1);

function phpNote(st, v) {
  const s = st.php_support[v];
  return s === 'eol' ? ' (end of life)' : s === 'security_only' ? ' (security fixes only)' : '';
}

document.addEventListener('change', (ev) => {
  const sel = ev.target.closest?.('select[data-php]');
  if (!sel) return;
  const n = sel.dataset.php;
  runJob(`Switching "${n}" to PHP ${sel.value}`, `/projects/${n}/php`, { method: 'POST', body: { php_version: sel.value } });
  sel.blur();
});

document.addEventListener('click', (ev) => {
  const u = ev.target.closest('button[data-upd]');
  if (u) {
    const id = u.dataset.upd;
    if (confirm(`Update ${id}? Its container is recreated (PHP images are rebuilt). Files and databases are kept.`)) runJob(`Updating ${id}`, '/updates/apply', { method: 'POST', body: { components: [id] } });
    return;
  }
  if (ev.target.id === 'banner-update') return updateAll();
  const rm = ev.target.closest('button[data-rmphp]');
  if (rm) {
    const v = rm.dataset.rmphp;
    if (confirm(`Remove the PHP ${v} container and image? No project uses it; it is recreated automatically if you switch a project to PHP ${v} later.`)) runJob(`Removing PHP ${v}`, `/php/${v}/remove`);
    return;
  }
  const dbBtn = ev.target.closest('button[data-db]');
  if (dbBtn) {
    const n = dbBtn.dataset.db;
    if (confirm(`Drop database "${n}"? All its tables and data are permanently deleted.`)) runJob(`Dropping database "${n}"`, `/databases/${encodeURIComponent(n)}`, { method: 'DELETE' });
    return;
  }
  const h = ev.target.closest('button[data-h]');
  if (h) {
    if (confirm(`Remove hosts entry ${h.dataset.h}?`)) runJob(`Removing hosts entry ${h.dataset.h}`, `/hosts/entry/${encodeURIComponent(h.dataset.h)}`, { method: 'DELETE' });
    return;
  }
  const b = ev.target.closest('button[data-a]');
  if (!b) return;
  const a = b.dataset.a;
  if (b.dataset.c) {
    if (a === 'logs') return openLogs(b.dataset.c);
    runJob(`${cap(a)} ${b.dataset.c}`, `/containers/${b.dataset.c}/${a}`);
  } else if (b.dataset.p) {
    const n = b.dataset.p;
    if (a === 'folder') return api(`/projects/${n}/open-folder`, { method: 'POST' }).catch((e) => alert(e.message));
    if (a === 'vscode') return api(`/projects/${n}/vscode`, { method: 'POST' }).catch((e) => alert(e.message));
    if (a === 'dbl') return openLinkDb(n).catch((e) => alert(e.message));
    if (a === 'delete') {
      if (!confirm(`Delete project "${n}" (hosts entry, certificate and vhost)? Site files are kept.`)) return;
      runJob(`Deleting project "${n}"`, `/projects/${n}`, { method: 'DELETE' });
    } else runJob(`${cap(a)} project "${n}"`, `/projects/${n}/${a}`);
  }
});

$('log-close').onclick = () => { clearInterval(logTimer); logName = null; $('log-section').classList.add('hidden'); };
function updateAll() {
  if (confirm('Update all software? Containers are recreated (PHP images are rebuilt) and sites may be unavailable for a minute. Files and databases are kept.')) {
    runJob('Updating all software', '/updates/apply', { method: 'POST', body: { components: [] } });
  }
}
$('btn-update-all').onclick = updateAll;
$('btn-check').onclick = () => runJob('Checking for updates', '/updates/check');
$('btn-newdb').onclick = () => $('dbdialog').showModal();
$('db-cancel').onclick = () => $('dbdialog').close();
$('db-form').onsubmit = (ev) => {
  ev.preventDefault();
  const name = new FormData(ev.target).get('name').trim();
  $('dbdialog').close();
  ev.target.reset();
  runJob(`Creating database "${name}"`, '/databases', { method: 'POST', body: { name } });
};
$('btn-new').onclick = () => { setSource('empty'); $('wizard').showModal(); };
$('wizard-cancel').onclick = () => $('wizard').close();

const wf = $('wizard-form');
let nameTouched = false;
const nameFromUrl = (u) => u.trim().replace(/[?#].*$/, '').replace(/\/+$/, '').split(/[/:\\]/).pop().replace(/\.git$/i, '').toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '');

function setSource(src) {
  wf.elements.source.value = src;
  document.querySelectorAll('[data-source]').forEach((el) => el.classList.toggle('hidden', el.dataset.source !== src));
  $('sql-row').classList.toggle('hidden', src === 'empty');
  wf.elements.git_url.required = src === 'git';
  wf.elements.folder.required = src === 'folder';
  nameTouched = false;
}
wf.querySelectorAll('input[name=source]').forEach((r) => { r.onchange = () => setSource(r.value); });
wf.elements.name.oninput = () => { nameTouched = true; };
wf.elements.git_url.oninput = (ev) => { if (!nameTouched) wf.elements.name.value = nameFromUrl(ev.target.value); };
api('/status').then((st) => $('git-missing').classList.toggle('hidden', st.git_available !== false)).catch(() => {});

async function inspectFolder() {
  const path = wf.elements.folder.value.trim();
  if (!path) return;
  try {
    const r = await api('/inspect-folder', { method: 'POST', body: { path } });
    if (!r.exists) { $('folder-info').textContent = 'Folder not found.'; return; }
    if (!nameTouched && r.suggested_name) wf.elements.name.value = r.suggested_name;
    if (r.suggested_docroot && !wf.elements.docroot.value) wf.elements.docroot.value = r.suggested_docroot;
    $('sql-options').innerHTML = r.sql_files.map((f) => `<option value="${esc(f)}">`).join('');
    const g = r.git ? `Git repository: branch ${r.git.branch || '?'}${r.git.remote ? ' · ' + r.git.remote : ''}. ` : 'Not a Git repository. ';
    $('folder-info').textContent = g + (r.sql_files.length ? `${r.sql_files.length} SQL file(s) found.` : '');
  } catch (e) { $('folder-info').textContent = e.message; }
}
wf.elements.folder.onchange = inspectFolder;
$('btn-browse').onclick = async () => {
  try {
    const r = await api('/pick-folder', { method: 'POST' });
    if (r.path) { wf.elements.folder.value = r.path; inspectFolder(); }
  } catch (e) { alert(e.message); }
};

let linkProject = null;
$('linkdb-cancel').onclick = () => $('linkdb').close();
async function openLinkDb(name) {
  linkProject = name;
  const p = (await api('/projects')).find((x) => x.name === name);
  $('linkdb-project').textContent = name;
  $('linkdb-form').reset();
  $('linkdb-form').elements.db_name.value = p?.db_name || name.replace(/-/g, '_');
  $('linkdb-options').innerHTML = (await api(`/projects/${name}/sql-files`).catch(() => [])).map((f) => `<option value="${esc(f)}">`).join('');
  $('linkdb').showModal();
}
$('linkdb-form').onsubmit = (ev) => {
  ev.preventDefault();
  const f = new FormData(ev.target);
  const body = { db_name: f.get('db_name').trim(), sql_file: f.get('sql_file').trim() || null };
  $('linkdb').close();
  runJob(`Database for "${linkProject}"`, `/projects/${linkProject}/database`, { method: 'POST', body });
};
$('btn-infra').onclick = () => runJob('Starting base services', '/infra/up');
$('btn-restore').onclick = () => confirm('Restore the hosts file backup?') && runJob('Restoring hosts file backup', '/hosts/restore');
$('btn-ca').onclick = () => runJob('Installing mkcert root certificate', '/certs/install-ca');

$('wizard-form').onsubmit = (ev) => {
  ev.preventDefault();
  const f = new FormData(ev.target);
  const src = f.get('source');
  const body = {
    name: f.get('name').trim(),
    domain: f.get('domain').trim() || null,
    php_version: f.get('php_version'),
    db_name: f.get('db_name').trim() || null,
    docroot: f.get('docroot').trim(),
    ssl: f.get('ssl') === 'on',
    source: src,
    git_url: src === 'git' ? f.get('git_url').trim() : null,
    git_branch: src === 'git' ? f.get('git_branch').trim() || null : null,
    folder: src === 'folder' ? f.get('folder').trim() : null,
    sql_file: src !== 'empty' ? f.get('sql_file').trim() || null : null,
  };
  $('wizard').close();
  runJob(`${src === 'git' ? 'Cloning' : 'Creating'} project "${body.name}"`, '/projects', { method: 'POST', body });
  ev.target.reset();
  setSource('empty');
};

hydrateIcons();
document.querySelectorAll('[data-tab-btn]').forEach((b) => { b.onclick = () => showTab(b.dataset.tabBtn); });
window.addEventListener('hashchange', () => showTab(location.hash.slice(1)));
showTab(location.hash.slice(1));
refresh();
setInterval(refresh, 5000);

/* ---------- Progressive web app ---------- */

if ('serviceWorker' in navigator) navigator.serviceWorker.register('/sw.js').catch(() => {});

let installPrompt = null;
window.addEventListener('beforeinstallprompt', (ev) => {
  ev.preventDefault();
  installPrompt = ev;
  $('btn-install').classList.remove('hidden');
});
window.addEventListener('appinstalled', () => {
  installPrompt = null;
  $('btn-install').classList.add('hidden');
});
$('btn-install').onclick = async () => {
  if (!installPrompt) return;
  installPrompt.prompt();
  await installPrompt.userChoice;
  installPrompt = null;
  $('btn-install').classList.add('hidden');
};
