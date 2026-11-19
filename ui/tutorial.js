'use strict';

const { invoke, convertFileSrc } = window.__TAURI__.core;
const byId = id => document.getElementById(id);
const player = byId('player');
let selected = false;
let ready = false;
let busy = false;
let captionsUrl;
let sourceDuration = 0;
let activeId = null;
let defaultProfile = "cloud";
let refreshing = false;

function controls() {
  byId('edit-fields').disabled = busy;
  byId('pick').disabled = busy;
  byId('replace').disabled = busy;
  byId('analyze').disabled = busy || !selected;
  byId('export').disabled = busy || !ready || !formats().length;
  byId('formats').disabled = busy;
  byId('sync').disabled = busy;
}

async function action(task) {
  if (busy) return;
  busy = true;
  controls();
  try { await task(); }
  catch (error) { byId('status').textContent = String(error); }
  finally { busy = false; controls(); await refreshLibrary(); }
}

function preview(path) {
  if (captionsUrl) URL.revokeObjectURL(captionsUrl);
  player.replaceChildren();
  player.src = convertFileSrc(path);
  player.hidden = false;
  byId('empty').hidden = true;
}

async function pick() {
  const path = await invoke('tutorial_import');
  if (!path) return;
  ready = false;
  activeId = null;
  selected = true;
  byId('editor').hidden = true;
  byId('editor').open = false;
  byId('source').textContent = path.split(/[\\/]/).pop();
  byId('source').title = path;
  byId('replace').hidden = false;
  byId('status').textContent = 'Recording ready for analysis.';
  preview(path);
}

async function analyze(changes = null) {
  player.pause();
  byId('status').textContent = changes ? 'Applying corrections…' : 'Starting analysis…';
  const result = await invoke('tutorial_analyze', { changes });
  await showResult(result);
}

async function showResult(result) {
  preview(result.video);
  activeId = result.source_sha256;
  ready = true;
  showEditor(result.editing);
  byId("editor").hidden = !selected;
  // MP4 subtitle support varies by WebView. Use the same exported cues for preview.
  const srt = await invoke('tutorial_subtitles');
  const cues = srt.replace(/(\d{2}:\d{2}:\d{2}),(\d{3})/g, '$1.$2');
  captionsUrl = URL.createObjectURL(new Blob(['WEBVTT\n\n', cues], { type: 'text/vtt' }));
  for (const track of Array.from(player.textTracks)) track.mode = 'disabled';
  const track = document.createElement('track');
  Object.assign(track, { kind: 'subtitles', srclang: 'en', label: 'English', src: captionsUrl, default: true });
  player.append(track);
  track.track.mode = 'showing';
  byId('status').textContent = 'Tutorial ready to preview and export.';
}

function labeled(label, input) {
  const wrapper = document.createElement('label');
  wrapper.append(label, input);
  return wrapper;
}

function addRange(start = '', end = '') {
  const row = document.createElement('div');
  for (const [label, value] of [['Start (s)', start], ['End (s)', end]]) {
    const input = document.createElement('input');
    Object.assign(input, { type: 'number', min: '0', max: String(sourceDuration), step: 'any', required: true, value });
    row.append(labeled(label, input));
  }
  const remove = document.createElement('button');
  remove.type = 'button';
  remove.textContent = 'Remove';
  remove.onclick = () => row.remove();
  row.append(remove);
  byId('ranges').append(row);
}

function showEditor(editing) {
  sourceDuration = editing.source_duration;
  byId('range-hint').textContent = `Enter times in the original ${sourceDuration.toFixed(1)}-second recording. These intervals will be preserved in the final video.`;
  byId('lines').replaceChildren();
  for (const [index, line] of editing.lines.entries()) {
    const input = document.createElement('textarea');
    Object.assign(input, { required: true, rows: 2, value: line });
    byId('lines').append(labeled(`Step ${index + 1}`, input));
  }
  byId('ranges').replaceChildren();
  for (const [start, end] of editing.protected_ranges) addRange(start, end);
  byId('editor').hidden = false;
}

function readChanges() {
  const lines = Array.from(byId('lines').querySelectorAll('textarea'), item => item.value.trim());
  if (lines.some(line => !line)) throw Error('Enter an English instruction for each step.');
  const protected_ranges = Array.from(byId('ranges').children, row => {
    const [start, end] = Array.from(row.querySelectorAll('input'), input => input.valueAsNumber);
    if (!Number.isFinite(start) || !Number.isFinite(end) || start < 0 || start >= end || end > sourceDuration) {
      throw Error('Each source interval needs a start before its end, within the recording.');
    }
    return [start, end];
  });
  return { lines, protected_ranges };
}

byId('add-range').onclick = () => addRange();
byId('edits').onsubmit = event => {
  event.preventDefault();
  action(() => analyze(readChanges()));
};

async function exportFiles() {
  const path = await invoke('tutorial_export', { formats: formats() });
  if (path) byId('status').textContent = 'Exported to ' + path;
}
byId('pick').onclick = byId('replace').onclick = () => action(pick);
byId('analyze').onclick = () => action(analyze);
byId('export').onclick = () => action(exportFiles);
window.__TAURI__.event.listen('tutorial-progress', ({payload}) => {
  const labels = { import: 'Import', transcribe: 'Transcription', vision: 'GUI detection',
    plan: 'Tutorial script', render: 'Rendering', export: 'Export' };
  const label = labels[payload.stage] || (payload.stage?.startsWith('voice-') ? 'English narration' : payload.stage);
  byId('status').textContent = [label, payload.message].filter(Boolean).join(' · ');
});
invoke('tutorial_profile').then(profile => {
  defaultProfile = profile;
  if (profile === 'fixture') byId('privacy').textContent = 'Offline fixture profile for testing the tutorial workflow.';
  if (profile === 'local') byId('privacy').textContent = 'Local analysis uses the configured on-device providers.';
});

function formats() {
  return Array.from(byId('formats').querySelectorAll('input:checked'), input => input.value);
}
byId('formats').onchange = controls;

function bytes(value) {
  return value >= 1024 ** 3 ? (value / 1024 ** 3).toFixed(1) + ' GiB'
    : value >= 1024 ** 2 ? (value / 1024 ** 2).toFixed(1) + ' MiB'
    : Math.ceil(value / 1024) + ' KiB';
}

async function restore(entry) {
  const profile = entry.profiles.includes(defaultProfile) ? defaultProfile : entry.profiles[0];
  const response = await invoke('tutorial_restore', { id: entry.id, profile });
  selected = Boolean(response.source);
  byId('source').textContent = entry.name;
  byId('source').title = response.source || entry.paths.join(' · ');
  byId('replace').hidden = false;
  await showResult(response.result);
}

function confirmClear(entry, scope) {
  const all = scope === 'all';
  const dialog = byId('confirm-clear');
  byId('confirm-title').textContent = all ? 'Delete recording cache?' : 'Clear rebuildable files?';
  byId('confirm-text').textContent = all
    ? 'Remove the saved analysis, model replies, narration and previews for ' + entry.name + '? Exported files and your original are kept.'
    : 'Clear rebuildable files for ' + entry.name + '? Transcription, model replies, narration and completed previews are kept.';
  dialog.returnValue = 'cancel';
  return new Promise(resolve => {
    dialog.addEventListener('close', () => resolve(dialog.returnValue === 'clear'), {once:true});
    dialog.showModal();
  });
}

async function clearEntry(entry, scope) {
  if (!await confirmClear(entry, scope)) return;
  const current = scope === 'all' && activeId === entry.id;
  const previous = player.src;
  if (current) { player.pause(); player.removeAttribute('src'); player.load(); }
  try {
    await invoke('tutorial_clear', {id: entry.id, scope});
  } catch (error) {
    if (current) player.src = previous;
    throw error;
  }
  if (current) {
    activeId = null; ready = false; selected = false;
    player.hidden = true;
    byId('empty').hidden = false;
    byId('editor').hidden = true;
    byId('replace').hidden = true;
    byId('source').textContent = '';
  }
  byId('status').textContent = 'Cache updated.';
}

function libraryButton(label, fn) {
  const button = document.createElement('button');
  button.type = 'button'; button.textContent = label;
  button.onclick = () => action(fn);
  return button;
}

async function refreshLibrary() {
  if (busy || refreshing || document.hidden) return;
  refreshing = true;
  try {
    const data = await invoke('tutorial_cache');
    byId('cache-size').textContent = bytes(data.total_bytes) + ' cached · ' + bytes(data.free_bytes) + ' free on disk';
    const messages = [...data.warnings];
    if (data.entries.some(e => e.source_state === 'missing')) messages.push('A source recording is missing. You can keep its tutorial or delete its cache below.');
    byId('cache-warning').textContent = messages.join(' ');
    byId('cache-warning').hidden = !messages.length;
    if (activeId && ready && !busy) {
      const current = data.entries.find(e => e.id === activeId);
      if (!current) {
        selected = ready = false; activeId = null;
        player.pause(); player.removeAttribute('src'); player.load(); player.hidden = true;
        byId('empty').hidden = false; byId('source').textContent = ''; byId('editor').hidden = true;
        byId('status').textContent = 'This tutorial was removed from the cache.';
      } else if (current.source_state !== 'available') {
        selected = false; byId('editor').hidden = true;
      }
      controls();
    }
    const items = data.entries.map(e => {
      const li = document.createElement('li');
      const title = document.createElement('strong'); title.textContent = e.name; title.title = e.paths.join(' · ');
      const info = document.createElement('span');
      info.textContent = bytes(e.bytes) + ' · Source ' + e.source_state + ' · ' + bytes(e.derived_bytes) + ' rebuildable (' + bytes(e.reclaimable_bytes) + ' unshared)';
      li.append(title, info);
      if (e.ready) li.append(libraryButton('Open tutorial', () => restore(e)));
      if (!e.ready && e.resumable) li.append(libraryButton('Resume analysis', async () => {
        const response = await invoke('tutorial_resume', {id:e.id});
        selected = Boolean(response.source);
        byId('source').textContent = e.name;
        byId('source').title = e.paths.join(' · ');
        byId('replace').hidden = false;
        await showResult(response.result);
      }));
      if (e.derived_bytes) li.append(libraryButton('Clear rebuildable', () => clearEntry(e, 'derived')));
      li.append(libraryButton('Delete cache', () => clearEntry(e, 'all')));
      return li;
    });
    byId('recordings').replaceChildren(...items);
  } catch (error) { byId('cache-size').textContent = String(error); }
  finally { refreshing = false; }
}
byId('sync').onclick = refreshLibrary;
window.addEventListener('focus',refreshLibrary);
document.addEventListener('visibilitychange',refreshLibrary);
setInterval(refreshLibrary,60000);
refreshLibrary();
