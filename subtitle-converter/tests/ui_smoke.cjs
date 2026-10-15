'use strict';

// Isolated browser regression: serves production assets and intercepts every API.
// NODE_PATH may point to an installed Playwright package directory.
const { chromium } = require('playwright');
const assert = require('node:assert/strict');
const http = require('node:http');
const fs = require('node:fs/promises');
const path = require('node:path');

async function main() {
  const web = path.resolve(__dirname, '../web');
  const output = process.env.SUBFORGE_UI_OUTPUT && path.resolve(process.env.SUBFORGE_UI_OUTPUT);
  if (output) await fs.mkdir(output, { recursive: true });
  const server = http.createServer(async (req, res) => {
    const asset = { '/': 'index.html', '/static/app.js': 'app.js', '/static/style.css': 'style.css', '/static/icon.svg': 'icon.svg' }[req.url];
    if (!asset) { res.writeHead(404); res.end(); return; }
    const mime = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.svg': 'image/svg+xml' }[path.extname(asset)];
    res.writeHead(200, { 'Content-Type': mime });
    res.end(await fs.readFile(path.join(web, asset)));
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  let browser;
  try {
    browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE } : {}) });
    const context = await browser.newContext({ viewport: { width: 1440, height: 1000 }, reducedMotion: 'reduce' });
    const page = await context.newPage();
    const assertViewport = async label => {
      await page.waitForFunction(() => document.documentElement.scrollHeight <= innerHeight && document.body.scrollHeight <= innerHeight, null, { timeout: 2000 });
      await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
      const sizes = await page.evaluate(() => {
        const workspace = document.querySelector('#workspace-scroll');
        return { height: innerHeight, width: innerWidth, documentHeight: document.documentElement.scrollHeight, documentWidth: document.documentElement.scrollWidth, bodyHeight: document.body.scrollHeight, scrollY, workspaceWidth: workspace.clientWidth, workspaceScrollWidth: workspace.scrollWidth };
      });
      assert(sizes.documentHeight <= sizes.height && sizes.bodyHeight <= sizes.height, `${label}: document stays inside the viewport ${JSON.stringify(sizes)}`);
      assert.equal(sizes.documentWidth, sizes.width, `${label}: no document horizontal scrolling`);
      assert.equal(sizes.scrollY, 0, `${label}: document never scrolls`);
      assert(sizes.workspaceScrollWidth <= sizes.workspaceWidth, `${label}: workspace has no clipped horizontal content`);
    };
    const assertTablePane = async (pageName, width, height) => {
      const measurements = await page.evaluate(name => {
        const workspace = document.querySelector('#workspace-scroll');
        const pane = document.querySelector(name === 'library' ? '.inventory-table-wrap' : '.jobs-table-wrap');
        const footer = document.querySelector(name === 'library' ? '.inventory-pagination' : '.jobs-pagination');
        return { paneHeight: pane?.clientHeight || 0, workspaceClient: workspace.clientHeight, workspaceScroll: workspace.scrollHeight, workspaceTop: workspace.scrollTop, footerBottom: footer?.getBoundingClientRect().bottom ?? Infinity, height: innerHeight };
      }, pageName);
      assert(measurements.paneHeight > 0, `${pageName} table body pane has usable height at ${width}x${height}: ${JSON.stringify(measurements)}`);
      if (pageName === 'library' && width <= 520 && height <= 480) assert(measurements.paneHeight >= 60, `Library retains at least 60px for rows at ${width}x${height}: ${JSON.stringify(measurements)}`);
      assert(measurements.workspaceScroll <= measurements.workspaceClient + 1, `${pageName} keeps page scrolling inside its table pane at ${width}x${height}: ${JSON.stringify(measurements)}`);
      assert.equal(measurements.workspaceTop, 0, `${pageName} workspace does not scroll at ${width}x${height}`);
      assert(measurements.footerBottom <= height + 1, `${pageName} footer stays in the viewport at ${width}x${height}: ${JSON.stringify(measurements)}`);
    };
    const assertToggleGlyph = async () => {
      const visible = await page.locator('#sidebar-collapse').evaluate(button => {
        const svg = button.querySelector('svg').getBoundingClientRect(), path = button.querySelector('path').getBoundingClientRect();
        return path.width > 0 && path.height > 0 && path.left >= svg.left - 1 && path.right <= svg.right + 1 && path.top >= svg.top - 1 && path.bottom <= svg.bottom + 1;
      });
      assert(visible, 'toggle glyph stays inside its SVG when expanded or collapsed');
    };
    const errors = [], writes = [];
    let workerReads = 0;
    await page.exposeFunction('__workerReads', () => workerReads);
    page.on('pageerror', error => errors.push(error.message));
    const now = Math.floor(Date.now() / 1000);
    const roots = [{ id: 'movies', label: 'Movies', path: '/media/movies' }, { id: 'tv', label: 'Television', path: '/media/tv' }];
    let items = [
      { name: 'Example movie.mkv', path: '/media/movies/Example movie.mkv', root_id: 'movies', type: 'video', tracks: [{ id: 3, language: 'en', codec: 'SubStationAlpha' }, { id: 4, language: 'fr', codec: 'PGS', image: true }], processing: { failed: true, last_error: 'Track 3: output has an invalid timestamp', pending_ocr: true } },
      { name: 'Example show S01E01.mkv', path: '/media/tv/Example show S01E01.mkv', root_id: 'tv', type: 'video', tracks: [{ id: 2, language: 'en', codec: 'SRT' }], processing: { generated_srt_count: 1 } }
    ];
    const jobs = [
      { id: 'failed-1', kind: 'extract', status: 'completed_with_errors', created: now, finished: now, paths: [items[0].path], summary: { failed: 1 }, file_errors: [{ message: items[0].processing.last_error, count: 1, examples: [items[0].name] }], failed_paths: [items[0].path] },
      { id: 'executors-1', kind: 'extract', status: 'completed', created: now - 30, finished: now - 30, paths: ['/media/movies/executors-fixture.mkv'], summary: { converted: 6 }, executors: [{ node_name: 'Local', resource: 'cpu', execution: 'local' }, { node_name: 'Studio GPU', resource: 'gpu', execution: 'remote' }, { node_name: 'Local', resource: 'cpu_ocr', execution: 'local' }, { node_name: 'Studio GPU', resource: 'gpu', execution: 'remote' }, { node_name: 'Local', resource: 'cpu', execution: 'local' }, { node_name: 'Studio GPU', resource: 'gpu', execution: 'remote' }], executors_truncated: true },
      { id: 'backup-1', kind: 'backup', status: 'completed', created: now - 60, summary: {} }
    ];
    jobs[0].executors=[null,{resource:'unknown',node_name:'Invalid',execution:'remote'},{resource:'cpu',node_name:'',execution:'local'}];
    let inventorySource = 'filesystem', inventoryAuthority = { status: 'ready' }, revision = 1;
    let settings = { languages: ['*'], permissions: { ocr: true }, integrations: {} };
    let allocation = { cpu_workers: 4, ocr_workers: 1, gpu_workers: 1, paused: false };
    const remote = { enabled: true, nodes: [{ id: 'node-1', name: 'Studio GPU', resource: 'gpu', last_seen_seconds: 2, active: 1, status: 'busy' }], pending: 0, active: 1 };
    const longGpuAdmissionReason = 'GPU admission is paused because memory headroom is below the configured safety threshold and will retry when the next file starts.';
    let runningJob = null, queuedJobs = [], fileWorkers = [];
    const workerData = () => ({ edition: 'cuda', max_requested: 1000000, allocation, requested: allocation, effective: { cpu_workers: 4, ocr_workers: 1, gpu_workers: 1 }, active: { cpu_workers: fileWorkers.filter(worker => worker.resource !== 'gpu').length, ocr_workers: fileWorkers.filter(worker => worker.resource === 'cpu_ocr').length, gpu_workers: fileWorkers.filter(worker => worker.resource === 'gpu').length }, detected: { cpu_allowance_cores: 4, gpu_available: true, gpu_verification: 'verified', gpu_reason: 'CUDA runtime enabled' }, deployment: { max_cpu_workers: 32, max_ocr_workers: 16, max_gpu_workers: 4 }, admission: { gpu: { allowed: false, reason: longGpuAdmissionReason } }, workers: fileWorkers, remote });
    await page.route('**/api/**', async route => {
      const request = route.request(), endpoint = new URL(request.url()).pathname;
      let value;
      if(request.method()==='GET'&&endpoint==='/api/workers')workerReads++;
      if (request.method() !== 'GET') {
        const body = request.postDataJSON(); writes.push({ endpoint, body });
        if (endpoint === '/api/workers') allocation = body;
        if (endpoint === '/api/settings') settings = { ...settings, ...body };
      }
      if (endpoint === '/api/session') value = { csrf_token: 'disposable-ui-fixture' };
      else if (endpoint === '/api/status') value = { edition: 'cuda', version: 'fixture', roots, running: runningJob, queue: queuedJobs, history: jobs, logs: [{ time: now, level: 'INFO', message: 'Fixture operation completed' }], inventory: { updated: now, state_revision: revision, partial: true, source: inventorySource, authority: inventoryAuthority }, pipeline: { root_count: 2, files_inspected: 2, pending_ocr_files: 37, failed_files: 512, generated_srt_count: 11326, unreachable_roots: 0, inventory_truncated: true } };
      else if (endpoint === '/api/inventory') value = { items, updated: now, state_revision: revision, truncated: inventorySource !== 'bazarr', source: inventorySource, authority: inventoryAuthority };
      else if (endpoint === '/api/settings') value = settings;
      else if (endpoint === '/api/workers') value = workerData();
      else if (endpoint === '/api/nodes') value = remote;
      else if (endpoint === '/api/system') value = { edition: 'cuda', version: 'fixture', uptime_seconds: 120, resource_usage: { cpu: { effective_cores: 4 }, memory: { cgroup_current_bytes: 1300000000, cgroup_limit_bytes: 1610612736 } }, resources: {}, limits: {} };
      else if (endpoint === '/api/jobs') value = { message: 'Fixture job queued' };
      else if (endpoint === '/api/root-folders') value = { roots: [], mounts: [] };
      else if (endpoint === '/api/tasks') value = { tasks: [], paused: false };
      else throw new Error(`Unexpected fixture API ${request.method()} ${endpoint}`);
      await route.fulfill({ contentType: 'application/json', body: JSON.stringify(value) });
    });
    await page.goto(`http://127.0.0.1:${server.address().port}/#/overview`);
    await page.getByText('Service connected', { exact: true }).waitFor();
    await page.locator('#pipeline-ready').getByText('11,326', { exact: true }).waitFor();
    await page.waitForFunction(async () => await window.__workerReads() > 0);
    await page.locator('#pipeline-status').getByText(/Inspected/i).waitFor();
    assert.equal(writes.length, 0, 'initial page must not mutate the backend');
    await page.setViewportSize({ width: 1440, height: 960 });
    await assertViewport('Overview idle desktop');
    const overviewSummary = page.locator('.overview-summary');
    assert(await overviewSummary.isVisible(), 'Overview presents one compact pipeline and attention summary');
    assert.equal(await overviewSummary.getByText('37', { exact: true }).count(), 1, 'pending OCR appears once in attention counters');
    assert.equal(await overviewSummary.getByText('512', { exact: true }).count(), 1, 'failed-file count appears once');
    assert.equal(await overviewSummary.locator('[data-attention="paths"]').count(), 1, 'unreachable-root counter is in the summary');
    assert.doesNotMatch(await overviewSummary.innerText(), /Recent activity|Activity history/i);
    assert.equal(await page.locator('.activity-panel').count(), 0, 'Overview no longer duplicates the Activity table');
    assert(await page.locator('.quick-panel').isVisible(), 'quick conversion remains on Overview');
    assert(await page.locator('#overview-workers').isVisible(), 'compact local worker controls remain on Overview');
    for (const selector of ['.quick-panel', '.worker-allocation-panel']) {
      const box = await page.locator(selector).boundingBox();
      assert(box.y + box.height <= 960, `${selector} fits on the initial desktop screen`);
    }
    assert(await page.locator('#current-work-panel').isHidden(), 'idle current-work panel is hidden');
    assert(await page.locator('#active-workers-panel').isHidden(), 'empty active-workers panel is hidden');
    const diagnostics = page.locator('#overview-diagnostics');
    assert(await diagnostics.isVisible(), 'remote nodes and operation log remain in native diagnostics disclosure');
    assert.equal(await diagnostics.evaluate(el => el.open), false, 'diagnostics are collapsed by default');
    assert.doesNotMatch(await page.locator('#overview-workers').innerText(), /This node/i, 'local node is not duplicated beside local controls');
    if (output) await page.screenshot({ path: path.join(output, 'overview-idle-desktop.png'), style: '.toast { visibility: hidden; }' });
    for (const [width, height] of [[1100, 900], [768, 900]]) {
      await page.setViewportSize({ width, height });
      await assertViewport(`Overview idle ${width}x${height}`);
      const clipped = await page.locator('.overview-summary, .overview-library-meta, .overview-library-counts, .overview-attention').evaluateAll(nodes => nodes.filter(node => node.scrollWidth > node.clientWidth + 1).map(node => ({ selector: node.className, width: node.clientWidth, scrollWidth: node.scrollWidth })));
      assert.deepEqual(clipped, [], `library sync/counts and attention counters fit at ${width}px`);
      const syncStyle = await page.locator('#pipeline-status').evaluate(el => ({ whiteSpace: getComputedStyle(el).whiteSpace, textOverflow: getComputedStyle(el).textOverflow, height: el.clientHeight, lineHeight: parseFloat(getComputedStyle(el).lineHeight) }));
      assert(syncStyle.whiteSpace !== 'nowrap' && syncStyle.textOverflow !== 'ellipsis' || syncStyle.height > syncStyle.lineHeight * 1.5, `library sync metadata wraps instead of clipping at ${width}px: ${JSON.stringify(syncStyle)}`);
      for (const selector of ['#worker-cpu-count', '#worker-ocr-count', '#worker-gpu-count']) {
        const control = await page.locator(selector).boundingBox(), panel = await page.locator('.worker-allocation-panel').boundingBox();
        assert(control.x >= panel.x && control.x + control.width <= panel.x + panel.width + 1, `${selector} fits the worker panel at ${width}px`);
      }
      if (output) await page.screenshot({ path: path.join(output, `overview-idle-${width}.png`), style: '.toast { visibility: hidden; }' });
    }
    await page.setViewportSize({ width: 1440, height: 960 });

    runningJob = { id: 'running-overview', kind: 'extract', status: 'running', total: 3, processed: 1, current: { path: '/media/movies/Active movie.mkv', stage: 'Recognizing image subtitles' } };
    queuedJobs = [{ id: 'queued-overview', kind: 'inspect', status: 'queued', paths: ['/media/tv/Queued show.mkv'] }];
    fileWorkers = [{ id: 7, resource: 'cpu_ocr', path: '/media/movies/Active movie.mkv', status: 'Recognizing image subtitles', started: now - 20 }];
    await page.locator('#refresh-button').click();
    await page.locator('#current-work-panel').waitFor({ state: 'visible' });
    await page.locator('#active-workers-panel').waitFor({ state: 'visible' });
    assert.match(await page.locator('#current-work-panel').innerText(), /1 \/ 3 files|Active movie/);
    assert.match(await page.locator('#current-work-panel').innerText(), /1 queued job/);
    assert.match(await page.locator('#active-workers-panel').innerText(), /CPU OCR|Active movie/);
    assert.equal(await page.locator('#workers-body tr').count(), 1, 'Overview shows one active CPU OCR worker in the local pool');
    assert.match(await page.locator('#worker-pool-state').innerText(), /1 active/, 'local pool total counts the CPU OCR worker exactly once');
    assert.doesNotMatch(await page.locator('.overview-summary').innerText(), /37.*37/);
    if (output) await page.screenshot({ path: path.join(output, 'overview-running-desktop.png'), style: '.toast { visibility: hidden; }' });
    runningJob = null; queuedJobs = []; fileWorkers = [];
    await page.locator('#refresh-button').click();
    await page.locator('#current-work-panel').waitFor({ state: 'hidden' });
    await page.locator('#active-workers-panel').waitFor({ state: 'hidden' });

    await page.setViewportSize({ width: 1440, height: 640 });
    await assertViewport('short desktop');
    const headerBefore = await page.locator('.topbar').boundingBox(), footerBefore = await page.locator('.sidebar-bottom').boundingBox();
    await page.locator('#workspace-scroll').focus();
    await page.keyboard.press('PageDown');
    await page.waitForFunction(() => document.querySelector('#workspace-scroll').scrollTop > 0);
    await assertViewport('desktop after keyboard scrolling');
    assert.deepEqual(await page.locator('.topbar').boundingBox(), headerBefore, 'header stays fixed while the workspace scrolls');
    assert.deepEqual(await page.locator('.sidebar-bottom').boundingBox(), footerBefore, 'navigation footer stays fixed');
    await page.getByRole('button', { name: 'Library', exact: true }).click();
    assert.equal(await page.locator('#workspace-scroll').evaluate(el => el.scrollTop), 0, 'navigation resets the workspace scroll position');
    await page.getByRole('button', { name: 'Overview', exact: true }).click();
    await page.setViewportSize({ width: 1440, height: 1000 });
    await assertToggleGlyph();
    await page.locator('#sidebar-collapse').click();
    assert.equal(await page.locator('#sidebar-collapse').getAttribute('aria-expanded'), 'false');
    assert(await page.locator('#sidebar-collapse').isVisible(), 'collapsed navigation keeps its top-bar toggle');
    await assertToggleGlyph();
    if (output) await page.screenshot({ path: path.join(output, 'sidebar-collapsed.png'), style: '.toast { visibility: hidden; }' });
    await page.reload();
    await page.locator('#pipeline-ready').getByText('11,326', { exact: true }).waitFor();
    assert.equal(await page.locator('#sidebar-collapse').getAttribute('aria-expanded'), 'false', 'collapsed state survives reload');
    await assertToggleGlyph();
    await page.locator('#sidebar-collapse').click();
    await assertToggleGlyph();
    await page.locator('[data-open-languages="quick"]').last().click();
    await page.locator('#language-list input[value="en"]').check();
    assert.equal(await page.locator('.all-option input').isChecked(), false, 'specific selection clears All visibly');
    assert.equal(await page.locator('#language-count').textContent(), '1 language selected');
    await page.locator('#apply-languages').click();
    assert.match(await page.locator('#quick-languages').textContent(), /English/);
    assert.equal(await page.locator('#settings-languages').textContent(), 'All languages', 'one-job choice must not edit defaults');
    await page.locator('#quick-root').selectOption('movies');
    await page.locator('#quick-form').getByRole('button', { name: 'Review conversion' }).click();
    assert.equal(writes.length, 0, 'scope review must not start a job');
    assert.match(await page.locator('#conversion-scope').innerText(), /Movies/);
    await page.locator('#confirm-conversion').click();
    await page.locator('#conversion-modal').waitFor({ state: 'hidden' });
    assert.deepEqual(writes.at(-1).body.root_ids, ['movies']);
    assert.deepEqual(writes.at(-1).body.languages, ['en']);
    await page.locator('[data-attention="failed"]').click();
    assert.match(await page.locator('#inventory-scope-note').innerText(), /512 failed files across/);
    assert.match(await page.locator('#inventory-body').innerText(), /invalid timestamp/);
    await page.locator('#inventory-body .row-action').first().click();
    assert.match(await page.locator('#track-drawer-failure').innerText(), /invalid timestamp/);
    assert.equal(await page.locator('#track-drawer-list input:checked').count(), 1, 'track defaults respect selected languages');
    assert.equal(await page.locator('#convert-selected-tracks').textContent(), 'Convert 1 track');
    await page.getByRole('button', { name: 'Close track details' }).click();
    await page.getByRole('button', { name: 'Activity', exact: true }).click();
    assert.doesNotMatch(await page.locator('#jobs-list').innerText(), /State backup/);
    await page.locator('#activity-kind').selectOption('all');
    assert.match(await page.locator('#jobs-list').innerText(), /State backup/);
    const executorRow=page.locator('#jobs-list .job-row').filter({ hasText: 'executors-fixture.mkv' });
    assert.equal(await executorRow.count(),1,'persisted activity includes the completed multiworker job');
    await executorRow.locator('[data-view-job]').click();
    const executorCard=page.locator('#job-drawer-content .job-card');
    await executorCard.waitFor();
    const closeJobDetails=page.getByRole('button', { name: 'Close job details' });
    assert.equal(await closeJobDetails.evaluate(button=>document.activeElement===button),true,'opening the job dialog moves focus to Close');
    await page.keyboard.press('Tab');
    assert.equal(await executorCard.locator('.job-executors-more summary').evaluate(summary=>document.activeElement===summary),true,'Tab reaches the worker disclosure inside the dialog');
    await page.keyboard.press('Escape');
    await page.locator('#job-drawer-backdrop').waitFor({ state: 'hidden' });
    assert.equal(await executorRow.locator('[data-view-job]').evaluate(button=>document.activeElement===button),true,'Escape returns focus to the invoking row action');
    await executorRow.locator('[data-view-job]').click();
    await executorCard.waitFor();
    if (output) await page.screenshot({ path: path.join(output, 'job-details-desktop.png'), style: '.toast { visibility: hidden; }' });
    assert.deepEqual(await executorCard.locator('.job-executors > li:not(.job-executors-more):not(.job-executors-truncated)').allTextContents(),['CPU · Local','GPU OCR · Studio GPU (remote)','CPU OCR · Local','GPU OCR · Studio GPU (remote)']);
    assert.equal(await executorCard.locator('.job-executors-more summary').textContent(),'2 more workers');
    await executorCard.locator('.job-executors-more summary').click();
    jobs[1].summary.converted++;
    await page.evaluate(()=>document.dispatchEvent(new Event('visibilitychange')));
    await executorCard.locator('.job-summary').getByText('7 converted', { exact: true }).waitFor();
    assert.equal(await executorCard.locator('.job-executors-more details').evaluate(details=>details.open),true,'worker disclosure remains open after a status update');
    assert.deepEqual(await executorCard.locator('.job-executors-more ul li').allTextContents(),['CPU · Local','GPU OCR · Studio GPU (remote)']);
    assert.match(await executorCard.locator('.job-executors').innerText(),/Additional workers not shown/);
    await page.getByRole('button', { name: 'Close job details' }).click();
    const legacyRow=page.locator('#jobs-list .job-row').filter({ hasText: 'Example movie.mkv' });
    assert.equal(await legacyRow.locator('.job-row-workers').innerText(),'—','legacy history does not invent executor hardware');
    await page.getByRole('button', { name: 'Settings', exact: true }).click();
    assert.equal(await page.locator('.settings-tab.active').getAttribute('aria-current'), 'page');
    await page.locator('[data-open-languages="settings"]').click();
    await page.locator('#language-list input[value="fr"]').check();
    await page.locator('#apply-languages').click();
    assert.match(await page.locator('#settings-languages').textContent(), /French/);
    assert.match(await page.locator('#quick-languages').textContent(), /English/, 'defaults must not overwrite one-job scope');
    await page.locator('#save-settings').click();
    await page.getByText('Settings saved.', { exact: true }).waitFor();
    assert.deepEqual(writes.at(-1).body.languages, ['fr']);
    await page.getByRole('button', { name: 'Overview', exact: true }).click();
    const diagnosticsSummary = page.locator('#overview-diagnostics > summary');
    const diagnosticNodeCount = page.locator('#worker-nodes-summary');
    assert(await diagnosticsSummary.isVisible(), 'remote worker disclosure has a visible native summary');
    assert.equal(await diagnostics.evaluate(el => el.open), false);
    await diagnosticsSummary.focus();
    await page.keyboard.press('Enter');
    assert.equal(await diagnostics.evaluate(el => el.open), true, 'keyboard can expand Overview diagnostics');
    await diagnostics.locator('#worker-nodes').getByText('Studio GPU', { exact: true }).waitFor();
    assert.doesNotMatch(await diagnostics.innerText(), /This node/i, 'remote diagnostics omit a duplicate local node');
    assert.match(await diagnosticNodeCount.innerText(), /1/);
    if (output) await page.screenshot({ path: path.join(output, 'overview-diagnostics-open.png'), style: '.toast { visibility: hidden; }' });
    await page.keyboard.press('Enter');
    assert.equal(await diagnostics.evaluate(el => el.open), false, 'keyboard can collapse Overview diagnostics');
    await page.locator('#overview-workers #worker-cpu-count').fill('1000');
    await page.locator('#overview-workers #save-worker-allocation').click();
    await page.getByText('Worker allocation saved. It applies to new files.', { exact: true }).waitFor();
    assert.equal(writes.at(-1).body.cpu_workers, 1000);
    const gpuAdmission = page.locator('#overview-workers #worker-gpu-capacity');
    assert.match(await gpuAdmission.textContent(), /memory headroom|memory headroom/i);
    assert.match(await gpuAdmission.textContent(), /configured safety threshold/);
    const admissionLayout = await gpuAdmission.evaluate(el => ({ height: el.clientHeight, lineHeight: parseFloat(getComputedStyle(el).lineHeight), whiteSpace: getComputedStyle(el).whiteSpace, text: el.innerText }));
    assert(admissionLayout.height > admissionLayout.lineHeight * 1.5, 'long GPU admission reason wraps into visible lines');
    assert.match(admissionLayout.text, /will retry when the next file starts/, 'GPU admission reason retains its full guidance');
    await page.locator('#overview-workers #worker-cpu-count').fill('999');
    const readsBeforeOverviewRefresh=workerReads;
    await page.locator('#refresh-button').click();
    await page.waitForFunction(async previous=>await window.__workerReads()>previous,readsBeforeOverviewRefresh);
    assert.equal(await page.locator('#overview-workers #worker-cpu-count').inputValue(),'999','worker polling preserves an unsaved user edit');
    assert.match(await page.locator('#worker-allocation-note').textContent(), /Unsaved/);
    assert(workerReads>readsBeforeOverviewRefresh,'overview refresh reloads persisted worker allocation');
    await page.locator('#overview-workers #worker-cpu-count').fill('1000');
    await page.locator('#overview-workers #save-worker-allocation').click();
    await page.getByText('Worker allocation saved. It applies to new files.', { exact: true }).waitFor();
    await page.evaluate(()=>{location.hash='#/settings/workers';});
    await page.waitForFunction(()=>location.hash.includes('/overview'));
    await page.locator('#overview-workers #worker-cpu-count').waitFor();
    assert.equal(await page.locator('#overview-workers #worker-cpu-count').inputValue(),'1000','legacy workers route redirects to Overview allocation controls');
    if (output) await page.screenshot({ path: path.join(output, 'workers-desktop.png'), fullPage: true, style: '.toast { visibility: hidden; }' });
    if (output) await page.screenshot({ path: path.join(output, 'overview-desktop.png'), fullPage: true, style: '.toast { visibility: hidden; }' });
    await page.setViewportSize({ width: 390, height: 844 });
    assert.equal(await page.locator('#sidebar').evaluate(el => el.inert), true);
    const quick = await page.locator('.quick-panel').boundingBox(), workers = await page.locator('.worker-allocation-panel').boundingBox();
    assert(quick.y < workers.y, 'mobile quick conversion precedes compact local worker controls');
    assert.equal(await page.locator('.activity-panel').count(), 0, 'mobile Overview does not duplicate recent Activity');
    assert.equal(await page.evaluate(() => document.body.scrollWidth), 390, 'mobile page must not overflow');
    if (output) await page.screenshot({ path: path.join(output, 'overview-mobile.png'), fullPage: true, style: '.toast { visibility: hidden; }' });
    await page.locator('#mobile-menu').click();
    await page.getByRole('button', { name: 'Library', exact: true }).click();
    assert.equal(await page.locator('#sidebar').evaluate(el => el.inert), true);
    assert.equal(await page.locator('#inventory-page-size').inputValue(), 'auto', 'Library starts with viewport-fitting Auto pagination');
    const table = await page.locator('.inventory-table').boundingBox();
    assert(table.width <= 390, 'mobile inventory uses cards without horizontal table overflow');
    assert(await page.locator('#inventory-body .row-action').first().isVisible());
    if (output) await page.screenshot({ path: path.join(output, 'library-mobile.png'), fullPage: true, style: '.toast { visibility: hidden; }' });
    await page.locator('#inventory-body tr').first().focus();
    await page.keyboard.press('Enter');
    await page.locator('#track-drawer-backdrop').waitFor({ state: 'visible' });
    await page.keyboard.press('Escape');
    await page.locator('#track-drawer-backdrop').waitFor({ state: 'hidden' });
    assert.equal(await page.locator('#inventory-body tr').first().evaluate(el => document.activeElement === el), true, 'drawer restores keyboard focus');
    for (const width of [320, 768, 1024, 1920]) {
      await page.setViewportSize({ width, height: 900 });
      assert.equal(await page.evaluate(() => document.body.scrollWidth), width, `library fits ${width}px viewport`);
      await assertViewport(`library ${width}px`);
      await assertTablePane('library', width, 900);
    }
    await page.locator('#inventory-page-size').selectOption('25');
    assert.equal(await page.locator('#inventory-page-size').inputValue(), '25');
    await page.locator('#inventory-page-size').selectOption('auto');
    // Bazarr requirements take precedence over historical failures and wildcard tracks.
    const target = (language, forced = false, hi = false) => ({ language, forced, hi });
    const wanted = [target('en'), target('fr'), target('ar')];
    const covered = {
      name: 'Covered movie.mkv', path: '/media/movies/Covered movie.mkv', root_id: 'movies', type: 'video',
      wanted_targets: wanted, missing_targets: [], missing_srt: false,
      coverage: { status: 'covered', reason: 'Valid SRT already covers every wanted language', existing_targets: wanted, missing_targets: [] },
      tracks: [{ id: 3, language: 'en', codec: 'PGS', image: true }, ...Array.from({ length: 27 }, (_, i) => ({ id: i + 4, language: 'und', codec: 'PGS', image: true }))],
      processing: { failed: true, pending_ocr: true, last_error: 'Historical extraction failure' }
    };
    const needed = {
      name: 'Missing French.mkv', path: '/media/movies/Missing French.mkv', root_id: 'movies', type: 'video',
      wanted_targets: wanted, missing_targets: [target('fr')], missing_srt: true,
      coverage: { status: 'needs_conversion', reason: 'French SRT is missing', existing_targets: [target('en'), target('ar')], missing_targets: [target('fr')] },
      tracks: [{ id: 2, language: 'en', codec: 'SRT' }, { id: 3, language: 'fr', codec: 'ASS' }, { id: 4, language: 'fr', codec: 'ASS', forced: true }, { id: 5, language: 'und', codec: 'PGS', image: true }]
    };
    items = [covered, needed]; inventorySource = 'bazarr'; revision++;
    await page.setViewportSize({ width: 1440, height: 1000 });
    await page.locator('#refresh-button').click();
    await page.locator('#inventory-source').getByText('Bazarr', { exact: true }).waitFor();
    await page.locator('#inventory-filter').selectOption('covered');
    assert.equal(await page.locator('#inventory-body tr').count(), 1);
    assert.match(await page.locator('#inventory-body').innerText(), /Already covered/);
    assert.doesNotMatch(await page.locator('#inventory-body').innerText(), /Historical extraction failure/);
    await page.locator('#inventory-body .row-action').first().click();
    assert.equal(await page.locator('#track-drawer-list input:checked').count(), 0);
    assert.match(await page.locator('#track-drawer-hint').innerText(), /No extraction needed/);
    assert(await page.locator('#convert-selected-tracks').isDisabled());
    assert(await page.locator('#track-drawer-list input').first().isDisabled());
    await page.locator('#force-covered-tracks').check();
    await page.locator('#track-drawer-list input').first().check();
    await page.locator('#convert-selected-tracks').click();
    await page.locator('#track-drawer-backdrop').waitFor({ state: 'hidden' });
    assert.equal(writes.at(-1).body.force, true);
    assert.deepEqual(writes.at(-1).body.track_ids, [3], 'force targets one deliberate track');
    await page.locator('#inventory-filter').selectOption('needed');
    await page.locator('#inventory-body .row-action').first().click();
    // One-job English selection from earlier must be reset to profile requirements.
    assert.equal(await page.locator('#track-drawer-list input:checked').count(), 0);
    await page.getByRole('button', { name: 'Close track details' }).click();
    await page.getByRole('button', { name: 'Overview', exact: true }).click();
    await page.locator('[data-open-languages="quick"]').last().click();
    await page.locator('.all-option input').check();
    await page.locator('#apply-languages').click();
    await page.getByRole('button', { name: 'Library', exact: true }).click();
    await page.locator('#inventory-body .row-action').first().click();
    assert.equal(await page.locator('#track-drawer-list input:checked').count(), 1, 'only missing full French target selected');
    assert(await page.locator('#track-drawer-list input').nth(1).isChecked());
    assert(!(await page.locator('#track-drawer-list input').nth(2).isChecked()), 'forced French does not cover full French');
    assert(!(await page.locator('#track-drawer-list input').nth(3).isChecked()), 'untagged track not selected automatically');
    await page.getByRole('button', { name: 'Close track details' }).click();
    await page.locator('#inventory-filter').selectOption('all');
    if (output) await page.screenshot({ path: path.join(output, 'bazarr-library-desktop.png'), fullPage: true, style: '.toast { visibility: hidden; }' });
    inventoryAuthority = { status: 'refresh_pending' }; revision++;
    await page.locator('#refresh-button').click();
    await page.locator('#inventory-authority').getByText(/indexing or inventory refresh is still pending/).waitFor();
    assert(await page.locator('[data-action="scan"]').first().isDisabled());
    assert(await page.locator('[data-action="inspect"]').first().isEnabled(), 'sync remains available');
    inventoryAuthority = { status: 'ready' }; revision++;
    needed.missing_targets = []; needed.missing_srt = false;
    needed.coverage = { status: 'covered', reason: 'Bazarr confirms the new SRT', existing_targets: wanted, missing_targets: [] };
    await page.locator('#refresh-button').click();
    await page.locator('#inventory-filter').selectOption('covered');
    await page.waitForFunction(() => document.querySelectorAll('#inventory-body tr').length === 2);
    assert(await page.locator('[data-action="scan"]').first().isEnabled());
    await page.getByRole('button', { name: 'Overview', exact: true }).click();
    inventoryAuthority = { status: 'refresh_pending' }; revision++;
    await page.locator('#refresh-button').click();
    await page.waitForFunction(() => /Bazarr/i.test(document.querySelector('#overview-inventory-notice').innerText) && /pending|indexing|refresh/i.test(document.querySelector('#overview-inventory-notice').innerText));
    const quickBazarrStatus = await page.locator('#overview-inventory-notice').innerText();
    assert.match(quickBazarrStatus, /Bazarr/i);
    assert.match(quickBazarrStatus, /pending|indexing|refresh/i, 'quick conversion explains when Bazarr inventory is not ready');
    inventoryAuthority = { status: 'ready' }; revision++;
    await page.locator('#refresh-button').click();
    await page.waitForFunction(() => !/pending|indexing/i.test(document.querySelector('#overview-inventory-notice').innerText));
    await page.locator('#quick-form').getByRole('button', { name: 'Review conversion' }).click();
    assert.match(await page.locator('#conversion-scope').innerText(), /Bazarr profile/);
    await page.locator('#confirm-conversion').click();
    await page.locator('#conversion-modal').waitFor({ state: 'hidden' });
    assert.equal(writes.at(-1).body.include_ocr, true, 'manual conversion requests available OCR slots');
    // A timed-out rescan is uncertain, while the actual conversion is complete.
    const pendingNotice = { provider: 'bazarr', ok: false, pending: true, uncertain: true, message: 'Bazarr response timed out; scan completion not yet confirmed' };
    jobs.unshift({ id: 'index-pending', kind: 'extract', status: 'completed', created: now, finished: now, summary: { converted: 1 }, notifications: [pendingNotice] });
    await page.getByRole('button', { name: 'Activity', exact: true }).click();
    await page.locator('#activity-kind').selectOption('all');
    await page.locator('#refresh-button').click();
    const pendingRow = page.locator('#jobs-list .job-row').filter({ hasText: 'Converted · indexing pending' });
    await pendingRow.locator('[data-view-job]').click();
    const pendingCard = page.locator('#job-drawer-content .job-card');
    await pendingCard.getByText('Converted · indexing pending', { exact: true }).waitFor();
    assert.equal(await pendingCard.locator('.job-error').count(), 0, 'pending confirmation is not a reported conversion error');
    assert(await pendingCard.locator('.result-pending').isVisible());
    const writesBeforeCheck = writes.length;
    await pendingCard.getByRole('button', { name: 'Check Bazarr indexing' }).click();
    assert.equal(writes.length, writesBeforeCheck + 1);
    assert.equal(writes.at(-1).endpoint, '/api/jobs');
    assert.equal(writes.at(-1).body.kind, 'inspect', 'verification queues an inventory read, not another extraction or rescan');
    for (const width of [1440, 320]) {
      await page.setViewportSize({ width, height: 640 });
      await pendingCard.scrollIntoViewIfNeeded();
      await assertViewport(`pending indexing ${width}px`);
      assert(await pendingCard.getByRole('button', { name: 'Check Bazarr indexing' }).isVisible());
      if (output) await page.screenshot({ path: path.join(output, `bazarr-indexing-pending-${width}.png`), style: '.toast { visibility: hidden; }' });
    }
    pendingNotice.ok = true; pendingNotice.pending = false; pendingNotice.uncertain = false;
    pendingNotice.message = 'Published SRT files confirmed by a later Bazarr inventory sync';
    await page.evaluate(()=>document.dispatchEvent(new Event('visibilitychange')));
    await pendingCard.getByText(pendingNotice.message, { exact: true }).waitFor();
    assert.equal(await pendingCard.locator('.result-pending, .job-error').count(), 0);
    await pendingCard.getByText('Completed', { exact: true }).waitFor();
    await page.getByRole('button', { name: 'Close job details' }).click();
    // Long activity and native controls must remain usable inside the fixed shell.
    await page.setViewportSize({ width: 1440, height: 1000 });
    jobs.push(...Array.from({ length: 20 }, (_, i) => ({ id: `history-${i}`, kind: 'inspect', status: 'completed', created: now - i * 60, summary: { inspected: 1 } })));
    await page.getByRole('button', { name: 'Activity', exact: true }).click();
    await page.locator('#activity-kind').selectOption('all');
    await page.locator('#refresh-button').click();
    await page.waitForFunction(() => document.querySelector('#jobs-range').textContent.includes('of 24 jobs'));
    await page.locator('#jobs-page-size').selectOption('10');
    assert.match(await page.locator('#jobs-range').textContent(), /^1–10 of/);
    assert.equal(await page.locator('#jobs-list .job-row').count(),10);
    await page.locator('#jobs-next').click();
    assert.match(await page.locator('#jobs-page-label').textContent(), /^Page 2 of/);
    await page.locator('#activity-status').selectOption('issues');
    assert.equal(await page.locator('#jobs-page-label').textContent(),'Page 1 of 1','changing filters resets pagination');
    await page.locator('#activity-status').selectOption('all');
    await page.locator('#jobs-next').click();
    for (const [width, height] of [[1920, 1080], [1024, 600], [390, 600], [320, 480]]) {
      await page.setViewportSize({ width, height });
      await assertViewport(`activity resized to ${width}x${height}`);
      await assertTablePane('activity', width, height);
      const header = await page.locator('.topbar').boundingBox();
      await page.locator('#jobs-list .job-row').last().scrollIntoViewIfNeeded();
      await page.locator('.jobs-table-wrap').evaluate(el=>{el.scrollTop=el.scrollHeight;});
      await assertViewport(`long activity ${width}x${height}`);
      assert.deepEqual(await page.locator('.topbar').boundingBox(), header, 'scrolling long activity keeps the header visible');
      const last = await page.locator('#jobs-list .job-row').last().boundingBox();
      const tablePane = await page.locator('.jobs-table-wrap').boundingBox();
      assert(last.y + last.height <= tablePane.y + tablePane.height + 1, `the final activity row remains reachable inside its scrolling pane: ${JSON.stringify({last,tablePane,width,height})}`);
      if (width===320) {
        assert(await page.locator('#jobs-page-size').isVisible(),'pagination controls remain visible at 320px');
        assert(await page.locator('#jobs-previous').isVisible());
        assert.equal(await page.locator('#jobs-list').evaluate(el=>el.scrollWidth<=el.clientWidth),true,'activity table fits without horizontal overflow at 320px');
      }
    }
    await page.setViewportSize({ width: 1440, height: 640 });
    await page.locator('#activity-status').focus();
    await page.keyboard.press('Home');
    await page.keyboard.press('ArrowDown');
    assert.equal(await page.locator('#activity-status').inputValue(), 'running', 'select retains native keyboard selection');
    await page.locator('#activity-status').selectOption('all');
    const selectStyles = await page.locator('#activity-status').evaluate(select => {
      const css = getComputedStyle(select), option = getComputedStyle(select.options[0]);
      return { rightPadding: parseFloat(css.paddingRight), arrow: css.backgroundImage, optionBackground: option.backgroundColor, background: css.backgroundColor };
    });
    assert(selectStyles.rightPadding >= 32 && selectStyles.arrow !== 'none', 'select text has reserved space for a visible arrow');
    assert.notEqual(selectStyles.optionBackground, 'rgb(255, 255, 255)', 'select options use the dark palette');
    await page.locator('#jobs-page-size').selectOption('auto');
    await page.locator('#jobs-page-size').selectOption('25');
    assert.equal(await page.locator('#jobs-list .job-row').count(),24,'manual page size shows all 24 fixture rows');
    await page.locator('#jobs-page-size').selectOption('auto');
    await page.setViewportSize({ width: 1440, height: 960 });
    if (output) await page.screenshot({ path: path.join(output, 'activity-table-desktop.png'), style: '.toast { visibility: hidden; }' });
    // Stress a long Library table: only its rows pane scrolls; filters and paging stay in view.
    items = Array.from({ length: 60 }, (_, i) => ({ name: `Viewport fixture ${String(i + 1).padStart(2, '0')}.mkv`, path: `/media/movies/viewport-${i + 1}.mkv`, root_id: 'movies', type: 'video', tracks: [], processing: {} }));
    inventorySource = 'filesystem'; inventoryAuthority = { status: 'ready' }; revision++;
    await page.getByRole('button', { name: 'Library', exact: true }).click();
    await page.locator('#refresh-button').click();
    await page.locator('#inventory-search').fill('');
    await page.locator('#inventory-root').selectOption('');
    await page.locator('#inventory-filter').selectOption('all');
    await page.waitForFunction(() => document.querySelector('#inventory-range').textContent.includes('of 60'));
    await page.locator('#inventory-page-size').selectOption('25');
    for (const [width, height] of [[1920, 960], [1440, 960], [768, 640], [390, 480], [320, 480]]) {
      await page.setViewportSize({ width, height });
      await assertViewport(`long library ${width}x${height}`);
      if (output) await page.screenshot({ path: path.join(output, `library-manual-${width}x${height}.png`), style: '.toast { visibility: hidden; }' });
      await assertTablePane('library', width, height);
      assert(await page.locator('.inventory-toolbar').isVisible(), `Library filters stay visible at ${width}x${height}`);
      assert(await page.locator('.inventory-pagination').isVisible(), `Library paging stays visible at ${width}x${height}`);
      assert.equal(await page.locator('#inventory-body tr').count(), 25, 'manual page size keeps exactly 25 rows');
    }
    assert.deepEqual(errors, [], 'browser must have no uncaught errors');
    console.log('PASS Subforge fixed viewport/internal scrolling, visible collapsed toggle, native selects, desktop/mobile UI, language isolation, scope review, failure triage, workers, remote nodes, executor metadata, keyboard, Bazarr coverage/authority, and pending indexing confirmation regressions');
    await context.close();
  } finally {
    if (browser) await browser.close();
    await new Promise(resolve => server.close(resolve));
  }
}
main().catch(error => { console.error(error); process.exitCode = 1; });
