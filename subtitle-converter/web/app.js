(() => {
  'use strict';
  const $ = (selector, root = document) => root.querySelector(selector);
  const $$ = (selector, root = document) => [...root.querySelectorAll(selector)];
  const languages = [
    ['en','English'],['fr','French'],['ar','Arabic'],['de','German'],['es','Spanish'],['it','Italian'],['pt','Portuguese'],['ja','Japanese'],['zh','Chinese'],['ko','Korean'],['ru','Russian'],['nl','Dutch'],['tr','Turkish'],['und','Unknown / untagged'],
    ['af','Afrikaans'],['sq','Albanian'],['am','Amharic'],['hy','Armenian'],['az','Azerbaijani'],['eu','Basque'],['bn','Bengali'],['bs','Bosnian'],['bg','Bulgarian'],['my','Burmese'],['ca','Catalan'],['hr','Croatian'],['cs','Czech'],['da','Danish'],['et','Estonian'],['fi','Finnish'],['el','Greek'],['he','Hebrew'],['hi','Hindi'],['hu','Hungarian'],['id','Indonesian'],['ga','Irish'],['is','Icelandic'],['kk','Kazakh'],['km','Khmer'],['lo','Lao'],['lv','Latvian'],['lt','Lithuanian'],['mk','Macedonian'],['ms','Malay'],['no','Norwegian'],['fa','Persian'],['pl','Polish'],['ro','Romanian'],['sr','Serbian'],['sk','Slovak'],['sl','Slovenian'],['sw','Swahili'],['sv','Swedish'],['th','Thai'],['uk','Ukrainian'],['ur','Urdu'],['vi','Vietnamese']
  ];
  const state = { csrf: '', status: null, settings: null, selectedLanguages: ['*'], draftLanguages: ['*'], languageTarget: 'quick', inventory: [], selectedFile: null, currentPage: 'overview', lastToast: 0, pollBusy: false };
  state.jellyfinLibraries=[];
  state.sidebarCollapsed=false;
  state.rootFolders={roots:[],mounts:[]};
  state.folderRequest=0;
  state.inventoryPage=1;
  state.inventoryPageSize=25;
  state.inventoryTruncated=false;
  state.inventoryUpdated=null;
  state.inventoryLoaded=false;
  state.inventoryBusy=false;
  state.inventoryRestoreRoot=null;
  state.collapsedPanels=new Set();
  state.errorViews=new Map();
  state.tasks=null;
  state.systemInfo=null;
  state.systemBusy=false;
  state.systemReceivedAt=0;
  state.systemSampleHealthy=false;
  state.workersInfo=null;
  state.workersBusy=false;
  state.workersDirty=false;
  state.workersSaving=false;
  state.tasksDirty=false;
  state.tasksBusy=false;
  state.tasksLoadedAt=0;
  state.taskRequests=new Set();
  const integrationDefaults = () => ({enabled:false,url:'',api_key:'',api_key_set:false,path_mappings:[]});
  const setText = (el, value) => { if (el) el.textContent = value == null ? '' : String(value); };
  const escStatus = value => String(value || '').toLowerCase().replace(/[^a-z0-9_-]/g, '');
  const fmtDate = value => { if (!value) return '—'; const parsed=typeof value==='number'?(value<1e12?value*1000:value):value; const date = new Date(parsed); return Number.isNaN(date.valueOf()) ? String(value) : new Intl.DateTimeFormat(undefined,{month:'short',day:'numeric',hour:'numeric',minute:'2-digit'}).format(date); };
  const currentLabel = current => { if (!current) return ''; if (typeof current!=='object') return String(current); const detail=shortPath(current.path || current.output || ''); return [current.status || 'Processing',detail].filter(Boolean).join(' · '); };
  const shortPath = value => { const path = String(value || ''); return path.length > 80 ? `…${path.slice(-77)}` : path; };
  const notify = (message, kind = 'info') => { const toast = $('#toast'); setText(toast, message); toast.className = `toast ${kind}`; clearTimeout(state.lastToast); state.lastToast = setTimeout(() => toast.classList.add('hidden'), 4200); };
  const api = async (path, options = {}) => {
    const headers = new Headers(options.headers || {});
    if (options.body && !(options.body instanceof FormData)) headers.set('Content-Type','application/json');
    if (state.csrf && options.method && options.method !== 'GET') headers.set('X-CSRF-Token', state.csrf);
    const response = await fetch(path,{...options,headers,credentials:'same-origin'});
    const contentType = response.headers.get('content-type') || '';
    const payload = contentType.includes('json') ? await response.json() : await response.text();
    if (!response.ok) throw new Error(typeof payload === 'object' ? payload.error || payload.message || `Request failed (${response.status})` : payload || `Request failed (${response.status})`);
    return payload;
  };
  const jobList = () => { const status = state.status || {}; return [status.running,...(status.queue || []),...(status.history || [])].filter(Boolean); };
  const langLabel = code => { const base = String(code || '').split('-')[0].toLowerCase(); return languages.find(item => item[0] === base)?.[1] || String(code || '').toUpperCase(); };
  const isAll = list => !Array.isArray(list) || list.includes('*');
  const languageDescription = list => isAll(list) ? 'All languages' : `${list.length} ${list.length === 1 ? 'language' : 'languages'} · ${list.slice(0,3).map(langLabel).join(', ')}${list.length > 3 ? ` +${list.length - 3}` : ''}`;
  const disclosureIcon = () => {
    const svg=document.createElementNS('http://www.w3.org/2000/svg','svg');svg.setAttribute('viewBox','0 0 24 24');svg.setAttribute('aria-hidden','true');
    const path=document.createElementNS(svg.namespaceURI,'path');path.setAttribute('d','m6 9 6 6 6-6');svg.append(path);return svg;
  };
  const makePanelCollapsible = card => {
    const key=card.dataset.collapse;if(!key||$('.collapse-toggle',card))return;
    const header=$(':scope > .panel-heading,:scope > .inventory-toolbar',card);if(!header)return;
    const title=$('h2',header)?.textContent || 'panel',content=document.createElement('div');content.className='collapsible-content';content.id=`panel-content-${key}`;
    while(header.nextSibling)content.append(header.nextSibling);card.append(content);
    const button=document.createElement('button');button.type='button';button.className='icon-button collapse-toggle';button.setAttribute('aria-controls',content.id);button.append(disclosureIcon());
    const sync=()=>{const collapsed=state.collapsedPanels.has(key);content.hidden=collapsed;card.classList.toggle('is-collapsed',collapsed);button.setAttribute('aria-expanded',String(!collapsed));button.setAttribute('aria-label',`${collapsed?'Expand':'Collapse'} ${title}`);button.title=`${collapsed?'Expand':'Collapse'} ${title}`;};
    button.addEventListener('click',()=>{if(state.collapsedPanels.has(key))state.collapsedPanels.delete(key);else state.collapsedPanels.add(key);sync();try{localStorage.setItem('subforge.collapsed-panels',JSON.stringify([...state.collapsedPanels]));}catch{}});
    if(header.classList.contains('inventory-toolbar')){const heading=header.firstElementChild;heading.classList.add('inventory-heading-title');heading.append(button);}
    else{const actions=document.createElement('div');actions.className='panel-heading-actions';while(header.children.length>1)actions.append(header.children[1]);actions.append(button);header.append(actions);}
    sync();
  };
  const errorViewKey = (job,compact) => `${compact?'overview':'activity'}:${job.id || `${job.kind}:${job.created}`}`;
  const groupedErrors = job => {
    const groups=new Map(),providers={sonarr:'Sonarr',radarr:'Radarr',jellyfin:'Jellyfin',bazarr:'Bazarr'};
    const add=(source,item)=>{const message=typeof item.message==='string'?item.message.trim():'';if(!message)return;const provider=typeof item.provider==='string'&&Object.hasOwn(providers,item.provider)?item.provider:null,key=JSON.stringify([source,provider,message]);const group=groups.get(key)||{source,provider,message,label:provider?providers[provider]:'Integration',count:0,examples:[]};group.count+=Number.isSafeInteger(item.count)&&item.count>0?item.count:1;group.examples=(item.examples||[]).filter(name=>typeof name==='string').slice(0,3);groups.set(key,group);};
    const duplicateRefresh=job.kind==='refresh'&&(job.results||[]).some(item=>item?.ok===false&&item.message===job.error);
    if(job.error&&!duplicateRefresh)add('job',{message:job.error});
    for(const item of Array.isArray(job.file_errors)?job.file_errors:[])add('conversion',item);
    if(Number(job.other_file_errors)>0)add('conversion',{message:'Additional file failures',count:job.other_file_errors});
    const reported=[...groups.values()].filter(group=>group.source==='conversion').reduce((sum,group)=>sum+group.count,0),missing=Number(job.summary?.failed||0)-reported;
    if(missing>0)add('conversion',{message:'Detailed causes are unavailable for this older job. Run a new scan to collect file diagnostics.',count:missing});
    for(const item of Array.isArray(job.notifications)?job.notifications:[])if(item?.ok===false)add('notification',item);
    for(const item of Array.isArray(job.results)?job.results:[])if(item?.ok===false)add('result',item);
    return [...groups.values()];
  };
  const renderErrors = (job,compact) => {
    const groups=groupedErrors(job);if(!groups.length)return null;
    const key=errorViewKey(job,compact),saved=state.errorViews.get(key)||{open:false,scroll:0},count=groups.reduce((sum,group)=>sum+group.count,0);
    const details=document.createElement('details');details.className='job-error';details.dataset.errorKey=key;details.open=saved.open;
    const summary=document.createElement('summary');summary.dataset.jobFocus=`${key}:errors`;const copy=document.createElement('span');copy.className='job-error-summary-copy';const title=document.createElement('strong');title.textContent=`${count.toLocaleString()} reported error${count===1?'':'s'} · ${groups.length} group${groups.length===1?'':'s'}`;
    const preview=document.createElement('span');preview.className='job-error-preview';preview.textContent=`${groups[0].source==='job'?'Job':groups[0].source==='conversion'?'Media file':groups[0].label}: ${groups[0].message}`;copy.append(title,preview);summary.append(copy,disclosureIcon());details.append(summary);
    const content=document.createElement('div');content.className='job-error-content';const list=document.createElement('ul');
    for(const group of groups){const row=document.createElement('li'),heading=document.createElement('div');heading.className='job-error-heading';const label=document.createElement('strong');label.textContent=group.source==='job'?'Job error':group.source==='conversion'?'Media file failures':`${group.label} · ${group.source==='notification'?'Scan notification':'Refresh result'}`;const repeats=document.createElement('span');repeats.className='job-error-count';repeats.textContent=`${group.count.toLocaleString()}×`;heading.append(label,repeats);const message=document.createElement('p');message.textContent=group.message;row.append(heading,message);
      if(group.examples.length){const examples=document.createElement('p');examples.className='job-error-examples';examples.textContent=`Examples: ${group.examples.join(' · ')}`;row.append(examples);}
      if(group.provider){const settings=document.createElement('button');settings.type='button';settings.className='text-button';settings.textContent=`Review ${group.label} settings`;settings.dataset.jobFocus=`${key}:${group.source}:${group.provider}:${group.message}`;settings.addEventListener('click',()=>{navigate('settings',group.provider);const card=$(`[data-provider="${group.provider}"]`);if(card?.classList.contains('is-collapsed'))$('.collapse-toggle',card).click();});row.append(settings);}
      list.append(row);
    }
    content.append(list);details.append(content);details.addEventListener('toggle',()=>{if(details.isConnected)state.errorViews.set(key,{open:details.open,scroll:content.scrollTop});});content.addEventListener('scroll',()=>{if(details.isConnected)state.errorViews.set(key,{open:details.open,scroll:content.scrollTop});});return details;
  };
  const renderRootSelectors = roots => {
    const rootList = $('#root-list'); rootList.replaceChildren();
    const quick = $('#quick-root'), inventoryRoot = $('#inventory-root');
    const keepQuick = quick.value, keepInventory = state.inventoryRestoreRoot ?? inventoryRoot.value;
    quick.replaceChildren(new Option('All scan roots','')); inventoryRoot.replaceChildren(new Option('All roots',''));
    for (const root of roots || []) {
      const label = root.label || root.path || root.id;
      const item = document.createElement('div'); item.className = 'root-item';
      const dot = document.createElement('span'); dot.className = 'root-dot';
      const text = document.createElement('span'); text.className = 'root-item-text'; text.title = root.path || label; text.textContent = label;
      const count = document.createElement('span'); count.className = 'root-count'; count.textContent = '•';
      item.append(dot,text,count); rootList.append(item);
      quick.add(new Option(label,root.id)); inventoryRoot.add(new Option(label,root.id));
    }
    if ([...quick.options].some(x=>x.value===keepQuick)) quick.value=keepQuick;
    if ([...inventoryRoot.options].some(x=>x.value===keepInventory)) inventoryRoot.value=keepInventory;
    state.inventoryRestoreRoot=null;
    setText($('#stat-roots'),(roots || []).length);
  };
  const updateLanguageLabels = () => {
    const label = languageDescription(state.selectedLanguages);
    setText($('#quick-languages'),label); setText($('#settings-languages'),label);
    setText($('#stat-languages'),isAll(state.selectedLanguages) ? 'All' : state.selectedLanguages.length);
  };
  const jobPercent = job => {
    const total=Number(job.total || 0), processed=Number(job.processed || 0);
    if (total>0) return Math.max(0,Math.min(100,Math.round(100*processed/total)));
    const summary = job.summary || {};
    const counts = ['converted','cached','pending','failed','planned'].reduce((sum,key)=>sum + Number(summary[key] || 0),0);
    if (Number.isFinite(Number(job.progress))) return Math.max(0,Math.min(100,Number(job.progress)));
    const done = Number(summary.converted || 0) + Number(summary.cached || 0) + Number(summary.failed || 0);
    return counts ? Math.round(100 * done / counts) : job.status === 'completed' ? 100 : 0;
  };
  const renderJobCard = (job, compact = false) => {
    const card = document.createElement('article'); card.className = `job-card${compact ? ' job-card-compact' : ''}`;
    const icon = document.createElement('div'); icon.className = `job-icon ${escStatus(job.status)}`; icon.textContent = job.kind === 'inspect' ? '⌕' : job.kind === 'extract' ? '▶' : '↻';
    const detail = document.createElement('div'); detail.className = 'job-detail';
    const title = document.createElement('strong'); title.textContent = job.kind==='refresh'?`${({sonarr:'Sonarr',radarr:'Radarr',jellyfin:'Jellyfin',bazarr:'Bazarr'})[job.provider] || 'Integration'} ${job.provider==='bazarr'?'sync and scan':'library refresh'}`:({scan:'Library discovery and text conversion',inspect:'Library inspection',extract:'Subtitle extraction',ocr:'Daily subtitle OCR',health:'Health check',cleanup:'Cache cleanup',backup:'State backup'})[job.kind] || 'Converter job';
    const sub = document.createElement('span'); sub.className = 'job-subtitle'; sub.textContent = currentLabel(job.current) || (job.paths && job.paths.length ? shortPath(job.paths[0]) : job.root_label || 'All scan roots');
    detail.append(title,sub);
    const status = document.createElement('div'); status.className = 'job-status-block';
    const pill = document.createElement('span'); pill.className=`status-pill ${escStatus(job.status)}`; pill.textContent=(job.status || 'queued').replaceAll('_',' ');
    const time=document.createElement('span'); time.className='job-time'; time.textContent=fmtDate(job.finished || job.started || job.created);
    status.append(pill,time);
    card.append(icon,detail,status);
    if (job.status === 'running' || job.status === 'queued') {
      const progress = document.createElement('div'); progress.className='job-progress'; const bar=document.createElement('span'); bar.style.width=`${jobPercent(job)}%`; progress.append(bar); card.append(progress);
      if (job.id) { const cancel=document.createElement('button'); cancel.className='text-button cancel-job'; cancel.textContent='Cancel'; cancel.dataset.jobId=job.id; cancel.dataset.jobFocus=`${errorViewKey(job,compact)}:cancel`; status.append(cancel); }
    } else if (job.summary) {
      const chips=document.createElement('div'); chips.className='job-summary';
      for (const [key,label] of [['converted','converted'],['cached','already ready'],['pending','pending'],['planned','planned'],['inspected','inspected'],['failed','failed']]) if (Number(job.summary[key] || 0)) { const chip=document.createElement('span'); chip.textContent=`${job.summary[key]} ${label}`; chips.append(chip); }
      if (chips.childNodes.length) card.append(chips);
    }
    const successfulResults=[...(job.kind==='refresh'&&Array.isArray(job.results)?job.results:[]),...(Array.isArray(job.notifications)?job.notifications.filter(item=>item?.provider==='bazarr'):[])].filter(item=>item?.ok===true&&typeof item.message==='string');
    for(const item of successfulResults){const result=document.createElement('p');result.className='job-result';result.textContent=item.message;card.append(result);}
    const errors=renderErrors(job,compact);if(errors)card.append(errors);
    return card;
  };
  const updateJobList = (container,jobs,compact) => {
    const signature=JSON.stringify(jobs);if(container.jobSignature===signature)return;
    const focusKey=container.contains(document.activeElement)?document.activeElement.dataset.jobFocus:null;
    for(const details of $$('.job-error',container))state.errorViews.set(details.dataset.errorKey,{open:details.open,scroll:$('.job-error-content',details).scrollTop});
    container.jobSignature=signature;container.replaceChildren();for(const job of jobs)container.append(renderJobCard(job,compact));
    if(!jobs.length){const empty=document.createElement('div');empty.className=`empty-state${compact?' compact':''}`;const icon=document.createElement('span');icon.className='empty-icon';icon.textContent='◷';const title=document.createElement('strong');title.textContent=compact?'No activity yet':'No jobs yet';const hint=document.createElement('span');hint.textContent=compact?'Start a scan to see progress here.':'Start a scan or extraction to see it here.';empty.append(icon,title,hint);container.append(empty);}
    if(focusKey)$$('[data-job-focus]',container).find(node=>node.dataset.jobFocus===focusKey)?.focus({preventScroll:true});
    for(const details of $$('.job-error',container))$('.job-error-content',details).scrollTop=state.errorViews.get(details.dataset.errorKey)?.scroll || 0;
  };
  const renderJobs = () => {
    const jobs=jobList(), running=state.status?.running;
    updateJobList($('#recent-activity'),jobs.slice(0,4),true);updateJobList($('#jobs-list'),jobs,false);
    const validKeys=new Set(jobs.flatMap(job=>[errorViewKey(job,true),errorViewKey(job,false)]));for(const key of state.errorViews.keys())if(!validKeys.has(key))state.errorViews.delete(key);
    setText($('#stat-running'),running ? 'Running' : 'Idle'); setText($('#stat-current'),currentLabel(running?.current) || (running ? 'Working through your library' : 'Nothing running right now'));
    const total=state.status?.converted_count ?? jobs.reduce((sum,job)=>sum+Number(job.summary?.converted || 0),0); setText($('#stat-converted'),total.toLocaleString());
    const queued=(state.status?.queue || []).length; const badge=$('#activity-badge'); badge.textContent=queued ? String(queued) : ''; badge.classList.toggle('hidden',!queued);
  };
  const renderLogs = logs => {
    const box=$('#live-logs'); box.replaceChildren();
    const entries=(logs || []).slice(-50);
    if (!entries.length) { const empty=document.createElement('div'); empty.className='log-empty'; empty.textContent='No recent converter events'; box.append(empty); return; }
    for (const entry of entries) {
      const row=document.createElement('div'); row.className='log-row';
      const time=document.createElement('time'); const timestamp=typeof entry.time==='number'&&entry.time<1e12?entry.time*1000:entry.time;time.textContent=timestamp ? new Date(timestamp).toLocaleTimeString([], {hour:'2-digit',minute:'2-digit',second:'2-digit'}) : '—';
      const level=document.createElement('span'); level.className=`log-level ${escStatus(entry.level)}`; level.textContent=(entry.level || 'INFO').toUpperCase();
      const message=document.createElement('span'); message.className='log-message'; message.textContent=entry.message || '';
      row.append(time,level,message); box.append(row);
    }
    box.scrollTop=box.scrollHeight;
  };
  const setConnection = good => { $('#connection-dot').classList.toggle('offline',!good); setText($('#connection-text'),good ? 'Service connected' : 'Service unavailable'); };
  const loadInventory = async summary => {
    if(state.inventoryBusy || state.inventoryLoaded && state.inventoryUpdated===summary?.updated)return;
    state.inventoryBusy=true;
    try {const data=await api('/api/inventory');state.inventory=Array.isArray(data.items)?data.items:[];state.inventoryUpdated=data.updated;state.inventoryTruncated=!!data.truncated;state.inventoryLoaded=true;renderInventory();}
    catch(error){notify(`Could not load library inventory: ${error.message}`,'error');}
    finally{state.inventoryBusy=false;}
  };
  const saveInventoryView = () => {
    try {sessionStorage.setItem('subforge.inventory-view',JSON.stringify({page:state.inventoryPage,pageSize:state.inventoryPageSize,search:$('#inventory-search').value,root:$('#inventory-root').value,filter:$('#inventory-filter').value}));}catch{}
  };
  const restoreInventoryView = () => {
    try {const view=JSON.parse(sessionStorage.getItem('subforge.inventory-view')||'{}');state.inventoryPage=Number.isInteger(view.page)&&view.page>0?Math.min(2000,view.page):1;state.inventoryPageSize=[25,50,100].includes(view.pageSize)?view.pageSize:25;$('#inventory-page-size').value=String(state.inventoryPageSize);$('#inventory-search').value=typeof view.search==='string'?view.search.slice(0,256):'';$('#inventory-filter').value=['all','video','sidecar','issues'].includes(view.filter)?view.filter:'all';state.inventoryRestoreRoot=typeof view.root==='string'?view.root.slice(0,64):'';}catch{}
  };
  const pollStatus = async () => {
    if (state.pollBusy || !state.csrf) return; state.pollBusy=true;
    try { state.status=await api('/api/status'); setConnection(true); renderRootSelectors(state.status.roots || []); renderJobs(); renderLogs(state.status.logs); await loadInventory(state.status.inventory); renderInventory(); setText($('#last-updated'),`Updated ${new Date().toLocaleTimeString([], {hour:'2-digit',minute:'2-digit'})}`); updateHookStates(); updateIntegrationUsage(); updateVersion(state.status.version,state.status.edition); if(state.currentPage==='settings'&&state.settingsSection==='tasks'&&Date.now()-state.tasksLoadedAt>10000)await loadTasks(); }
    catch (error) { setConnection(false); if (!state.status) notify(`Could not load converter status: ${error.message}`,'error'); }
    finally { state.pollBusy=false; }
  };
  const startJob = async ({kind,rootIds=[],paths=[],languages: selected=state.selectedLanguages,retry=false}) => {
    try { const result=await api('/api/jobs',{method:'POST',body:JSON.stringify({kind,root_ids:rootIds,paths,languages:selected,retry})}); notify(result.message || 'Job queued.'); await pollStatus(); return result; }
    catch (error) { notify(error.message,'error'); return null; }
  };
  const selectedRootIds = selector => { const value=$(selector).value; return value ? [value] : []; };
  const renderInventory = () => {
    const body=$('#inventory-body');
    const filter=$('#inventory-filter').value, root=$('#inventory-root').value, query=$('#inventory-search').value.trim().toLowerCase();
    let rows=state.inventory.filter(row=>!root || row.root_id===root);
    if (filter==='video'||filter==='sidecar') rows=rows.filter(row=>row.type===filter);
    if (filter==='issues') rows=rows.filter(row=>row.error || row.type==='video' && !(row.tracks || []).length);
    if (query) rows=rows.filter(row=>`${row.name || ''}\n${row.path || ''}`.toLowerCase().includes(query));
    const count=rows.length, pages=Math.max(1,Math.ceil(count/state.inventoryPageSize));
    if(state.inventoryLoaded)state.inventoryPage=Math.max(1,Math.min(state.inventoryPage,pages));
    const start=(state.inventoryPage-1)*state.inventoryPageSize, visibleRows=rows.slice(start,start+state.inventoryPageSize);
    setText($('#inventory-meta'),`${count.toLocaleString()}${query||root||filter!=='all'?` of ${state.inventory.length.toLocaleString()}`:''} item${count===1?'':'s'}`);
    const range=count?`${(start+1).toLocaleString()}–${(start+visibleRows.length).toLocaleString()} of ${count.toLocaleString()} items`:'0 items';
    if ($('#inventory-range').textContent!==range) setText($('#inventory-range'),range);
    const pageSelect=$('#inventory-page');
    if (pageSelect.options.length!==pages) { pageSelect.replaceChildren();for(let page=1;page<=pages;page++)pageSelect.add(new Option(`Page ${page} of ${pages}`,String(page))); }
    pageSelect.value=String(state.inventoryPage);pageSelect.disabled=!count;
    $('#inventory-prev').disabled=state.inventoryPage===1;$('#inventory-next').disabled=state.inventoryPage===pages;
    $('#inventory-limit-note').classList.toggle('hidden',!state.inventoryTruncated);
    const refreshing=state.status?.running?.kind==='inspect'||(state.status?.queue||[]).some(job=>job.kind==='inspect');
    setText($('#inventory-snapshot-note'),state.inventoryUpdated?`${refreshing?'Refreshing… Showing the saved inventory. ':''}Last inspection: ${fmtDate(state.inventoryUpdated)}${state.status?.inventory?.partial?' · Partial inventory':''}`:refreshing?'Building your first inventory…':'');
    const emptyMessage=state.inventory.length?'No files match your search or filters.':'No inventory yet. Choose Refresh inventory to list available subtitle tracks.';
    const viewKey=JSON.stringify([visibleRows,visibleRows.length?'':emptyMessage]);
    if (viewKey===state.inventoryViewKey) return;
    state.inventoryViewKey=viewKey;body.replaceChildren();
    if (!count) { const tr=document.createElement('tr'),td=document.createElement('td'); td.colSpan=5; td.className='table-empty'; td.textContent=emptyMessage; tr.append(td); body.append(tr); return; }
    for (const row of visibleRows) {
      const tr=document.createElement('tr'); const name=document.createElement('td'); name.className='file-cell'; const fileIcon=document.createElement('span'); fileIcon.className=`file-icon ${row.type==='sidecar'?'srt':''}`; fileIcon.textContent=row.type==='sidecar'?'S':'▣'; const filename=document.createElement('span'); filename.className='file-name'; filename.textContent=row.name || row.path; filename.title=row.path || row.name; name.append(fileIcon,filename);
      const type=document.createElement('td'); const badge=document.createElement('span'); badge.className='type-badge'; badge.textContent=row.error?'Issue':row.type==='sidecar'?'Sidecar':'Video'; type.append(badge);
      const tracks=document.createElement('td'); tracks.textContent=row.error ? row.error : `${(row.tracks || []).length} track${(row.tracks || []).length===1?'':'s'}`;
      const lang=document.createElement('td'); lang.className='language-tags'; for (const item of [...new Set((row.tracks || []).map(track=>track.language || 'und'))].slice(0,4)) { const tag=document.createElement('span'); tag.textContent=langLabel(item); lang.append(tag); } if (!lang.childNodes.length) lang.textContent='—';
      const action=document.createElement('td'); if (!row.error && (row.type==='sidecar' || row.type==='video' && row.tracks?.length)) { const button=document.createElement('button'); button.className='row-action'; button.textContent='Extract'; button.title='Extract selected languages from this file'; button.addEventListener('click',()=>startJob({kind:'extract',paths:[row.path],rootIds:row.root_id?[row.root_id]:[]})); action.append(button); } else if (row.error) { const issue=document.createElement('span'); issue.className='issue-dot'; issue.textContent='!'; issue.title=row.error; action.append(issue); } else action.textContent='—';
      tr.append(name,type,tracks,lang,action); body.append(tr);
    }
  };
  const inspect = async () => { const result=await startJob({kind:'inspect',rootIds:selectedRootIds('#inventory-root')}); if (!result) return; if (Array.isArray(result.results)) { state.inventory=result.results; state.inventoryTruncated=!!result.truncated; renderInventory(); } };
  const renderLanguageList = () => {
    const list=$('#language-list'), term=$('#language-search').value.trim().toLowerCase(); list.replaceChildren();
    const allLabel=document.createElement('label'); allLabel.className='language-option all-option'; const all=document.createElement('input'); all.type='checkbox'; all.checked=isAll(state.draftLanguages); all.addEventListener('change',()=>{state.draftLanguages=all.checked?['*']:[];renderLanguageList();}); const allCopy=document.createElement('span'); const allTitle=document.createElement('strong'); allTitle.textContent='All languages'; const allHint=document.createElement('small'); allHint.textContent='Extract every supported subtitle track'; allCopy.append(allTitle,allHint); allLabel.append(all,allCopy); list.append(allLabel);
    const codes=new Set(languages.map(x=>x[0].toLowerCase())); for (const code of state.draftLanguages) if (code!=='*' && !codes.has(code.toLowerCase())) languages.push([code,`${langLabel(code)} · ${code.toUpperCase()}`]);
    for (const [code,name] of languages) {
      if (term && !`${name} ${code}`.toLowerCase().includes(term)) continue;
      const label=document.createElement('label'); label.className='language-option'; const input=document.createElement('input'); input.type='checkbox'; input.value=code; input.checked=!isAll(state.draftLanguages) && state.draftLanguages.some(item=>item.toLowerCase()===code.toLowerCase()); input.addEventListener('change',()=>{if(isAll(state.draftLanguages))state.draftLanguages=[];state.draftLanguages=input.checked?[...new Set([...state.draftLanguages,code])]:state.draftLanguages.filter(item=>item!==code);updateLanguageCount();}); const copy=document.createElement('span'); const title=document.createElement('strong'); title.textContent=name; const detail=document.createElement('small'); detail.textContent=code.toUpperCase(); copy.append(title,detail); label.append(input,copy); list.append(label);
    }
    updateLanguageCount();
  };
  const updateLanguageCount = () => setText($('#language-count'),isAll(state.draftLanguages)?'All languages selected':`${state.draftLanguages.length} language${state.draftLanguages.length===1?'':'s'} selected`);
  const openLanguageDialog = target => { state.languageTarget=target; state.modalTrigger=document.activeElement; state.draftLanguages=[...state.selectedLanguages]; $('#language-search').value=''; $('#language-modal').classList.remove('hidden'); renderLanguageList(); $('#language-list').scrollTop=0; $('.language-modal-body').scrollTop=0; $('#language-search').focus(); };
  const closeDialog = backdrop => { if (!backdrop || backdrop.classList.contains('hidden')) return; backdrop.classList.add('hidden'); if (state.modalTrigger?.isConnected) state.modalTrigger.focus(); };
  const addCustomLanguage = () => { const input=$('#custom-language'), code=input.value.trim().toLowerCase(); if (!/^[a-z]{2,3}(?:-[a-z0-9]{2,8})*$/.test(code)) { notify('Enter a valid ISO language code, such as pl or pt-BR.','error'); return; } languages.push([code,code.toUpperCase()]); state.draftLanguages=isAll(state.draftLanguages)?[code]:[...new Set([...state.draftLanguages,code])]; input.value=''; renderLanguageList(); };
  const buildIntegrationCard = (provider, config={}) => {
    const card=$('#integration-template').content.firstElementChild.cloneNode(true); card.dataset.provider=provider;
    const title={sonarr:'Sonarr',radarr:'Radarr',jellyfin:'Jellyfin',bazarr:'Bazarr'}[provider]; const description={sonarr:'Trigger subtitle scans when episodes are imported, and notify Sonarr when sidecars are ready.',radarr:'Trigger subtitle scans when movies are imported, and notify Radarr when sidecars are ready.',jellyfin:'Refresh selected Jellyfin libraries after subtitle extraction.',bazarr:'Automatically sync and scan affected media after each extraction batch. Bazarr may download missing subtitles according to its own rules during sync.'}[provider];
    setText($('.integration-title',card),title); setText($('.integration-description',card),description);
    const usage=document.createElement('div');usage.className='path-map-field';
    const usageTitle=document.createElement('strong');usageTitle.textContent='How this integration is used';
    const usageCopy=document.createElement('p');usageCopy.className='field-explainer';usageCopy.textContent={sonarr:'An optional Sonarr Connect webhook starts extraction when episodes are imported or renamed. New SRT files trigger a rescan of the matching series.',radarr:'An optional Radarr Connect webhook starts extraction when movies are imported or renamed. New SRT files trigger a rescan of the matching movie.',jellyfin:'New SRT files trigger metadata refreshes within your selected movie and series libraries. Refresh selected libraries runs a broader manual refresh.',bazarr:'Changed movies and episodes are synchronized and their subtitles indexed after the extraction batch. Sync and scan library runs the broader manual tasks.'}[provider];
    const last=document.createElement('p');last.className='field-explainer integration-last-result';last.textContent='No extraction notification recorded yet.';usage.append(usageTitle,usageCopy,last);$('.integration-fields',card).append(usage);
    $('.integration-enabled',card).checked=!!config.enabled; $('.integration-url',card).value=config.url || '';
    $('.integration-key',card).value=config.api_key || ''; setText($('.api-key-hint',card),config.api_key_set?'A key is saved. Leave blank to keep it.':'');
    if (provider==='jellyfin') {
      $('.api-key-field',card).querySelector('span').childNodes[0].textContent='API key ';
      $('.integration-url',card).placeholder='http://jellyfin:8096'; $('.integration-key',card).placeholder='Jellyfin API key';
      $('.jellyfin-fields',card).classList.remove('hidden');
      $('.integration-url',card).readOnly=true; $('.integration-key',card).readOnly=true;
      $('.integration-key',card).value=config.api_key_set?'••••••••••••••••':'Not configured';
      $('.integration-key',card).type='text'; $('.integration-key',card).classList.add('managed-value');
      setText($('.api-key-hint',card),`Managed by JELLYFIN_INTERNAL_URL and JELLYFIN_API_KEY · ${config.api_key_set?'key configured':'key missing'}`);
      renderMappings(card,(state.settings?.path_mappings || []).filter(item=>!item.provider || item.provider==='jellyfin'));
      populateLibraryOptions(card,state.jellyfinLibraries.length?state.jellyfinLibraries.filter(item=>item.type==='movies'):config.movie_libraries || [],state.jellyfinLibraries.length?state.jellyfinLibraries.filter(item=>item.type==='series'):config.series_libraries || [],config.movie_libraries || [],config.series_libraries || []);
      $('.refresh-movies',card).checked=!!config.refresh_movies; $('.refresh-series',card).checked=!!config.refresh_series; $('.refresh-mode',card).value=config.refresh_mode || 'immediate';
      $('.refresh-integration',card).textContent='Refresh selected libraries';
    } else {
      $('.integration-url',card).placeholder={sonarr:'http://sonarr:8989',radarr:'http://radarr:7878',bazarr:'http://bazarr:6767'}[provider];
      $('.integration-key',card).placeholder='API key'; renderMappings(card,(state.settings?.path_mappings || []).filter(item=>!item.provider || item.provider===provider));
      if(provider==='bazarr')$('.refresh-integration',card).textContent='Sync and scan library';
    }
    $('.test-integration',card).addEventListener('click',()=>testIntegration(provider,card));
    $('.refresh-integration',card).addEventListener('click',()=>refreshIntegration(provider,card));
    $$('.load-libraries',card).forEach(button=>button.addEventListener('click',()=>loadLibraries(card)));
    $('.add-path-map',card).addEventListener('click',()=>{const entries=readMappings(card); entries.push({remote:'',local:''});renderMappings(card,entries);});
    card.dataset.collapse=`integration-${provider}`;makePanelCollapsible(card);return card;
  };
  const populateLibraryOptions = (card,movies,series,selectedMovies=movies,selectedSeries=series) => {
    for (const [selector,values,selected] of [['.movie-libraries',movies,selectedMovies],['.series-libraries',series,selectedSeries]]) {
      const container=$(selector,card); container.replaceChildren();
      if (!values.length) { const muted=document.createElement('span'); muted.className='muted'; muted.textContent='Load libraries to choose where refreshes apply.'; container.append(muted); continue; }
      for (const item of values) { const label=document.createElement('label'); label.className='library-check'; const input=document.createElement('input'); input.type='checkbox'; input.value=typeof item==='string'?item:item.id; input.checked=selected.some(value=>(typeof value==='string'?value:value.id)===input.value); const text=document.createElement('span'); text.textContent=typeof item==='string'?item:item.name || item.id; label.append(input,text); container.append(label); }
    }
  };
  const renderMappings = (card,entries) => {
    const list=$('.path-map-list',card); list.replaceChildren();
    for (const mapping of entries) {
      const row=document.createElement('div'); row.className='path-map-row'; const remote=document.createElement('input'); remote.className='input map-remote'; remote.value=mapping.remote || ''; remote.placeholder='Remote path, e.g. /tv'; remote.setAttribute('aria-label','Remote path'); const arrow=document.createElement('span'); arrow.textContent='→'; const local=document.createElement('input'); local.className='input map-local'; local.value=mapping.local || ''; local.placeholder='Local path, e.g. /media/tv'; local.setAttribute('aria-label','Local path'); const remove=document.createElement('button'); remove.type='button'; remove.className='icon-button remove-map'; remove.setAttribute('aria-label','Remove path mapping'); remove.textContent='×'; remove.addEventListener('click',()=>row.remove()); row.append(remote,arrow,local,remove); list.append(row);
    }
  };
  const readMappings = card => $$('.path-map-row',card).map(row=>({remote:$('.map-remote',row).value.trim(),local:$('.map-local',row).value.trim()})).filter(item=>item.remote || item.local);
  const readSelectedLibraries = (card,selector) => $(`${selector}`,card) ? $$('.library-check input:checked',$(selector,card)).map(input=>input.value) : [];
  const formatSpace = bytes => {
    if(typeof bytes!=='number'||!Number.isFinite(bytes)||bytes<0)return '—';
    const units=['B','KiB','MiB','GiB','TiB'];let value=bytes,index=0;
    while(value>=1024&&index<units.length-1){value/=1024;index++;}
    return `${value.toLocaleString(undefined,{maximumFractionDigits:index?1:0})} ${units[index]}`;
  };
  const renderRootFolders = () => {
    const body=$('#root-folder-body');body.replaceChildren();
    const roots=state.rootFolders.roots || [];
    if(!roots.length){const row=document.createElement('tr'),cell=document.createElement('td');cell.colSpan=4;cell.className='table-empty';cell.textContent='No root folders selected. Add a folder to enable scanning.';row.append(cell);body.append(row);return;}
    for(const root of roots){
      const row=document.createElement('tr'),path=document.createElement('td'),space=document.createElement('td'),status=document.createElement('td'),actions=document.createElement('td');
      const pathButton=document.createElement('button');pathButton.type='button';pathButton.className='root-folder-path';pathButton.textContent=root.path;pathButton.title='Browse this folder';pathButton.disabled=root.status!=='ready';pathButton.addEventListener('click',()=>openRootFolderDialog(root.path));path.append(pathButton);
      space.textContent=formatSpace(root.free_bytes);
      const badge=document.createElement('span');badge.className=`status-pill ${root.status==='ready'?'success':'failed'}`;badge.textContent=root.status==='ready'?'Available':'Unavailable';status.append(badge);
      const group=document.createElement('div');group.className='root-folder-actions';
      const scan=document.createElement('button');scan.type='button';scan.className='icon-button scan-root-folder';scan.textContent='↻';scan.title='Scan this folder';scan.setAttribute('aria-label',`Scan ${root.path}`);scan.disabled=root.status!=='ready';scan.addEventListener('click',()=>startJob({kind:'scan',rootIds:[root.id]}));
      const remove=document.createElement('button');remove.type='button';remove.className='icon-button remove-root-folder';remove.textContent='×';remove.title='Remove from scan folders';remove.setAttribute('aria-label',`Remove ${root.path}`);remove.addEventListener('click',()=>saveRootFolders(roots.filter(item=>item.path!==root.path).map(item=>item.path)));
      group.append(scan,remove);actions.append(group);row.append(path,space,status,actions);body.append(row);
    }
  };
  const loadRootFolders = async () => {
    try{state.rootFolders=await api('/api/root-folders');renderRootFolders();}
    catch(error){notify(`Could not load root folders: ${error.message}`,'error');}
  };
  const saveRootFolders = async paths => {
    const controls=$$('.remove-root-folder,#select-root-folder');controls.forEach(button=>button.disabled=true);
    try{state.rootFolders=await api('/api/root-folders',{method:'PUT',body:JSON.stringify({paths})});renderRootFolders();closeDialog($('#root-folder-modal'));await pollStatus();notify('Root folders saved. Changes apply to future scans.','success');}
    catch(error){notify(`Root folders not saved: ${error.message}`,'error');}
    finally{controls.filter(button=>button.isConnected).forEach(button=>button.disabled=false);}
  };
  const browseRootFolder = async path => {
    const request=++state.folderRequest,button=$('#select-root-folder'),list=$('#folder-browser-list');button.disabled=true;state.currentFolder=null;list.replaceChildren();setText($('#folder-browser-note'),'Loading folders…');
    try{
      const folder=await api(`/api/folders?path=${encodeURIComponent(path || '')}`);if(request!==state.folderRequest)return;
      state.currentFolder=folder.path;state.folderParent=folder.parent;$('#folder-path').value=folder.path || '';setText($('#folder-current-path'),folder.path || 'Media locations');$('#folder-parent-button').disabled=!folder.parent;
      for(const item of folder.folders || []){const child=document.createElement('button');child.type='button';child.className='folder-browser-item';const icon=document.createElement('span');icon.textContent='▱';icon.setAttribute('aria-hidden','true');const name=document.createElement('span');name.textContent=item.name;child.append(icon,name);child.addEventListener('click',()=>browseRootFolder(item.path));list.append(child);}
      if(!list.children.length){const empty=document.createElement('div');empty.className='folder-browser-empty';empty.textContent='No subfolders here. You can select this folder.';list.append(empty);}
      const alreadySelected=state.rootFolders.roots.some(root=>root.path===folder.path);button.disabled=!folder.path||alreadySelected;
      setText($('#folder-browser-note'),alreadySelected?'This folder is already selected. Browse a subfolder or choose another media location.':folder.truncated?'Showing the first 200 subfolders. Enter a folder path to browse further.':'Only folders inside the available media locations can be selected.');
    }catch(error){if(request!==state.folderRequest)return;setText($('#folder-browser-note'),error.message);state.folderParent=null;$('#folder-parent-button').disabled=true;}
  };
  const openRootFolderDialog = async path => {
    state.modalTrigger=document.activeElement;await loadRootFolders();const select=$('#folder-mount');select.replaceChildren();
    for(const mount of state.rootFolders.mounts || []){const option=new Option(`${mount.path}${mount.status==='ready'?'':' · unavailable'}`,mount.path);option.disabled=mount.status!=='ready';select.add(option);}
    const selected=path || state.rootFolders.mounts.find(mount=>mount.status==='ready')?.path || '';
    const mount=state.rootFolders.mounts.filter(item=>selected===item.path||selected.startsWith(item.path.replace(/[\\/]$/,'')+'/')||selected.startsWith(item.path.replace(/[\\/]$/,'')+'\\')).sort((a,b)=>b.path.length-a.path.length)[0];
    if(mount)select.value=mount.path;
    $('#root-folder-modal').classList.remove('hidden');$('#folder-path').focus();await browseRootFolder(selected);
  };
  const persistSettings = async () => { const data=await api('/api/settings',{method:'PUT',body:JSON.stringify(gatherSettings())}); state.settings=data; return data; };
  const loadSettings = async () => {
    try {
      const data=await api('/api/settings'); state.settings=data; state.selectedLanguages=data.languages || ['*'];
      const ocrEnabled=data.permissions?.ocr===true;
      setText($('#ocr-permission-state'),ocrEnabled?'Enabled':'Locked');
      setText($('#ocr-permission-note'),ocrEnabled?'Image subtitle extraction is enabled for this service.':'Text subtitles are enabled. To include image subtitles, enable CONVERTER_ENABLE_OCR and use a 1536 MiB memory limit.');
      for (const provider of ['sonarr','radarr','jellyfin','bazarr']) { const node=$(`#${provider}-settings`); node.replaceChildren(buildIntegrationCard(provider,{...integrationDefaults(),...(data.integrations?.[provider] || {})})); }
      updateLanguageLabels(); updateHookStates(); updateIntegrationUsage();
    } catch(error) { notify(`Could not load settings: ${error.message}`,'error'); }
  };
  const gatherSettings = () => {
    const integrations={}; const path_mappings=[];
    for (const provider of ['sonarr','radarr','jellyfin','bazarr']) {
      const card=$(`[data-provider="${provider}"]`); const old=state.settings?.integrations?.[provider] || {};
      integrations[provider]={enabled:$('.integration-enabled',card).checked,url:$('.integration-url',card).value.trim(),api_key:provider==='jellyfin'?'':$('.integration-key',card).value,
        api_key_set:!!old.api_key_set};
      path_mappings.push(...readMappings(card).map(item=>({...item,provider})));
      if(provider==='jellyfin') { delete integrations[provider].url; delete integrations[provider].api_key; Object.assign(integrations[provider],{movie_libraries:readSelectedLibraries(card,'.movie-libraries'),series_libraries:readSelectedLibraries(card,'.series-libraries'),refresh_movies:$('.refresh-movies',card).checked,refresh_series:$('.refresh-series',card).checked,refresh_mode:$('.refresh-mode',card).value}); }
    }
    return {languages:state.selectedLanguages,integrations,path_mappings};
  };
  const saveSettings = async () => {
    const button=$('#save-settings'), note=$('#settings-save-note'); button.disabled=true; setText(note,'Saving…');
    try { const data=await persistSettings(); for (const provider of ['sonarr','radarr','jellyfin','bazarr']) {const node=$(`#${provider}-settings`);node.replaceChildren(buildIntegrationCard(provider,{...integrationDefaults(),...(data.integrations?.[provider] || {})}));} state.selectedLanguages=data.languages || state.selectedLanguages; updateLanguageLabels(); setText(note,'Changes saved'); notify('Settings saved.','success'); }
    catch(error) { setText(note,error.message); notify(`Settings not saved: ${error.message}`,'error'); }
    finally { button.disabled=false; setTimeout(()=>setText(note,''),5000); }
  };
  const testIntegration = async (provider,card) => { const result=$('.test-result',card),button=$('.test-integration',card); button.disabled=true; setText(result,'Testing…'); result.className='test-result muted';
    try { await persistSettings(); const response=await api(`/api/integrations/${provider}/test`,{method:'POST',body:'{}'}); setText(result,response.message || `Connected${response.version?` · ${response.version}`:''}`); result.className=`test-result ${response.connected===false?'error-text':'success-text'}`; if(provider==='jellyfin'&&Array.isArray(response.libraries)) {const config=state.settings?.integrations?.jellyfin || {};populateLibraryOptions(card,response.libraries.filter(x=>/movie/i.test(x.type)),response.libraries.filter(x=>/series|tv/i.test(x.type)),config.movie_libraries||[],config.series_libraries||[]);} }
    catch(error) { setText(result,error.message); result.className='test-result error-text'; }
    finally { button.disabled=false; }
  };
  const refreshIntegration = async (provider,card) => { const result=$('.test-result',card),button=$('.refresh-integration',card); button.disabled=true; setText(result,'Queueing refresh…'); result.className='test-result muted';
    try { await persistSettings(); const response=await api(`/api/integrations/${provider}/refresh`,{method:'POST',body:'{}'}); setText(result,response.message || 'Refresh job queued.'); result.className='test-result success-text'; notify(response.message || `${provider} refresh queued.`,'success'); await pollStatus(); }
    catch(error) { setText(result,error.message); result.className='test-result error-text'; notify(error.message,'error'); }
    finally { button.disabled=false; }
  };
  const loadLibraries = async card => { try { const data=await api('/api/integrations/jellyfin/libraries'); state.jellyfinLibraries=data.libraries||[]; const config=state.settings?.integrations?.jellyfin || {}; populateLibraryOptions(card,state.jellyfinLibraries.filter(x=>/movie/i.test(x.type)),state.jellyfinLibraries.filter(x=>/series|tv/i.test(x.type)),config.movie_libraries||[],config.series_libraries||[]); notify('Jellyfin libraries loaded.','success'); } catch(error) { notify(error.message,'error'); } };
  const updateHookStates = () => { const integrations=state.status?.settings?.integrations || state.settings?.integrations || {}; for(const name of ['sonarr','radarr']) { const enabled=!!integrations[name]?.webhook_enabled; const node=$(`#${name}-hook-state`); if(node){node.className=`connection-state ${enabled?'is-on':''}`;setText(node,enabled?'READY':'TOKEN REQUIRED');} } };
  const updateIntegrationUsage = () => {
    for(const provider of ['sonarr','radarr','jellyfin','bazarr']) {
      const node=$(`[data-provider="${provider}"] .integration-last-result`);if(!node)continue;
      const jobs=jobList();let match=null;
      for(const job of jobs){const result=[...(job.notifications||[]),...(job.results||[])].find(item=>item?.provider===provider&&typeof item.ok==='boolean');if(result){match={job,result};break;}}
      setText(node,match?`Latest recorded request: ${match.result.ok?'Succeeded':'Failed'} · ${fmtDate(match.job.finished||match.job.started||match.job.created)}`:'No extraction notification recorded yet. Connection tests check access; they do not register import webhooks.');
    }
  };
  const editionLabel = edition => ({cpu:'CPU only',cuda:'CUDA'})[edition] || 'Unavailable';
  const updateEdition = edition => {
    if(!['cpu','cuda'].includes(edition))return;
    state.edition=edition;
    for(const selector of ['#edition-label','#header-edition']){const node=$(selector);setText(node,editionLabel(edition));node.hidden=false;node.dataset.edition=edition;node.title=edition==='cuda'?'CUDA edition; GPU workers require a granted device and a saved allocation.':'CPU-only edition; GPU workers are unavailable.';}
  };
  const updateVersion = (version,edition) => {updateEdition(edition);if(typeof version==='string'&&/^\d+\.\d+\.\d+(?:[-+][a-zA-Z0-9.-]+)?$/.test(version)){setText($('#version-label'),`v${version}`);$('#version-label').title=`Subforge ${version} (${editionLabel(state.edition)})`;$('.brand').title=$('#version-label').title;}};
  const intervalLabel = seconds => seconds%604800===0?`Every ${seconds/604800===1?'week':`${seconds/604800} weeks`}`:seconds%86400===0?`Every ${seconds/86400===1?'day':`${seconds/86400} days`}`:`Every ${seconds/3600===1?'hour':`${seconds/3600} hours`}`;
  const markTasksDirty = () => {state.tasksDirty=true;$('#save-task-schedules').disabled=false;setText($('#task-schedule-note'),'Unsaved schedule changes. Manual Run uses the saved schedule and permissions.');};
  const renderTasks = () => {
    if(!state.tasks)return;
    updateVersion(state.tasks.version,state.tasks.edition);
    const paused=!!state.tasks.paused;setText($('#scheduler-state'),paused?'Paused':'Scheduled');$('#scheduler-state').className=`status-pill ${paused?'cancelled':'completed'}`;
    if(state.tasksDirty)return;
    $('#pause-schedules').checked=paused;
    const body=$('#tasks-body');body.replaceChildren();
    for(const task of state.tasks.tasks || []) {
      const row=document.createElement('tr');row.dataset.taskId=task.id;
      const name=document.createElement('td'),copy=document.createElement('div');copy.className='task-name';const title=document.createElement('strong'),description=document.createElement('small');title.textContent=task.name;description.textContent=task.description;copy.append(title,description);name.append(copy);
      const schedule=document.createElement('td'),controls=document.createElement('div');schedule.dataset.label='Schedule';controls.className='task-schedule';const label=document.createElement('label');label.className='check-row';const enabled=document.createElement('input');enabled.type='checkbox';enabled.className='task-enabled';enabled.checked=!!task.enabled;enabled.disabled=task.available===false;enabled.setAttribute('aria-label',`Automatically run ${task.name}`);const labelText=document.createElement('span');labelText.textContent='Automatic';label.append(enabled,labelText);
      const interval=document.createElement('select');interval.className='input compact-input task-interval';interval.setAttribute('aria-label',`${task.name} interval`);interval.disabled=task.available===false;
      const choices=[...new Set([3600,21600,43200,86400,172800,604800,1209600,2592000,task.interval_seconds])].filter(value=>value>=task.min_interval_seconds).sort((a,b)=>a-b);
      for(const value of choices)interval.add(new Option(intervalLabel(value),String(value)));interval.value=String(task.interval_seconds);enabled.addEventListener('change',markTasksDirty);interval.addEventListener('change',markTasksDirty);controls.append(label,interval);schedule.append(controls);
      const next=document.createElement('td');next.dataset.label='Next execution';next.className='task-next';next.textContent=task.available===false?'Permission disabled':paused?'Paused':task.enabled?fmtDate(task.next_run):'Manual only';
      const last=document.createElement('td');last.dataset.label='Last result';last.className='task-last-result';const result=document.createElement('span');result.textContent=task.last_result||'Not run yet';last.append(result);if(task.last_run){const time=document.createElement('small');time.textContent=fmtDate(task.last_run);last.append(document.createElement('br'),time);}
      const action=document.createElement('td'),run=document.createElement('button');run.type='button';run.className='button button-secondary task-run';run.textContent=['running','queued'].includes(task.status)?'…':'▶';run.setAttribute('aria-label',`Run ${task.name}`);run.title=task.blocked_reason||`Run ${task.name} now`;run.disabled=task.available===false||['running','queued'].includes(task.status)||state.taskRequests.has(task.id);run.addEventListener('click',()=>runTask(task.id));action.append(run);row.append(name,schedule,next,last,action);body.append(row);
    }
    setText($('#task-schedule-note'),'Manual Run buttons remain available while schedules are paused. Pause does not cancel current work.');
  };
  const loadTasks = async () => {
    if(state.tasksBusy||!state.csrf)return;state.tasksBusy=true;
    try{state.tasks=await api('/api/tasks');state.tasksLoadedAt=Date.now();renderTasks();}
    catch(error){notify(`Could not load scheduled tasks: ${error.message}`,'error');}
    finally{state.tasksBusy=false;}
  };
  const saveTasks = async () => {
    const tasks=$$('#tasks-body tr[data-task-id]').map(row=>({id:row.dataset.taskId,enabled:$('.task-enabled',row).checked,interval_seconds:Number($('.task-interval',row).value)}));
    $('#save-task-schedules').disabled=true;
    try{state.tasks=await api('/api/tasks',{method:'PUT',body:JSON.stringify({paused:$('#pause-schedules').checked,tasks})});state.tasksDirty=false;renderTasks();notify('Task schedules saved.','success');}
    catch(error){$('#save-task-schedules').disabled=false;notify(error.message,'error');}
  };
  const runTask = async id => {
    if(state.taskRequests.has(id))return;state.taskRequests.add(id);renderTasks();
    try{const body=id==='ocr'?{retry:$('#ocr-retry-failed').checked}:{};const result=await api(`/api/tasks/${encodeURIComponent(id)}/run`,{method:'POST',body:JSON.stringify(body)});if(id==='ocr')$('#ocr-retry-failed').checked=false;notify(result.message||'Task queued.');await pollStatus();await loadTasks();}
    catch(error){notify(error.message,'error');}
    finally{state.taskRequests.delete(id);renderTasks();}
  };
  const workersVisible = () => state.currentPage==='settings'&&state.settingsSection==='workers'&&!document.hidden;
  const workerFields={cpu_workers:'#worker-cpu-count',ocr_workers:'#worker-ocr-count',gpu_workers:'#worker-gpu-count'};
  const workerDirty = () => {state.workersDirty=true;$('#save-worker-allocation').disabled=state.workersSaving;setText($('#worker-allocation-note'),'Unsaved allocation. Save to apply it to the next files; running files keep their current resources.');renderWorkerSteppers();};
  const renderWorkerSteppers = () => {
    const data=state.workersInfo;if(!data)return;
    for(const [kind,selector] of Object.entries(workerFields)){
      const input=$(selector),ceiling=Number(data.deployment?.[`max_${kind}`]??0);
      input.max=String(kind==='ocr_workers'?Math.min(ceiling,Number($('#worker-cpu-count').value)||1):ceiling);
      input.disabled=ceiling===0||state.workersSaving;
      $$(`[data-worker-step="${kind}"]`).forEach(button=>{const next=Number(input.value)+Number(button.dataset.delta);button.disabled=input.disabled||next<Number(input.min)||next>Number(input.max);});
    }
  };
  const renderWorkers = () => {
    const data=state.workersInfo;if(!data)return;
    updateEdition(data.edition);
    const allocation=data.allocation||{},effective=data.effective||{},active=data.active||{},detected=data.detected||{},usage=state.systemInfo?.resource_usage||{},cpu=usage.cpu||{},memory=usage.memory||{};
    if(!state.workersDirty&&!state.workersSaving){for(const [kind,selector] of Object.entries(workerFields))$(selector).value=String(allocation[kind]??0);$('#worker-paused').checked=!!allocation.paused;}
    renderWorkerSteppers();
    const paused=!!allocation.paused,total=Number(active.cpu_workers||0)+Number(active.gpu_workers||0);
    setText($('#worker-pool-state'),paused?'Paused':total?`${total} active`:'Ready');$('#worker-pool-state').className=`status-pill ${paused?'cancelled':total?'running':'completed'}`;
    const allowance=measuredNumber(cpu.effective_cores)??measuredNumber(detected.cpu_allowance_cores);
    setText($('#worker-cpu-allowance'),allowance===null?'Unavailable':`${allowance.toLocaleString()} CPU${allowance===1?'':'s'}`);
    setText($('#worker-cpu-usage'),measuredNumber(cpu.quota_usage_percent)!==null?`${cpu.quota_usage_percent.toFixed(1)}% of allowance in use`:'Waiting for a CPU sample');
    setText($('#worker-ram'),measuredNumber(memory.cgroup_current_bytes)===null?'Unavailable':`${formatBytes(memory.cgroup_current_bytes)}${measuredNumber(memory.cgroup_limit_bytes)!==null?` / ${formatBytes(memory.cgroup_limit_bytes)}`:''}`);
    const meter=$('#worker-memory-meter');meter.value=memory.cgroup_limit_bytes>0?Math.min(1,(memory.cgroup_current_bytes||0)/memory.cgroup_limit_bytes):0;meter.hidden=measuredNumber(memory.cgroup_current_bytes)===null||!(memory.cgroup_limit_bytes>0);
    setText($('#worker-gpu-availability'),data.edition==='cpu'?'CPU only':detected.gpu_available?(detected.gpu_verification==='verified'?'CUDA checked':'Runtime enabled'):'Unavailable');setText($('#worker-gpu-reason'),detected.gpu_reason||'GPU OCR requires the CUDA edition, an enabled runtime and a granted device.');
    setText($('#worker-cpu-capacity'),`${active.cpu_workers||0} active · ${effective.cpu_workers??0} effective slots${data.cpu_limit_reason?` · ${data.cpu_limit_reason}`:''}`);
    setText($('#worker-ocr-capacity'),`${active.ocr_workers||0} active · ${effective.ocr_workers??0} effective slots${data.ocr_limit_reason?` · ${data.ocr_limit_reason}`:''}`);
    setText($('#worker-gpu-capacity'),`${active.gpu_workers||0} active · ${effective.gpu_workers??0} effective slots`);
    if(!state.workersDirty&&!state.workersSaving)setText($('#worker-allocation-note'),paused?'New file work is paused. Resume and save to continue; running files finish safely.':'Allocation is saved in workers.conf. Each tool uses one CPU thread; CPU and RAM limits keep parallel work bounded. GPU slots stay off until you enable them.');
    const body=$('#workers-body');body.replaceChildren();
    for(const worker of (data.workers||[]).slice(0,9)){
      const row=document.createElement('tr');const elapsed=Math.max(0,Math.floor(Date.now()/1000-Number(worker.started||Date.now()/1000)));
      for(const [label,value] of [['Worker',`#${worker.id}`],['Processor',worker.resource==='gpu'?'GPU OCR':worker.resource==='cpu_ocr'?'CPU OCR':'CPU'],['Media file',shortPath(worker.path||'')],['Status',worker.status||'Processing'],['Elapsed',`${Math.floor(elapsed/60)}m ${elapsed%60}s`]]){const cell=document.createElement('td');cell.dataset.label=label;cell.textContent=value;if(label==='Media file'){cell.className='worker-file';cell.title=worker.path||'';}row.append(cell);}body.append(row);
    }
    if(!body.children.length){const row=document.createElement('tr'),cell=document.createElement('td');cell.colSpan=5;cell.className='table-empty';cell.textContent=paused?'New work is paused. Running files will finish before this pool becomes idle.':'No file workers running. Start a scan, extraction, or inventory refresh to use this pool.';row.append(cell);body.append(row);}
    setText($('#worker-live-state'),`Live · ${new Date().toLocaleTimeString([],{hour:'2-digit',minute:'2-digit',second:'2-digit'})}`);
  };
  const loadWorkers = async (quiet=false) => {
    if(!state.csrf||state.workersBusy||state.workersSaving)return;state.workersBusy=true;
    try{state.workersInfo=await api('/api/workers');renderWorkers();}
    catch(error){setText($('#worker-live-state'),'Disconnected · retrying');if(!quiet)notify(`Could not load workers: ${error.message}`,'error');}
    finally{state.workersBusy=false;}
  };
  const saveWorkers = async event => {
    event.preventDefault();if(state.workersSaving||!state.workersInfo)return;
    const allocation={paused:$('#worker-paused').checked};
    for(const [kind,selector] of Object.entries(workerFields)){const input=$(selector),value=Number(input.value);if(!Number.isInteger(value)||value<Number(input.min)||value>Number(input.max)){notify('Choose whole worker counts within the available limits.','error');input.focus();return;}allocation[kind]=value;}
    state.workersSaving=true;$('#save-worker-allocation').disabled=true;renderWorkerSteppers();
    try{state.workersInfo=await api('/api/workers',{method:'PUT',body:JSON.stringify(allocation)});state.workersDirty=false;notify('Worker allocation saved. It applies to new files.','success');}
    catch(error){notify(error.message,'error');}
    finally{state.workersSaving=false;$('#save-worker-allocation').disabled=!state.workersDirty;renderWorkers();if(systemVisible())loadSystem(true);}
  };
  const systemVisible = () => state.currentPage==='settings'&&['status','workers'].includes(state.settingsSection)&&!document.hidden;
  const measuredNumber = value => typeof value==='number'&&Number.isFinite(value)&&value>=0?value:null;
  const formatBytes = value => {
    const bytes=measuredNumber(value);if(bytes===null)return 'Unavailable';
    if(bytes<1024)return `${Math.round(bytes)} B`;
    const unit=bytes>=1073741824?['GiB',1073741824]:bytes>=1048576?['MiB',1048576]:['KiB',1024];
    return `${(bytes/unit[1]).toLocaleString(undefined,{maximumFractionDigits:2})} ${unit[0]}`;
  };
  const renderSystemRows = (container,rows) => {
    const signature=JSON.stringify(rows);if(container.systemSignature===signature)return;
    container.systemSignature=signature;container.replaceChildren();
    for(const [label,value,id] of rows){const term=document.createElement('dt'),detail=document.createElement('dd');term.textContent=label;detail.textContent=String(value??'Unavailable');if(id)detail.id=id;container.append(term,detail);}
  };
  const formatUptime = seconds => {const uptime=Math.max(0,Math.floor(seconds));return `${Math.floor(uptime/86400)}d ${Math.floor(uptime%86400/3600)}h ${Math.floor(uptime%3600/60)}m ${uptime%60}s`;};
  const tickSystemUptime = () => {
    if(!systemVisible()||state.settingsSection!=='status'||!state.systemSampleHealthy||!state.systemInfo)return;
    const elapsed=(performance.now()-state.systemReceivedAt)/1000;
    if(elapsed>15){state.systemSampleHealthy=false;setText($('#system-sample-state'),'Sample stale');return;}
    setText($('#system-uptime'),formatUptime(Number(state.systemInfo.uptime_seconds||0)+elapsed));
  };
  const renderSystem = () => {
    const info=state.systemInfo,policy=info.resource_policy||{},usage=info.resource_usage||{},cpu=usage.cpu||{},memory=usage.memory||{},cache=usage.metadata_cache||{},worker=usage.worker||{};
    updateVersion(info.version,info.edition);
    renderSystemRows($('#system-about'),[
      ['Subforge version',info.version?`v${info.version}`:'Unavailable'],['Image edition',editionLabel(info.edition)],['Runtime',info.runtime||'Unavailable'],
      ['Operating system',[info.os,info.arch].filter(Boolean).join(' · ')||'Unavailable'],
      ['Uptime',formatUptime(Number(info.uptime_seconds)||0),'system-uptime'],['Browser time zone',Intl.DateTimeFormat().resolvedOptions().timeZone]
    ]);
    const cores=measuredNumber(cpu.effective_cores),quota=measuredNumber(cpu.quota_cores);
    const allowance=cores===null?'Unavailable':cpu.limit_unlimited===true?`${cores.toLocaleString()} allowed CPUs · no quota`:`${cores.toLocaleString()} CPU${cores===1?'':'s'}${quota!==null&&quota!==cores?' · quota restricted by CPU affinity':''}`;
    const percent=measuredNumber(cpu.quota_usage_percent),absolute=measuredNumber(cpu.usage_percent);
    const cpuUsage=percent!==null?`${percent.toFixed(1)}% of allowance${absolute!==null?` · ${(absolute/100).toFixed(2)} CPUs used`:''}`:absolute!==null?`${(absolute/100).toFixed(2)} CPUs used`:cpu.usage_sample_state==='collecting'?'Collecting first sample…':'Unavailable';
    const limit=measuredNumber(memory.cgroup_limit_bytes);
    const ram=measuredNumber(memory.cgroup_current_bytes)!==null?`${formatBytes(memory.cgroup_current_bytes)}${limit!==null?` / ${formatBytes(limit)}`:memory.cgroup_limit_unlimited===true?' · no memory limit':''}`:'Unavailable';
    const cacheCount=measuredNumber(cache.entries),cacheMaximum=measuredNumber(cache.max_entries);
    renderSystemRows($('#system-resource-usage'),[
      ['Effective CPU allowance',allowance,'system-cpu-allowance'],['Container CPU usage',cpuUsage,'system-cpu-usage'],
      ['Container RAM',ram,'system-container-ram'],['Rust process RAM',formatBytes(memory.process_rss_bytes),'system-process-ram'],
      ['Active file workers',measuredNumber(worker.active)===null?'Unavailable':`${worker.active} / ${Number(policy.conversion_workers||0)+Number(policy.gpu_workers||0)}${worker.kind?` · ${worker.kind}`:''}`,'system-active-workers'],
      ['Rust process threads',measuredNumber(usage.process?.threads)??'Unavailable','system-process-threads'],
      ['Cached media files',cacheCount===null?'Unavailable':`${cacheCount.toLocaleString()} / ${cacheMaximum?.toLocaleString()??'—'}`,'system-cache-entries'],
      ['Cache payload estimate',`${formatBytes(cache.accounted_bytes)}${measuredNumber(cache.max_bytes)!==null?` / ${formatBytes(cache.max_bytes)}`:''}`,'system-cache-bytes']
    ]);
    renderSystemRows($('#system-limits'),[
      ['Effective CPU workers',policy.conversion_workers??'Unavailable'],['Effective CPU OCR slots',policy.ocr_workers??'Unavailable'],['Effective GPU OCR slots',policy.gpu_workers??0],['CPU threads per tool',policy.ocr_threads??'Unavailable'],
      ['Metadata cache budget',`${formatBytes(policy.metadata_cache_max_bytes)} · ${measuredNumber(policy.metadata_cache_max_entries)?.toLocaleString()??'—'} files maximum`]
    ]);
    setText($('#system-sample-state'),`Live · ${new Date().toLocaleTimeString([],{hour:'2-digit',minute:'2-digit',second:'2-digit'})}`);
    setText($('#system-policy'),`Set parallel file allocations in Settings → Workers. CPU OCR slots share the CPU pool; each tool uses one CPU thread. Docker's CPU and memory limits remain the hard ceiling, while memory admission may reduce simultaneous work. Discovery and import hooks convert text and queue new or changed image tracks for scheduled OCR${policy.ocr_permission?'':'; OCR is disabled by deployment permissions'}. Changing a Docker limit requires recreating the container. Unsupported measurements are shown as unavailable.`);
  };
  const loadSystem = async (quiet=false) => {
    if(!state.csrf||state.systemBusy)return;state.systemBusy=true;
    try{state.systemInfo=await api('/api/system');state.systemReceivedAt=performance.now();state.systemSampleHealthy=true;renderSystem();if(workersVisible())renderWorkers();}
    catch(error){state.systemSampleHealthy=false;setText($('#system-sample-state'),'Disconnected · retrying');if(!quiet)notify(`Could not load system status: ${error.message}`,'error');}
    finally{state.systemBusy=false;}
  };
  const syncSystemMonitor = () => {
    clearTimeout(state.systemTimer);clearInterval(state.systemClock);
    if(!systemVisible())return;
    tickSystemUptime();state.systemClock=setInterval(tickSystemUptime,1000);
    state.systemTimer=setTimeout(async()=>{await Promise.all([loadSystem(true),workersVisible()?loadWorkers(true):Promise.resolve()]);syncSystemMonitor();},3000);
  };
  const pageNames={overview:'Overview',library:'Library',activity:'Activity',settings:'Settings'};
  const settingsSections=['general','media','tasks','workers','status','sonarr','radarr','jellyfin','bazarr'];
  state.settingsSection='general';
  const routeHash = (page,section) => `#/${page}${page==='settings'?`/${section}`:''}`;
  const readRoute = () => {
    const parts=location.hash.replace(/^#\/?/,'').replace(/\/+$/,'').split('/');
    const page=parts[0] || 'overview';
    if(page==='system'&&parts.length<=2&&['tasks','status'].includes(parts[1]||'tasks'))return {page:'settings',section:parts[1]||'tasks'};
    if(!Object.hasOwn(pageNames,page))return {page:'overview',section:'general'};
    if(page==='settings') {
      const section=parts[1] || 'general';
      if(parts.length<=2&&settingsSections.includes(section))return {page,section};
    } else if(parts.length===1)return {page,section:'general'};
    return {page:'overview',section:'general'};
  };
  const syncNavigation = () => {const mobile=window.matchMedia('(max-width: 800px)').matches,open=$('#sidebar').classList.contains('mobile-open'),button=$('#mobile-menu'),collapse=$('#sidebar-collapse');$('.shell').classList.toggle('sidebar-collapsed',!mobile&&state.sidebarCollapsed);$('#sidebar').inert=mobile&&!open;const mobileLabel=open?'Close navigation':'Open navigation',desktopLabel=state.sidebarCollapsed?'Expand sidebar':'Collapse sidebar';button.setAttribute('aria-expanded',String(mobile&&open));button.setAttribute('aria-label',mobileLabel);button.title=mobileLabel;collapse.setAttribute('aria-expanded',String(!state.sidebarCollapsed));collapse.setAttribute('aria-label',desktopLabel);collapse.title=desktopLabel;};
  const closeMobileMenu = (restoreFocus=false) => { const sidebar=$('#sidebar'),button=$('#mobile-menu'),backdrop=$('.sidebar-backdrop'); sidebar.classList.remove('mobile-open');backdrop.classList.remove('visible');backdrop.setAttribute('aria-hidden','true');syncNavigation();if(restoreFocus)button.focus(); };
  const toggleMobileMenu = () => {if(!window.matchMedia('(max-width: 800px)').matches){state.sidebarCollapsed=!state.sidebarCollapsed;try{localStorage.setItem('subforge.sidebar-collapsed',String(state.sidebarCollapsed));}catch{}syncNavigation();return;}const sidebar=$('#sidebar'),button=$('#mobile-menu'),backdrop=$('.sidebar-backdrop'),open=!sidebar.classList.contains('mobile-open');sidebar.classList.toggle('mobile-open',open);backdrop.classList.toggle('visible',open);backdrop.setAttribute('aria-hidden',String(!open));syncNavigation();if(open)$('.nav-link[data-page]',sidebar)?.focus();else button.focus();};
  const showRoute = ({page,section}) => {
    const fromMobileMenu=$('#sidebar').classList.contains('mobile-open');
    const previousPage=document.activeElement.closest('.page');
    const changed=state.currentPage!==page||(page==='settings'&&state.settingsSection!==section);
    state.currentPage=page;
    if(page==='settings')state.settingsSection=section;
    $$('.page').forEach(node=>node.classList.toggle('active',node.id===`page-${page}`));
    $$('.nav-link[data-page],.text-button[data-page]').forEach(node=>node.classList.toggle('active',node.dataset.page===page));
    $$('.settings-tab').forEach(node=>node.classList.toggle('active',node.dataset.settingTab===state.settingsSection));
    $$('.settings-section').forEach(node=>node.classList.toggle('active',node.id===`settings-${state.settingsSection}`));
    setText($('#page-crumb'),pageNames[page]);closeMobileMenu();
    if(fromMobileMenu||(previousPage&&previousPage.id!==`page-${page}`)) {
      const heading=$(`#page-${page} h1`);heading?.setAttribute('tabindex','-1');heading?.focus({preventScroll:true});
    }
    if(page==='settings'&&state.csrf) {
      if(!state.settings)loadSettings();
      if(changed&&section==='media')loadRootFolders();
    }
    const administration=page==='settings'&&['tasks','workers','status'].includes(section);$('#refresh-system').classList.toggle('hidden',!administration);$('#save-settings').classList.toggle('hidden',administration);
    if(administration&&state.csrf&&changed){if(section==='tasks')loadTasks();else{loadSystem();if(section==='workers')loadWorkers();}}
    syncSystemMonitor();
  };
  const restoreRoute = () => {
    const route=readRoute(),hash=routeHash(route.page,route.section);
    if(location.hash!==hash)history.replaceState(null,'',hash);
    showRoute(route);
  };
  const navigate = (page,section=state.settingsSection) => {
    if(!Object.hasOwn(pageNames,page)||(page==='settings'&&!settingsSections.includes(section)))return;
    const hash=routeHash(page,section);
    if(location.hash!==hash)history.pushState(null,'',hash);
    showRoute({page,section});
  };
  const cancelJob = async id => { try { const result=await api(`/api/jobs/${encodeURIComponent(id)}/cancel`,{method:'POST',body:'{}'}); notify(result.message || 'Cancellation requested.'); await pollStatus(); } catch(error) { notify(error.message,'error'); } };
  const bind = () => {
    try{const saved=JSON.parse(localStorage.getItem('subforge.collapsed-panels')||'[]');if(Array.isArray(saved))state.collapsedPanels=new Set(saved.filter(key=>typeof key==='string'));}catch{}
    $$('[data-collapse]').forEach(makePanelCollapsible);
    const mobileBackdrop=document.createElement('button'); mobileBackdrop.type='button'; mobileBackdrop.className='sidebar-backdrop'; mobileBackdrop.setAttribute('aria-label','Close navigation'); mobileBackdrop.setAttribute('aria-hidden','true'); document.body.append(mobileBackdrop);try{state.sidebarCollapsed=localStorage.getItem('subforge.sidebar-collapsed')==='true';}catch{}syncNavigation();
    $('.brand')?.addEventListener('click',event=>{event.preventDefault();navigate('overview');});
    $$('.nav-link[data-page],.text-button[data-page]').forEach(node=>node.addEventListener('click',()=>navigate(node.dataset.page)));
    $('#mobile-menu').addEventListener('click',toggleMobileMenu); $('#sidebar-collapse').addEventListener('click',toggleMobileMenu); mobileBackdrop.addEventListener('click',()=>closeMobileMenu(true));
    window.addEventListener('resize',()=>{if(!window.matchMedia('(max-width: 800px)').matches)closeMobileMenu();else syncNavigation();});
    $('#refresh-button').addEventListener('click',()=>{pollStatus();if(systemVisible())loadSystem();if(workersVisible())loadWorkers();}); $('#clear-view-button').addEventListener('click',()=>pollStatus());
    $$('[data-action="scan"]').forEach(button=>button.addEventListener('click',()=>startJob({kind:'scan'})));
    $$('[data-action="inspect"]').forEach(button=>button.addEventListener('click',async()=>{navigate('library');await inspect();}));
    $('#quick-form').addEventListener('submit',event=>{event.preventDefault();startJob({kind:'scan',rootIds:selectedRootIds('#quick-root')});});
    restoreInventoryView();
    const resetInventoryPage=()=>{state.inventoryPage=1;renderInventory();saveInventoryView();};
    $('#inventory-filter').addEventListener('change',resetInventoryPage); $('#inventory-root').addEventListener('change',resetInventoryPage); $('#inventory-search').addEventListener('input',resetInventoryPage);
    $('#inventory-page-size').addEventListener('change',()=>{state.inventoryPageSize=Number($('#inventory-page-size').value);resetInventoryPage();});
    $('#inventory-prev').addEventListener('click',()=>{state.inventoryPage--;renderInventory();saveInventoryView();});$('#inventory-next').addEventListener('click',()=>{state.inventoryPage++;renderInventory();saveInventoryView();});$('#inventory-page').addEventListener('change',()=>{state.inventoryPage=Number($('#inventory-page').value);renderInventory();saveInventoryView();});
    $('#jobs-list').addEventListener('click',event=>{const button=event.target.closest('.cancel-job');if(button)cancelJob(button.dataset.jobId);}); $('#recent-activity').addEventListener('click',event=>{const button=event.target.closest('.cancel-job');if(button)cancelJob(button.dataset.jobId);});
    $('#save-settings').addEventListener('click',saveSettings); $('#language-search').addEventListener('input',renderLanguageList); $('#add-language').addEventListener('click',addCustomLanguage); $('#custom-language').addEventListener('keydown',event=>{if(event.key==='Enter'){event.preventDefault();addCustomLanguage();}});
    $$('[data-open-languages]').forEach(button=>button.addEventListener('click',()=>openLanguageDialog(button.dataset.openLanguages)));
    $('#apply-languages').addEventListener('click',()=>{if(!state.draftLanguages.length){notify('Select at least one language or choose All languages.','error');return;}state.selectedLanguages=[...state.draftLanguages];updateLanguageLabels();const backdrop=$('#language-modal');closeDialog(backdrop);if(state.languageTarget==='settings')setText($('#settings-save-note'),'Save changes to keep this default.');});
    $$('.modal-close').forEach(button=>button.addEventListener('click',()=>closeDialog(button.closest('.modal-backdrop'))));
    $$('.modal-backdrop').forEach(backdrop=>backdrop.addEventListener('click',event=>{if(event.target===backdrop)closeDialog(backdrop);}));
    $('#add-root-folder').addEventListener('click',()=>openRootFolderDialog());$('#refresh-root-folders').addEventListener('click',loadRootFolders);
    $('#folder-mount').addEventListener('change',()=>browseRootFolder($('#folder-mount').value));$('#browse-root-folder').addEventListener('click',()=>browseRootFolder($('#folder-path').value.trim()));$('#folder-path').addEventListener('input',()=>{state.folderRequest++;state.currentFolder=null;$('#select-root-folder').disabled=true;setText($('#folder-browser-note'),'Browse to check this folder before adding it.');});$('#folder-path').addEventListener('keydown',event=>{if(event.key==='Enter'){event.preventDefault();browseRootFolder($('#folder-path').value.trim());}});
    $('#folder-parent-button').addEventListener('click',()=>{if(state.folderParent)browseRootFolder(state.folderParent);});$('#select-root-folder').addEventListener('click',()=>{if(state.currentFolder)saveRootFolders([...state.rootFolders.roots.map(root=>root.path),state.currentFolder]);});
    $$('.settings-tab').forEach(button=>button.addEventListener('click',()=>navigate('settings',button.dataset.settingTab)));
    $('#refresh-system').addEventListener('click',()=>{if(state.settingsSection==='tasks')loadTasks();else{loadSystem();if(workersVisible())loadWorkers();}});
    $('#worker-allocation-form').addEventListener('submit',saveWorkers);
    $('#worker-paused').addEventListener('change',workerDirty);
    for(const selector of Object.values(workerFields))$(selector).addEventListener('input',workerDirty);
    $$('[data-worker-step]').forEach(button=>button.addEventListener('click',()=>{const input=$(workerFields[button.dataset.workerStep]),next=Number(input.value)+Number(button.dataset.delta);input.value=String(Math.max(Number(input.min),Math.min(Number(input.max),next)));workerDirty();}));
    $('#pause-schedules').addEventListener('change',markTasksDirty);
    $('#save-task-schedules').addEventListener('click',saveTasks);
    window.addEventListener('popstate',restoreRoute);window.addEventListener('hashchange',restoreRoute);
    document.addEventListener('keydown',event=>{
      const modal=$$('.modal-backdrop').find(item=>!item.classList.contains('hidden'));
      if(event.key==='Escape'){if(modal){event.preventDefault();closeDialog(modal);}else if($('#sidebar').classList.contains('mobile-open')){event.preventDefault();closeMobileMenu(true);}return;}
      if(event.key!=='Tab')return;
      if(modal){const focusable=$$('button:not(:disabled),input:not(:disabled),select:not(:disabled),[href],[tabindex]:not([tabindex="-1"])',modal).filter(node=>node.getClientRects().length);if(!focusable.length)return;const first=focusable[0],last=focusable.at(-1);if(event.shiftKey&&document.activeElement===first){event.preventDefault();last.focus();}else if(!event.shiftKey&&document.activeElement===last){event.preventDefault();first.focus();}return;}
      if($('#sidebar').classList.contains('mobile-open')){const focusable=$$('button:not(:disabled),a[href],input:not(:disabled),select:not(:disabled)', $('#sidebar')).filter(node=>!node.closest('[inert]')&&node.getClientRects().length);if(!focusable.length)return;const first=focusable[0],last=focusable.at(-1);if(event.shiftKey&&document.activeElement===first){event.preventDefault();last.focus();}else if(!event.shiftKey&&document.activeElement===last){event.preventDefault();first.focus();}}
    });
  };
  const scheduleStatusPoll = () => {clearTimeout(state.pollTimer);const busy=!!state.status?.running||!!state.status?.queue?.length;state.pollTimer=setTimeout(async()=>{await pollStatus();scheduleStatusPoll();},document.hidden?60000:busy?3000:15000);};
  const initialize = async () => { bind();restoreRoute(); try { const session=await api('/api/session'); state.csrf=session.csrf_token || ''; await Promise.all([pollStatus(),loadSettings()]);if(state.currentPage==='settings'&&state.settingsSection==='media')await loadRootFolders();if(state.currentPage==='settings'&&state.settingsSection==='tasks')await loadTasks();if(systemVisible())await loadSystem();if(workersVisible())await loadWorkers(); } catch(error) { setConnection(false); notify(`Service initialization failed: ${error.message}`,'error'); } scheduleStatusPoll();syncSystemMonitor();document.addEventListener('visibilitychange',async()=>{syncSystemMonitor();if(!document.hidden){await pollStatus();if(systemVisible())await loadSystem();if(workersVisible())await loadWorkers();}scheduleStatusPoll();}); };
  initialize();
})();
