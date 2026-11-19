'use strict';

const { invoke, convertFileSrc } = window.__TAURI__.core;
const byId = id => document.getElementById(id);
const player = byId('player');
let selected = false;
let ready = false;
let busy = false;
let captionsUrl;
let sourceDuration = 0;

function controls() {
  byId('edit-fields').disabled = busy;
  byId('pick').disabled = busy;
  byId('replace').disabled = busy;
  byId('analyze').disabled = busy || !selected;
  byId('export').disabled = busy || !ready;
}

async function action(task) {
  if (busy) return;
  busy = true;
  controls();
  try { await task(); }
  catch (error) { byId('status').textContent = String(error); }
  finally { busy = false; controls(); }
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
  preview(result.video);
  ready = true;
  showEditor(result.editing);
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
  const path = await invoke('tutorial_export');
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
  if (profile === 'fixture') byId('privacy').textContent = 'Offline fixture profile for testing the tutorial workflow.';
  if (profile === 'local') byId('privacy').textContent = 'Local analysis uses the configured on-device providers.';
});
