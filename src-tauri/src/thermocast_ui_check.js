(async () => {
  const waitFor = async (predicate, label) => {
    const end = Date.now() + 45000;
    while (Date.now() < end) {
      if (predicate()) return;
      await new Promise(resolve => setTimeout(resolve, 75));
    }
    throw Error(label + ': ' + [document.getElementById('status').textContent, document.getElementById('cache-size').textContent].join(' '));
  };
  const finish = value => window.__TAURI__.event.emit('desktop-check-result', value);
  const note = message => window.__TAURI__.event.emit("desktop-check-note", message);
  try {
    await note("WebView loaded");
    // The window is intentionally offscreen; the production library pauses when hidden.
    Object.defineProperty(document, 'hidden', {get: () => false});
    await refreshLibrary();
    await note("Library " + document.getElementById("cache-size").textContent);
    const open = () => [...document.querySelectorAll('#recordings button')].find(b => b.textContent === 'Open tutorial');
    await waitFor(open, 'Load Saved recordings');
    const entries = await window.__TAURI__.core.invoke('tutorial_cache');
    const saved = entries.entries[0].last_export;
    if (saved && ![...document.querySelectorAll('#recordings button')].some(b => b.textContent === 'Open export folder')) throw Error('No saved export on restart');
    open().click();
    await note("Opening recording");
    const exportButton = document.getElementById('export');
    await waitFor(() => !exportButton.disabled && !document.getElementById('player').hidden, 'Open tutorial');
    await waitFor(() => document.getElementById('player').readyState >= 1, 'Native video metadata');
    const video = document.getElementById('player');
    video.currentTime = Math.min(1.3, video.duration / 2);
    await waitFor(() => !video.seeking && video.readyState >= 2, 'Native decoded video frame');
    if (saved) {
      await waitFor(() => document.getElementById('export-path').textContent === saved, 'Restore saved location');
    }
    await note("Native decoded frame ready");
    exportButton.click();
    await waitFor(() => !exportButton.disabled && !document.getElementById('last-export').hidden, 'Export result');
    if (!document.getElementById('export-path').textContent || document.getElementById('show-export').hidden) throw Error('No visible export location');
    await waitFor(() => !busy && !refreshing, 'Finish library refresh');
    await finish('passed');
  } catch (error) {
    await finish(String(error));
  }
})();
