const state = { folder: '', media: null, targets: new Set(['en']), languages: {}, jobs: [], jobFilter: 'all' };
const $ = id => document.getElementById(id);

const ui = {
  empty: 'px-6 py-10 text-center text-[13px] text-[#7f92a2]',
  crumb: 'cursor-pointer bg-transparent p-[3px] text-xs text-[#a7bccc] hover:text-[#71d9ae] focus-visible:outline-2 focus-visible:outline-mint',
  divider: 'text-[#526474]',
  library: 'flex w-full cursor-pointer items-center gap-3 rounded-[9px] border p-3 text-left text-[13px] text-[#dbe6ec] hover:bg-[#1c2a36] focus-visible:outline-2 focus-visible:outline-mint',
  language: 'cursor-pointer rounded-lg border px-[5px] py-[10px] text-xs font-bold hover:border-[#5a927c] focus-visible:outline-2 focus-visible:outline-mint',
  job: 'mb-[10px] rounded-[11px] border border-[#2b3a45] bg-[#16212b] px-[14px] py-[15px]',
  stage: 'mt-[10px] mb-2 text-[11px] text-[#91a3af]',
  badge: 'whitespace-nowrap rounded-[5px] px-[7px] py-[5px] text-[10px] uppercase tracking-[0.08em]',
};

function element(tag, className = '', text) {
  const item = document.createElement(tag);
  item.className = className;
  if (text !== undefined) item.textContent = text;
  return item;
}

async function api(url, options) {
  const response = await fetch(url, options);
  let data;
  try { data = await response.json(); }
  catch { throw Error(`Request failed (${response.status})`); }
  if (!response.ok) throw Error(data.error || `Request failed (${response.status})`);
  return data;
}

function seconds(value) {
  if (!value) return 'Duration unknown';
  const total = Math.round(value);
  return `${Math.floor(total / 3600)}:${String(Math.floor(total % 3600 / 60)).padStart(2, '0')}:${String(total % 60).padStart(2, '0')}`;
}

function displayError(message) {
  $('form-error').textContent = message;
  $('form-error').hidden = !message;
}

function languageClasses(selected) {
  return `${ui.language} ${selected
    ? 'border-[#54c995] bg-[#1b4435] text-[#a4f2c9]'
    : 'border-[#344453] bg-[#18242e] text-[#b8c7d0]'}`;
}

function libraryClasses(selected) {
  return `${ui.library} ${selected
    ? 'border-[#397d65] bg-[#18352d]'
    : 'border-transparent bg-transparent'}`;
}

async function loadConfig() {
  const data = await api('/api/config');
  state.languages = data.languages;
  $('whisper-model').textContent = `Whisper ${data.whisper_model}`;

  const source = $('source-language');
  source.append(new Option('Auto detect', 'auto'));
  for (const [code, name] of Object.entries(data.languages)) {
    source.append(new Option(`${name} (${code})`, code));
  }

  const choices = $('language-grid');
  for (const [code, name] of Object.entries(data.languages)) {
    const button = element('button', languageClasses(state.targets.has(code)), `${name} · ${code.toUpperCase()}`);
    button.type = 'button';
    button.dataset.code = code;
    button.setAttribute('aria-pressed', String(state.targets.has(code)));
    button.onclick = () => {
      if (state.targets.has(code)) state.targets.delete(code);
      else state.targets.add(code);
      button.className = languageClasses(state.targets.has(code));
      button.setAttribute('aria-pressed', String(state.targets.has(code)));
    };
    choices.append(button);
  }

  const models = $('model-select');
  if (data.models.length) {
    for (const model of data.models) models.append(new Option(model, model));
    models.value = data.default_model || data.models[0];
    $('model-note').textContent = 'Select the model used for translations and cached results.';
  } else {
    models.append(new Option('No model available', ''));
    $('model-note').textContent = data.model_error
      ? `LM Studio is unavailable: ${data.model_error}`
      : 'Load a model in LM Studio to enable translations.';
  }
  models.disabled = !data.models.length;
}

function showBreadcrumb(path) {
  const bar = $('breadcrumb');
  bar.replaceChildren();
  const parts = path ? path.split('/') : [];
  const root = element('button', `${ui.crumb} ${parts.length ? '' : 'text-[#71d9ae]'}`, 'Library');
  root.type = 'button';
  root.onclick = () => loadFolder('');
  bar.append(root);
  parts.forEach((part, index) => {
    bar.append(element('span', ui.divider, '›'));
    const button = element('button', `${ui.crumb} ${index === parts.length - 1 ? 'text-[#71d9ae]' : ''}`, part);
    button.type = 'button';
    button.onclick = () => loadFolder(parts.slice(0, index + 1).join('/'));
    bar.append(button);
  });
}

async function loadFolder(path) {
  const list = $('library-list');
  list.replaceChildren(element('div', ui.empty, 'Loading folder…'));
  try {
    const data = await api(`/api/library?path=${encodeURIComponent(path)}`);
    state.folder = data.path;
    showBreadcrumb(data.path);
    list.replaceChildren();
    if (!data.entries.length) list.append(element('div', ui.empty, 'No media files in this folder.'));
    for (const entry of data.entries) {
      const selected = entry.path === state.media?.path;
      const button = element('button', libraryClasses(selected));
      button.type = 'button';
      button.append(
        element('span', 'w-[26px] shrink-0 text-center text-[19px] text-[#72d1b4]', entry.type === 'folder' ? '▰' : '▶︎'),
        element('span', 'min-w-0 flex-1 truncate', entry.name),
      );
      if (entry.type === 'folder') button.append(element('span', 'text-[#6b7d89]', '›'));
      button.onclick = () => entry.type === 'folder' ? loadFolder(entry.path) : selectMedia(entry.path);
      list.append(button);
    }
  } catch (error) {
    list.replaceChildren(element('div', ui.empty, error.message));
  }
}

async function selectMedia(path) {
  displayError('');
  try {
    const media = await api(`/api/media?path=${encodeURIComponent(path)}`);
    if (!media.tracks.length) throw Error('This file has no audio tracks');
    state.media = media;
    $('media-empty').hidden = true;
    $('media-options').hidden = false;
    $('media-name').textContent = path.split('/').at(-1);
    $('media-duration').textContent = seconds(media.duration);
    const tracks = $('audio-track');
    tracks.replaceChildren();
    for (const track of media.tracks) {
      let label = `Track ${track.index} · ${track.language.toUpperCase()} · ${track.codec}`;
      if (track.title) label += ` · ${track.title}`;
      if (track.channels) label += ` · ${track.channels} ch`;
      tracks.append(new Option(label, String(track.index)));
    }
    loadFolder(state.folder);
  } catch (error) {
    displayError(error.message);
  }
}

async function submitJob() {
  displayError('');
  if (!state.media) return displayError('Choose a video first.');
  if (!state.targets.size && !$('transcript').checked) return displayError('Select an output language or save the transcript.');
  const model = $('model-select').value;
  if (state.targets.size && !model) return displayError('Load a translation model in LM Studio first.');
  const button = $('submit-job');
  button.disabled = true;
  try {
    await api('/api/jobs', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        path: state.media.path,
        audio_stream_index: Number($('audio-track').value),
        source_language: $('source-language').value,
        transcript: $('transcript').checked,
        targets: [...state.targets],
        model,
      }),
    });
    state.jobFilter = 'all';
    await loadJobs();
  } catch (error) {
    displayError(error.message);
  } finally {
    button.disabled = false;
  }
}

function badgeClasses(status) {
  const colors = {
    completed: 'bg-[#20513a] text-[#9eebbd]',
    running: 'bg-[#354825] text-[#dcf0a2]',
    failed: 'bg-[#502e30] text-[#ffc1be]',
  };
  return `${ui.badge} ${colors[status] || 'bg-[#36404b] text-[#cad7de]'}`;
}

function filterClasses(selected) {
  return `shrink-0 cursor-pointer rounded-md border px-[9px] py-[6px] text-[11px] font-bold focus-visible:outline-2 focus-visible:outline-mint ${selected
    ? 'border-[#54c995] bg-[#1b4435] text-[#a4f2c9]'
    : 'border-[#344453] bg-[#18242e] text-[#b8c7d0] hover:border-[#5a927c]'}`;
}

function renderJobs() {
  for (const button of $('job-filters').querySelectorAll('[data-job-filter]')) {
    const selected = button.dataset.jobFilter === state.jobFilter;
    button.className = filterClasses(selected);
    button.setAttribute('aria-pressed', String(selected));
  }
  const list = $('jobs-list');
  list.replaceChildren();
  const visibleJobs = state.jobFilter === 'all'
    ? state.jobs
    : state.jobs.filter(job => job.status === state.jobFilter);
  if (!visibleJobs.length) {
    const message = state.jobFilter === 'all' ? 'No jobs yet.' : `No ${state.jobFilter} jobs.`;
    list.append(element('div', ui.empty, message));
    return;
  }
  for (const job of visibleJobs) {
    const card = element('div', ui.job);
    const top = element('div', 'flex items-center justify-between gap-[10px]');
    top.append(
      element('div', 'min-w-0 truncate text-xs font-bold', job.filename),
      element('span', badgeClasses(job.status), job.status),
    );
    card.append(top);
    let stage = job.stage;
    if (job.status === 'running' && job.stage === 'Transcribing' && job.duration) {
      stage += ` · ${seconds(job.position)} / ${seconds(job.duration)}`;
    }
    card.append(element('div', ui.stage, `${stage} · ${job.progress}%`));
    const progress = element('div', 'h-[5px] overflow-hidden rounded-[10px] bg-[#30404b]');
    const fill = element('div', 'h-full bg-mint');
    fill.style.width = `${job.progress}%`;
    progress.append(fill);
    card.append(progress);
    if (job.reused?.length) card.append(element('div', ui.stage, `Reused: ${job.reused.join(', ')}`));
    if (job.error) card.append(element('div', 'mt-[9px] text-[11px] leading-[1.4] text-[#ffafaa]', job.error));
    const codes = Object.keys(job.outputs || {});
    if (codes.length) {
      const links = element('div', 'mt-3 flex flex-wrap gap-[6px]');
      for (const code of codes) {
        const link = element('a', 'rounded-md border border-[#3a8064] px-2 py-[6px] text-[11px] font-bold text-[#91ecc0] no-underline hover:bg-[#23503c] focus-visible:outline-2 focus-visible:outline-mint', `${code.toUpperCase()} .srt ↓`);
        link.href = job.outputs[code].url;
        links.append(link);
      }
      card.append(links);
    }
    list.append(card);
  }
}

async function loadJobs() {
  try {
    state.jobs = await api('/api/jobs');
    renderJobs();
  } catch (error) {
    $('jobs-list').replaceChildren(element('div', ui.empty, error.message));
  }
}

$('refresh-library').onclick = () => loadFolder(state.folder);
$('refresh-jobs').onclick = loadJobs;
$('submit-job').onclick = submitJob;
for (const button of $('job-filters').querySelectorAll('[data-job-filter]')) {
  button.onclick = () => {
    state.jobFilter = button.dataset.jobFilter;
    renderJobs();
  };
}
renderJobs();
Promise.all([loadConfig(), loadFolder(''), loadJobs()]).catch(error => displayError(error.message));
setInterval(loadJobs, 2500);
