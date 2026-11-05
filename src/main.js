import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import './style.css';

const $ = (s) => document.querySelector(s);

const state = {
  folder: null,
  shots: [],
  index: 0,
  stats: null,
  sessions: [],
  previewUrls: new Map(), // key: `${size}:${path}` -> data url
  thumbQueue: [],
  thumbRunning: 0,
  busy: false,
};

// ---------- calendar ----------

const cal = { y: 0, m: 0 }; // m is 0-based; y=0 means "not initialized yet"
let dayMenu = null;

function sessionDateKey(s) {
  const mt = s.name.match(/^(\d{4})-(\d{2})-(\d{2})/);
  return mt ? `${mt[1]}-${mt[2]}-${mt[3]}` : null;
}

function closeDayMenu() {
  if (dayMenu) {
    dayMenu.remove();
    dayMenu = null;
  }
}

function showDayMenu(anchor, list) {
  closeDayMenu();
  const menu = document.createElement('div');
  menu.className = 'day-menu';
  for (const s of list) {
    const b = document.createElement('button');
    b.textContent = `${s.name} (${s.total} shots)`;
    b.onclick = (e) => {
      e.stopPropagation();
      closeDayMenu();
      openSession(s.path);
    };
    menu.appendChild(b);
  }
  document.body.appendChild(menu);
  const r = anchor.getBoundingClientRect();
  menu.style.top = `${r.bottom + window.scrollY + 4}px`;
  menu.style.left = `${r.left + window.scrollX}px`;
  dayMenu = menu;
}

function renderCalendar() {
  const byDay = new Map();
  for (const s of state.sessions) {
    const key = sessionDateKey(s);
    if (!key) continue;
    if (!byDay.has(key)) byDay.set(key, []);
    byDay.get(key).push(s);
  }
  $('#cal-title').textContent = new Date(cal.y, cal.m, 1).toLocaleDateString('en', {
    month: 'long',
    year: 'numeric',
  });
  const grid = $('#cal-days');
  grid.innerHTML = '';
  const startOffset = (new Date(cal.y, cal.m, 1).getDay() + 6) % 7; // Monday-first
  const daysInMonth = new Date(cal.y, cal.m + 1, 0).getDate();
  const now = new Date();
  for (let i = 0; i < startOffset; i++) {
    const pad = document.createElement('div');
    pad.className = 'cal-cell empty';
    grid.appendChild(pad);
  }
  for (let d = 1; d <= daysInMonth; d++) {
    const cell = document.createElement('div');
    cell.className = 'cal-cell';
    if (d === now.getDate() && cal.m === now.getMonth() && cal.y === now.getFullYear()) {
      cell.classList.add('today');
    }
    cell.innerHTML = `<span>${d}</span>`;
    const key = `${cal.y}-${String(cal.m + 1).padStart(2, '0')}-${String(d).padStart(2, '0')}`;
    const list = byDay.get(key);
    if (list) {
      cell.classList.add('has');
      cell.title = list.map((s) => `${s.name} — ${s.total} shots, ${s.pending} pending`).join('\n');
      const badge = document.createElement('span');
      badge.className = 'badge';
      badge.textContent = list.length;
      cell.appendChild(badge);
      cell.onclick = (e) => {
        e.stopPropagation();
        if (list.length === 1) {
          openSession(list[0].path);
        } else {
          showDayMenu(cell, list);
        }
      };
    }
    grid.appendChild(cell);
  }
}

// ---------- import view ----------

async function refreshVolumes() {
  const el = $('#volumes');
  el.innerHTML = '';
  try {
    const vols = await invoke('list_volumes');
    if (vols.length === 0) {
      el.innerHTML = '<p class="muted">No camera found under /Volumes. Insert a card or use a manual path below.</p>';
      return;
    }
    for (const v of vols) {
      const card = document.createElement('div');
      card.className = 'card';
      card.innerHTML = `
        <div class="name">${escapeHtml(v.name)}</div>
        <div class="sub">${escapeHtml(v.path)}<br>${v.file_count} media files${v.has_dcim ? ' · DCIM' : ''}</div>`;
      const btn = document.createElement('button');
      btn.textContent = 'Import';
      btn.onclick = () => startImport(v.path);
      card.appendChild(btn);
      el.appendChild(card);
    }
  } catch (e) {
    el.innerHTML = `<p class="muted">Error: ${escapeHtml(String(e))}</p>`;
  }
}

async function refreshSessions() {
  const el = $('#sessions');
  el.innerHTML = '';
  const sessions = await invoke('list_sessions');
  state.sessions = sessions;
  if (cal.y === 0) {
    // default to the month of the latest import, else today
    const mt = sessions[0] && sessionDateKey(sessions[0]);
    if (mt) {
      cal.y = Number(mt.slice(0, 4));
      cal.m = Number(mt.slice(5, 7)) - 1;
    } else {
      const now = new Date();
      cal.y = now.getFullYear();
      cal.m = now.getMonth();
    }
  }
  renderCalendar();
  if (sessions.length === 0) {
    el.innerHTML = '<p class="muted">No imports yet.</p>';
    return;
  }
  for (const s of sessions) {
    const card = document.createElement('div');
    card.className = 'card';
    card.innerHTML = `
      <div class="name">${escapeHtml(s.name)}</div>
      <div class="sub">${escapeHtml(s.path)}</div>
      <div class="sub">${s.total} shots · ${s.kept} kept · ${s.discarded} discarded · ${s.pending} pending</div>
      <div class="preview-strip"></div>`;
    const btn = document.createElement('button');
    btn.textContent = 'Open';
    btn.onclick = () => openSession(s.path);
    card.appendChild(btn);
    el.appendChild(card);
    invoke('session_previews', { folder: s.path, limit: 6 }).then((previews) => {
      const strip = card.querySelector('.preview-strip');
      for (const b64 of previews) {
        const img = document.createElement('img');
        img.src = `data:image/jpeg;base64,${b64}`;
        img.alt = '';
        img.loading = 'lazy';
        strip.appendChild(img);
      }
    }).catch(() => {});
  }
}

async function startImport(src) {
  if (!src || state.busy) return;
  state.busy = true;
  toggleSettings(false);
  $('#import-progress').classList.remove('hidden');
  $('#import-bar-fill').style.width = '0%';
  $('#import-status').textContent = 'Starting…';
  const unlisten = await listen('import-progress', (e) => {
    const { done, total, file } = e.payload;
    $('#import-bar-fill').style.width = `${Math.round((done / total) * 100)}%`;
    $('#import-status').textContent = `${done} / ${total} — ${file}`;
  });
  try {
    const res = await invoke('import_media', { src });
    $('#import-status').textContent = `Done: ${res.copied} files → ${res.dest}`;
    await openSession(res.dest);
  } catch (e) {
    $('#import-status').textContent = `Import failed: ${e}`;
  } finally {
    unlisten();
    state.busy = false;
  }
}

// ---------- review view ----------

async function openSession(folder) {
  state.folder = folder;
  state.shots = await invoke('list_shots', { folder });
  state.stats = await invoke('session_stats', { folder });
  state.previewUrls.clear();
  const pending = state.shots.findIndex((s) => s.state === 'pending');
  state.index = pending === -1 ? 0 : pending;
  $('#session-name').textContent = folder;
  $('#view-import').classList.add('hidden');
  $('#view-cull').classList.remove('hidden');
  $('#done-overlay').classList.add('hidden');
  renderStats();
  renderGrid();
  await showShot(state.index);
}

function renderStats() {
  const s = state.stats;
  if (!s) return;
  $('#stat-pending').textContent = `${s.pending} pending`;
  $('#stat-kept').textContent = `${s.kept} kept`;
  $('#stat-discarded').textContent = `${s.discarded} discarded`;
  $('#counter').textContent = `${state.shots.length ? state.index + 1 : 0} / ${s.total}`;
}

function renderGrid() {
  const grid = $('#grid');
  grid.innerHTML = '';
  state.shots.forEach((shot, i) => {
    const cell = document.createElement('div');
    cell.className = `cell ${shot.state}` + (i === state.index ? ' current' : '');
    cell.dataset.index = i;
    const img = document.createElement('img');
    img.alt = shot.name;
    cell.appendChild(img);
    const label = document.createElement('div');
    label.className = 'label';
    label.textContent = shot.name;
    cell.appendChild(label);
    cell.onclick = () => showShot(i);
    grid.appendChild(cell);
    enqueueThumb(i, img);
  });
}

function enqueueThumb(i, img) {
  state.thumbQueue.push([i, img]);
  pumpThumbs();
}

function pumpThumbs() {
  while (state.thumbRunning < 4 && state.thumbQueue.length > 0) {
    const [i, img] = state.thumbQueue.shift();
    state.thumbRunning++;
    loadPreview(state.shots[i], 300).then((url) => {
      if (url) img.src = url;
    }).catch(() => {}).finally(() => {
      state.thumbRunning--;
      pumpThumbs();
    });
  }
}

async function loadPreview(shot, size) {
  if (!shot || !shot.preview) return null;
  const key = `${size}:${shot.preview}`;
  if (state.previewUrls.has(key)) return state.previewUrls.get(key);
  const b64 = await invoke('preview', { path: shot.preview, size });
  const url = `data:image/jpeg;base64,${b64}`;
  state.previewUrls.set(key, url);
  return url;
}

async function showShot(i) {
  if (state.shots.length === 0) return;
  state.index = Math.max(0, Math.min(i, state.shots.length - 1));
  const shot = state.shots[state.index];
  $('#shot-name').textContent = `${shot.name}${shot.dir ? '  ·  ' + shot.dir : ''}`;
  const filesEl = $('#shot-files');
  filesEl.innerHTML = '';
  for (const f of shot.files) {
    const chip = document.createElement('span');
    chip.className = `fchip ${f.kind}`;
    chip.textContent = f.name;
    filesEl.appendChild(chip);
  }
  if (shot.files.length === 0) {
    const chip = document.createElement('span');
    chip.className = 'fchip';
    chip.textContent = 'files removed (discarded)';
    filesEl.appendChild(chip);
  }
  const img = $('#preview');
  $('#preview-empty').classList.add('hidden');
  img.classList.add('hidden');
  try {
    const url = await loadPreview(shot, 1600);
    if (url && state.shots[state.index] === shot) {
      img.src = url;
      img.classList.remove('hidden');
    } else if (!url) {
      $('#preview-empty').classList.remove('hidden');
    }
  } catch (e) {
    if (state.shots[state.index] === shot) $('#preview-empty').classList.remove('hidden');
  }
  document.querySelectorAll('.cell').forEach((c) => {
    c.classList.toggle('current', Number(c.dataset.index) === state.index);
  });
  renderStats();
}

function nextPending(fromIndex) {
  const n = state.shots.length;
  for (let step = 1; step <= n; step++) {
    const i = (fromIndex + step) % n;
    if (state.shots[i].state === 'pending') return i;
  }
  return (fromIndex + 1) % n;
}

async function act(newState) {
  if (state.busy || state.shots.length === 0) return;
  state.busy = true;
  const shot = state.shots[state.index];
  try {
    if (newState === 'discarded') {
      state.stats = await invoke('discard_shot', { folder: state.folder, id: shot.id });
    } else {
      state.stats = await invoke('set_shot_state', { folder: state.folder, id: shot.id, state: newState });
    }
    shot.state = newState;
    const cell = document.querySelectorAll('.cell')[state.index];
    if (cell) {
      cell.classList.remove('pending', 'kept', 'discarded');
      cell.classList.add(newState);
    }
    renderStats();
    if (state.stats.pending === 0) {
      showDone();
    } else {
      await showShot(nextPending(state.index));
    }
  } catch (e) {
    alert(`Error: ${e}`);
  } finally {
    state.busy = false;
  }
}

function showDone() {
  const s = state.stats;
  $('#done-stats').textContent = `${s.kept} kept, ${s.discarded} discarded out of ${s.total} shots. Discarded files are in the Trash.`;
  $('#done-overlay').classList.remove('hidden');
}

function escapeHtml(s) {
  return s.replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
}

// ---------- wiring ----------

const drawer = $('#settings-pane');
const backdrop = $('#drawer-backdrop');
function toggleSettings(open) {
  const o = open === undefined ? !drawer.classList.contains('open') : open;
  drawer.classList.toggle('open', o);
  backdrop.classList.toggle('open', o);
}
$('#cal-prev').onclick = () => {
  cal.m--;
  if (cal.m < 0) { cal.m = 11; cal.y--; }
  renderCalendar();
};
$('#cal-next').onclick = () => {
  cal.m++;
  if (cal.m > 11) { cal.m = 0; cal.y++; }
  renderCalendar();
};
document.addEventListener('click', (e) => {
  if (dayMenu && !dayMenu.contains(e.target)) closeDayMenu();
});
$('#btn-settings').onclick = () => toggleSettings();
$('#btn-close-settings').onclick = () => toggleSettings(false);
backdrop.onclick = () => toggleSettings(false);
$('#btn-import-manual').onclick = () => startImport($('#manual-path').value.trim());
invoke('library_path').then((p) => { $('#library-path').textContent = p; }).catch(() => {});
$('#manual-path').addEventListener('keydown', (e) => {
  if (e.key === 'Enter') startImport($('#manual-path').value.trim());
});
$('#btn-back').onclick = async () => {
  $('#view-cull').classList.add('hidden');
  $('#view-import').classList.remove('hidden');
  $('#done-overlay').classList.add('hidden');
  await refreshSessions();
  await refreshVolumes();
};
$('#btn-keep').onclick = () => act('kept');
$('#btn-cull').onclick = () => act('discarded');
$('#btn-next').onclick = () => showShot(state.index + 1);
$('#btn-prev').onclick = () => showShot(state.index - 1);
$('#btn-reveal').onclick = () => invoke('reveal', { path: state.folder });
$('#btn-done-finder').onclick = () => invoke('reveal', { path: state.folder });
$('#btn-done-back').onclick = () => $('#btn-back').click();

document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape' && drawer.classList.contains('open')) {
    toggleSettings(false);
    return;
  }
  if (!$('#view-cull').classList.contains('hidden')) {
    if (e.key === 'ArrowRight') { e.preventDefault(); showShot(state.index + 1); }
    else if (e.key === 'ArrowLeft') { e.preventDefault(); showShot(state.index - 1); }
    else if (e.key === 'k' || e.key === 'K' || e.key === 'Enter') { e.preventDefault(); act('kept'); }
    else if (e.key === 'x' || e.key === 'X' || e.key === 'Delete' || e.key === 'Backspace') { e.preventDefault(); act('discarded'); }
  } else if (e.key === 'Enter' && document.activeElement === $('#manual-path')) {
    startImport($('#manual-path').value.trim());
  }
});

refreshVolumes();
refreshSessions();
