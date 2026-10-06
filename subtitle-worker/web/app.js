const state = { folder: '', media: null, targets: new Set(['en']), languages: {}, jobs: [], jobFilter: 'all', whisperModels: new Map(), whisperLocalOnly: false, defaultReviewModel: '', defaultTranslationModel: '', openReviews: new Set(), openSteps: new Set(), pendingReviews: new Set(), pendingStepActions: new Set(), jobActionFeedback: new Map(), reviewingMissing: false };
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

function updateWhisperSelection() {
  const selected = $('whisper-select').value;
  const downloaded = state.whisperModels.get(selected);
  const availability = state.whisperLocalOnly && !downloaded
    ? `Not downloaded locally. Run: docker compose run --rm subtitle-worker python download_model.py ${selected}`
    : downloaded
      ? 'Downloaded locally. A different model needs a new transcription unless its result is cached.'
      : 'This model will be downloaded when first used.';
  $('whisper-note').textContent = `${availability}${selected.endsWith('.en') ? ' This model only supports English audio.' : ''}`;
}

function updateReviewSelection() {
  const enabled = $('review-enabled').checked;
  $('review-model-select').disabled = !enabled;
  if (!enabled) {
    $('review-note').textContent = 'Review is optional. You can review saved SRTs later from Jobs & downloads.';
    return;
  }
  const reviewer = $('review-model-select').value;
  const translator = $('model-select').value;
  const configured = /translategemma/i.test(state.defaultReviewModel) ? '' : state.defaultReviewModel;
  const fallback = /translategemma/i.test(state.defaultTranslationModel) ? '' : state.defaultTranslationModel;
  const automatic = configured || (translator && !/translategemma/i.test(translator) ? translator : '') || fallback || $('review-model-select').options[1]?.value;
  let selection = 'Automatic selects a general model in LM Studio.';
  if ((reviewer || automatic) === translator && translator) {
    selection = 'Uses the translation model for review; self-review may miss errors.';
  } else if (reviewer) {
    selection = 'Uses the selected local model for final review.';
  } else if (configured) {
    selection = 'Automatic uses the configured local review model.';
  } else if (!automatic) {
    selection = 'No general review model is listed. Translations can finish with review unavailable.';
  }
  $('review-note').textContent = `${selection} Adds estimated fidelity and fluency scores, not measured accuracy. You can also review saved SRTs later.`;
}

async function loadConfig() {
  const data = await api('/api/config');
  state.languages = data.languages;
  state.whisperLocalOnly = data.whisper_local_only;
  state.defaultReviewModel = data.default_review_model || '';
  state.defaultTranslationModel = data.default_model || '';
  const whisperSelect = $('whisper-select');
  for (const item of data.whisper_models) {
    state.whisperModels.set(item.id, item.downloaded);
    whisperSelect.append(new Option(`${item.id}${item.downloaded ? ' · downloaded' : ' · download needed'}`, item.id));
  }
  whisperSelect.value = data.whisper_model;
  whisperSelect.onchange = updateWhisperSelection;
  updateWhisperSelection();

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
  const reviewers = $('review-model-select');
  reviewers.append(new Option('Automatic · suitable local model', ''));
  const savedReviewers = $('jobs-review-model');
  savedReviewers.append(new Option('Automatic · suitable local model', ''));
  for (const model of data.models.filter(model => !/translategemma/i.test(model))) {
    reviewers.append(new Option(model, model));
    savedReviewers.append(new Option(model, model));
  }
  models.onchange = updateReviewSelection;
  reviewers.onchange = updateReviewSelection;
  updateReviewSelection();
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
        whisper_model: $('whisper-select').value,
        transcript: $('transcript').checked,
        targets: [...state.targets],
        model,
        review_enabled: $('review-enabled').checked,
        review_model: $('review-model-select').value,
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
    cancelling: 'bg-[#574624] text-[#f5d89d]',
    failed: 'bg-[#502e30] text-[#ffc1be]',
  };
  return `${ui.badge} ${colors[status] || 'bg-[#36404b] text-[#cad7de]'}`;
}

function filterClasses(selected) {
  return `shrink-0 cursor-pointer rounded-md border px-[9px] py-[6px] text-[11px] font-bold focus-visible:outline-2 focus-visible:outline-mint ${selected
    ? 'border-[#54c995] bg-[#1b4435] text-[#a4f2c9]'
    : 'border-[#344453] bg-[#18242e] text-[#b8c7d0] hover:border-[#5a927c]'}`;
}

function matchesJobFilter(job, filter) {
  const statuses = [job.status, job.review_task?.status, job.step_task?.status];
  if (filter === 'all') return true;
  if (filter === 'running') return statuses.some(status => ['running', 'cancelling'].includes(status));
  if (filter === 'queued') return statuses.includes('queued');
  if (filter === 'cancelled') return statuses.includes('cancelled');
  return job.status === filter;
}

function reviewScore(value) {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0 && value <= 100
    ? `${Number(value.toFixed(1))}/100`
    : 'Unavailable';
}

function cueTime(value) {
  if (typeof value !== 'number' || !Number.isFinite(value) || value < 0) return 'Time unavailable';
  const tenths = Math.round(value * 10);
  return `${Math.floor(tenths / 36000)}:${String(Math.floor(tenths / 600) % 60).padStart(2, '0')}:${String(Math.floor(tenths / 10) % 60).padStart(2, '0')}.${tenths % 10}`;
}

function reviewMetric(label, value) {
  const metric = element('div');
  metric.append(
    element('dt', 'text-[10px] text-[#8196a3]', label),
    element('dd', 'mt-1 text-xs font-bold text-[#dbe6ec]', value),
  );
  return metric;
}

function reviewBusy(job) {
  return ['queued', 'running', 'cancelling'].includes(job.status)
    || ['queued', 'running', 'cancelling'].includes(job.review_task?.status)
    || ['queued', 'running', 'cancelling'].includes(job.step_task?.status)
    || state.pendingReviews.has(job.id)
    || stepActionPending(job)
    || state.reviewingMissing;
}

function stepActionPending(job) {
  return [...state.pendingStepActions].some(key => key.startsWith(`${job.id}:`));
}

function controlButton(job, label, step, action) {
  const button = element('button', 'cursor-pointer rounded-md border border-[#465564] px-2 py-[5px] text-[11px] font-bold text-[#bdccd5] hover:bg-[#294052] disabled:cursor-not-allowed disabled:opacity-50 focus-visible:outline-2 focus-visible:outline-mint', label);
  button.type = 'button';
  button.disabled = stepActionPending(job) || state.pendingReviews.has(job.id) || state.reviewingMissing;
  if (step) button.setAttribute('aria-label', `${label} ${job.steps[step].label || step}`);
  button.onclick = () => controlJob(job, step, action);
  return button;
}

async function controlJob(job, step, action) {
  if (stepActionPending(job) || state.pendingReviews.has(job.id) || state.reviewingMissing) return;
  const key = `${job.id}:${step || 'job'}:${action}`;
  state.pendingStepActions.add(key);
  state.jobActionFeedback.delete(job.id);
  renderJobs();
  const route = step ? `steps/${action}` : 'cancel';
  try {
    await api(`/api/jobs/${encodeURIComponent(job.id)}/${route}`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(step ? { step } : {}),
    });
    const label = step ? job.steps[step].label || step : 'job';
    state.jobActionFeedback.set(job.id, {
      message: action === 'cancel' ? `Cancellation requested for ${label}. Completed results are kept.` : `Retry queued for ${label}.`,
      error: false,
    });
    await loadJobs();
  } catch (error) {
    state.jobActionFeedback.set(job.id, { message: error.message, error: true });
  } finally {
    state.pendingStepActions.delete(key);
    renderJobs();
  }
}

function renderJobControls(card, job) {
  const activeWork = ['queued', 'running', 'cancelling'].includes(job.status)
    || ['queued', 'running', 'cancelling'].includes(job.review_task?.status)
    || ['queued', 'running', 'cancelling'].includes(job.step_task?.status);
  const active = typeof job.can_cancel === 'boolean' ? job.can_cancel || (job.cancel_requested && activeWork) : activeWork;
  if (active) {
    const cancelling = job.cancel_requested || job.status === 'cancelling';
    const actions = element('div', 'mt-3');
    const cancel = controlButton(job, cancelling ? 'Cancelling…' : 'Cancel job', null, 'cancel');
    cancel.disabled ||= cancelling;
    actions.append(cancel);
    card.append(actions);
  }
  if (Object.values(job.steps || {}).some(step => step.status === 'cancelling')) {
    card.append(element('p', 'mt-2 text-[11px] leading-[1.4] text-[#d2cc9f]', 'Stops after the current segment or model request. Completed results are kept.'));
  }
  const feedback = state.jobActionFeedback.get(job.id);
  if (feedback) {
    const message = element('p', `mt-2 text-[11px] leading-[1.4] ${feedback.error ? 'text-[#ffafaa]' : 'text-[#91a3af]'}`, feedback.message);
    message.setAttribute('role', feedback.error ? 'alert' : 'status');
    card.append(message);
  }
}

function renderSteps(card, job) {
  const steps = Object.entries(job.steps || {});
  if (!steps.length) return;
  const details = element('details', 'mt-3 rounded-lg border border-[#304351] bg-[#121c25]');
  details.open = state.openSteps.has(job.id);
  details.addEventListener('toggle', () => {
    if (details.open) state.openSteps.add(job.id);
    else state.openSteps.delete(job.id);
  });
  details.append(element('summary', 'cursor-pointer rounded-lg px-3 py-[10px] text-[11px] font-bold text-[#adbfcb] focus-visible:outline-2 focus-visible:outline-mint', 'Processing steps'));
  const body = element('div', 'space-y-3 border-t border-[#304351] px-3 py-3');
  for (const [key, step] of steps) {
    const row = element('div', 'rounded-md border border-[#304351] bg-[#16212b] p-[10px]');
    const top = element('div', 'flex items-center justify-between gap-2');
    const value = Number(step.progress);
    const progress = Number.isFinite(value) ? Math.min(100, Math.max(0, value)) : 0;
    top.append(element('p', 'min-w-0 text-[11px] font-bold text-[#dbe6ec]', step.label || key));
    top.append(element('span', badgeClasses(step.status), step.status || 'pending'));
    row.append(top);
    row.append(element('p', 'mt-2 text-[10px] text-[#91a3af]', `${progress}%`));
    if (['running', 'cancelling'].includes(step.status)) {
      const bar = element('div', 'mt-2 h-[4px] overflow-hidden rounded-[10px] bg-[#30404b]');
      const fill = element('div', 'h-full bg-mint');
      fill.style.width = `${progress}%`;
      bar.append(fill);
      row.append(bar);
    }
    if (step.error) row.append(element('p', 'mt-2 break-words text-[11px] leading-[1.4] text-[#ffafaa]', step.error));
    const buttons = element('div', 'mt-2 flex flex-wrap gap-[6px]');
    if (step.can_cancel) buttons.append(controlButton(job, 'Cancel', key, 'cancel'));
    if (step.can_retry) buttons.append(controlButton(job, 'Retry', key, 'retry'));
    if (buttons.childElementCount) row.append(buttons);
    body.append(row);
  }
  details.append(body);
  card.append(details);
}

function renderStepActivity(card, job) {
  const task = job.step_task;
  if (!task) return;
  const section = element('div', 'mt-3 rounded-lg border border-[#304351] bg-[#121c25] px-3 py-[10px]');
  const top = element('div', 'flex items-center justify-between gap-2');
  const label = job.steps?.[task.step]?.label || task.step || 'Processing step';
  top.append(element('p', 'text-[11px] font-bold text-[#adbfcb]', `Retry · ${label}`));
  top.append(element('span', badgeClasses(task.status), task.status));
  section.append(top);
  if (['queued', 'running', 'cancelling'].includes(task.status)) {
    const value = Number(task.progress);
    const percent = Number.isFinite(value) ? Math.min(100, Math.max(0, value)) : 0;
    section.append(element('p', 'mt-2 mb-2 text-[11px] text-[#91a3af]', `${task.stage || 'Waiting to retry'} · ${percent}%`));
    const progress = element('div', 'h-[5px] overflow-hidden rounded-[10px] bg-[#30404b]');
    const fill = element('div', 'h-full bg-mint');
    fill.style.width = `${percent}%`;
    progress.append(fill);
    section.append(progress);
  }
  if (task.error) section.append(element('p', 'mt-2 break-words text-[11px] leading-[1.4] text-[#ffafaa]', task.error));
  card.append(section);
}

function reviewFeedback(message, error = false) {
  const feedback = $('review-feedback');
  feedback.replaceChildren(element('p', error ? 'text-[#ffafaa]' : '', message));
  feedback.hidden = !message;
}

async function reviewJob(job, languages, force) {
  if (reviewBusy(job)) return;
  state.pendingReviews.add(job.id);
  renderJobs();
  reviewFeedback(`Queuing ${force ? 'a new review' : 'missing reviews'} for ${job.filename}…`);
  try {
    await api(`/api/jobs/${encodeURIComponent(job.id)}/review`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ review_model: $('jobs-review-model').value, force, languages }),
    });
    reviewFeedback(`Queued ${force ? 'a new review' : 'missing reviews'} for ${languages.map(code => code.toUpperCase()).join(', ')}. Saved subtitles remain available.`);
    await loadJobs();
  } catch (error) {
    reviewFeedback(error.message, true);
  } finally {
    state.pendingReviews.delete(job.id);
    renderJobs();
  }
}

async function reviewMissingJobs() {
  if (state.reviewingMissing) return;
  state.reviewingMissing = true;
  const button = $('review-missing-jobs');
  button.disabled = true;
  renderJobs();
  reviewFeedback('Checking all saved jobs for missing reviews…');
  try {
    const result = await api('/api/reviews/missing', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ review_model: $('jobs-review-model').value }),
    });
    const queued = Array.isArray(result.queued) ? result.queued : [];
    const skipped = Array.isArray(result.skipped) ? result.skipped : [];
    reviewFeedback(`${queued.length} ${queued.length === 1 ? 'job queued' : 'jobs queued'} for missing reviews. ${skipped.length} skipped.`);
    if (skipped.length) {
      const details = element('details', 'mt-2');
      details.append(element('summary', 'cursor-pointer text-[#91a3af] focus-visible:outline-2 focus-visible:outline-mint', 'Why jobs were skipped'));
      const reasons = new Map();
      for (const entry of skipped) {
        const reason = entry.reason || 'No review needed';
        reasons.set(reason, (reasons.get(reason) || 0) + 1);
      }
      const list = element('ul', 'mt-2 list-disc space-y-1 pl-4 text-[#91a3af]');
      for (const [reason, count] of reasons) list.append(element('li', '', `${count} ${count === 1 ? 'job' : 'jobs'}: ${reason}`));
      details.append(list);
      $('review-feedback').append(details);
    }
    await loadJobs();
  } catch (error) {
    reviewFeedback(error.message, true);
  } finally {
    state.reviewingMissing = false;
    button.disabled = false;
    renderJobs();
  }
}

function reviewButton(job, label, languages, force) {
  const button = element('button', 'cursor-pointer rounded-md border border-[#3a8064] px-2 py-[6px] text-[11px] font-bold text-[#91ecc0] hover:bg-[#23503c] disabled:cursor-not-allowed disabled:opacity-50 focus-visible:outline-2 focus-visible:outline-mint', label);
  button.type = 'button';
  button.disabled = reviewBusy(job);
  button.onclick = () => reviewJob(job, languages, force);
  return button;
}

function renderReviewActivity(card, job) {
  const task = job.review_task;
  if (!task) return;
  const section = element('div', 'mt-3 rounded-lg border border-[#304351] bg-[#121c25] px-3 py-[10px]');
  const top = element('div', 'flex items-center justify-between gap-2');
  top.append(element('p', 'text-[11px] font-bold text-[#adbfcb]', task.force ? 'Re-review' : 'Translation review'));
  top.append(element('span', badgeClasses(task.status), task.status));
  section.append(top);
  if (['queued', 'running', 'cancelling'].includes(task.status)) {
    const value = Number(task.progress);
    const percent = Number.isFinite(value) ? Math.min(100, Math.max(0, value)) : 0;
    section.append(element('p', 'mt-2 mb-2 text-[11px] text-[#91a3af]', `${task.stage || 'Waiting for reviewer'} · ${percent}%`));
    const progress = element('div', 'h-[5px] overflow-hidden rounded-[10px] bg-[#30404b]');
    const fill = element('div', 'h-full bg-mint');
    fill.style.width = `${percent}%`;
    progress.append(fill);
    section.append(progress);
    if (task.status === 'cancelling') section.append(element('p', 'mt-2 text-[11px] leading-[1.4] text-[#d2cc9f]', 'Stops after the current segment or model request. Completed results are kept.'));
    if (task.force && (task.languages || []).some(code => job.quality?.[code]?.status === 'completed')) {
      section.append(element('p', 'mt-2 text-[11px] leading-[1.4] text-[#8196a3]', 'The previous completed review stays visible until the new review finishes.'));
    }
  }
  if (task.error) {
    section.append(element('p', 'mt-2 break-words text-[11px] leading-[1.4] text-[#ffafaa]', task.error));
    if ((task.languages || []).some(code => job.quality?.[code]?.status === 'completed')) {
      section.append(element('p', 'mt-2 text-[11px] text-[#91a3af]', 'Previous completed reviews have been retained.'));
    }
  }
  card.append(section);
}

function renderReviewActions(card, job) {
  if (['queued', 'running', 'cancelling'].includes(job.status)) return;
  const available = job.review_available;
  const languages = Array.isArray(available?.languages) ? available.languages : [];
  if (!languages.length) {
    if (available?.reason) card.append(element('p', 'mt-3 text-[11px] leading-[1.4] text-[#8196a3]', available.reason));
    return;
  }
  const missing = languages.filter(code => job.quality?.[code]?.status !== 'completed');
  const completed = languages.filter(code => job.quality?.[code]?.status === 'completed');
  const actions = element('div', 'mt-3 flex flex-wrap gap-[6px]');
  const retry = job.review_task?.status === 'failed'
    ? (job.review_task.languages || []).filter(code => languages.includes(code))
    : [];
  if (retry.length) actions.append(reviewButton(job, 'Retry review', retry, false));
  if (missing.length) actions.append(reviewButton(job, 'Review missing', missing, false));
  if (completed.length) actions.append(reviewButton(job, 'Re-review translations', completed, true));
  card.append(actions);
  for (const [code, reason] of Object.entries(available?.rejected || {})) {
    card.append(element('p', 'mt-2 text-[11px] leading-[1.4] text-[#ffafaa]', `${code.toUpperCase()} review unavailable: ${reason}`));
  }
}

function renderQuality(card, job) {
  const reports = Object.entries(job.quality || {});
  if (!reports.length) {
    const hasTranslation = (job.targets || []).some(code => code !== job.detected_language && job.outputs?.[code]);
    if (['completed', 'failed', 'cancelled'].includes(job.status) && hasTranslation) {
      const message = job.review_enabled === false
        ? 'Review was optional and skipped. Use Review missing to review the saved subtitles.'
        : 'No translation review saved for this job.';
      card.append(element('p', 'mt-3 text-[11px] text-[#8196a3]', message));
    }
    return;
  }
  const section = element('div', 'mt-3 space-y-2');
  for (const [code, report] of reports) {
    const complete = report.status === 'completed';
    const unavailable = report.status === 'unavailable';
    const details = element('details', 'rounded-lg border border-[#304351] bg-[#121c25]');
    const key = `${job.id}:${code}`;
    details.open = state.openReviews.has(key);
    details.addEventListener('toggle', () => {
      if (details.open) state.openReviews.add(key);
      else state.openReviews.delete(key);
    });
    const summaryClass = complete ? 'text-[#a4f2c9]' : unavailable ? 'text-[#ffb9b2]' : 'text-[#adbfcb]';
    let label = `${code.toUpperCase()} · Reviewing ${report.reviewed_cues || 0}/${report.total_cues || 0} cues`;
    if (complete) label = `${code.toUpperCase()} · Estimated fidelity ${reviewScore(report.score)}`;
    if (unavailable) label = `${code.toUpperCase()} · Review unavailable`;
    details.append(element('summary', `cursor-pointer rounded-lg px-3 py-[10px] text-[11px] font-bold focus-visible:outline-2 focus-visible:outline-mint ${summaryClass}`, label));
    const body = element('div', 'space-y-3 border-t border-[#304351] px-3 py-3');
    const metrics = element('dl', 'grid grid-cols-2 gap-x-3 gap-y-3');
    if (complete) {
      metrics.append(reviewMetric('Estimated fidelity', reviewScore(report.score)), reviewMetric('Fluency', reviewScore(report.fluency)));
    }
    metrics.append(reviewMetric('Coverage', `${report.reviewed_cues || 0}/${report.total_cues || 0} cues`));
    if (complete) metrics.append(reviewMetric('Flagged cues', String(report.flagged_cues || 0)));
    body.append(metrics);
    body.append(element('p', 'break-words text-[11px] leading-[1.5] text-[#91a3af]', `Reviewer: ${report.model || 'No model available'}`));
    body.append(element('p', 'text-[11px] leading-[1.5] text-[#8196a3]', 'Model estimate against the source transcript, not measured accuracy. Transcription mistakes can affect the result.'));
    if (report.same_model) {
      body.append(element('p', 'text-[11px] leading-[1.5] text-[#d2cc9f]', 'The translation model also reviewed its own output; this can miss errors.'));
    }
    if (unavailable) {
      body.append(element('p', 'break-words text-[11px] leading-[1.5] text-[#ffafaa]', report.error || 'The local model could not complete this review.'));
      body.append(element('p', 'text-[11px] leading-[1.5] text-[#91a3af]', 'Generated SRT files remain available. Review missing resumes cached review batches.'));
    } else if (!complete) {
      body.append(element('p', 'text-[11px] leading-[1.5] text-[#91a3af]', 'Final scores appear after every cue has been reviewed.'));
    }
    const issues = (Array.isArray(report.issues) ? report.issues : []).slice(0, 5);
    if (complete && issues.length) {
      body.append(element('p', 'text-[10px] font-bold uppercase tracking-[0.08em] text-[#91a3af]', 'Cues to check'));
      for (const issue of issues) {
        const item = element('div', 'space-y-2 rounded-md border border-[#304351] bg-[#16212b] p-[10px]');
        item.append(element('p', 'text-[11px] font-bold text-[#dbe6ec]', `Cue ${issue.cue} · ${cueTime(issue.start)}`));
        item.append(element('p', 'text-[10px] text-[#91a3af]', `Fidelity ${reviewScore(issue.fidelity)} · Fluency ${reviewScore(issue.fluency)}`));
        for (const [heading, text] of [['Source transcript', issue.source], ['Translation', issue.translation]]) {
          const passage = element('div');
          passage.append(element('p', 'mb-1 text-[10px] font-bold text-[#8196a3]', heading));
          const content = element('p', 'whitespace-pre-wrap break-words text-[11px] leading-[1.5] text-[#bdccd5] [overflow-wrap:anywhere]', text || '');
          content.dir = 'auto';
          passage.append(content);
          item.append(passage);
        }
        if (issue.issue) item.append(element('p', 'break-words text-[11px] leading-[1.5] text-[#d2cc9f]', issue.issue));
        body.append(item);
      }
    } else if (complete) {
      body.append(element('p', 'text-[11px] text-[#91a3af]', 'No cues fell below the review threshold.'));
    }
    if ((job.review_available?.languages || []).includes(code) && !['queued', 'running', 'cancelling'].includes(job.status)) {
      body.append(reviewButton(job, complete ? `Re-review ${code.toUpperCase()}` : `Retry ${code.toUpperCase()} review`, [code], complete));
    }
    details.append(body);
    section.append(details);
  }
  card.append(section);
}

function renderJobs() {
  for (const button of $('job-filters').querySelectorAll('[data-job-filter]')) {
    const selected = button.dataset.jobFilter === state.jobFilter;
    button.className = filterClasses(selected);
    button.setAttribute('aria-pressed', String(selected));
  }
  const list = $('jobs-list');
  list.replaceChildren();
  const visibleJobs = state.jobs.filter(job => matchesJobFilter(job, state.jobFilter));
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
    if (job.whisper_model) card.append(element('div', 'mt-[5px] text-[11px] text-[#91a3af]', `Whisper ${job.whisper_model}`));
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
    renderReviewActivity(card, job);
    renderStepActivity(card, job);
    renderJobControls(card, job);
    renderSteps(card, job);
    renderReviewActions(card, job);
    renderQuality(card, job);
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
$('review-missing-jobs').onclick = reviewMissingJobs;
$('review-enabled').onchange = updateReviewSelection;
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
