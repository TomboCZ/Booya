'use strict';

const { invoke, convertFileSrc } = window.__TAURI__.core;
const byId = id => document.getElementById(id);
const player = byId('player');
let selected = false;
let ready = false;
let busy = false;
let captionsUrl;

function controls() {
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
  byId('source').textContent = path.split(/[\\/]/).pop();
  byId('source').title = path;
  byId('replace').hidden = false;
  byId('status').textContent = 'Recording ready for analysis.';
  preview(path);
}

async function analyze() {
  ready = false;
  player.pause();
  byId('status').textContent = 'Starting analysis…';
  const result = await invoke('tutorial_analyze');
  preview(result.video);
  ready = true;
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
