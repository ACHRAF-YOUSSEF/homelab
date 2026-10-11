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
  const renderRootSelectors = roots => {
    const rootList = $('#root-list'); rootList.replaceChildren();
    const quick = $('#quick-root'), inventoryRoot = $('#inventory-root');
    const keepQuick = quick.value, keepInventory = inventoryRoot.value;
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
    const title = document.createElement('strong'); title.textContent = `${({scan:'Library scan',inspect:'Library inspection',extract:'Subtitle extraction'})[job.kind] || 'Converter job'}`;
    const sub = document.createElement('span'); sub.className = 'job-subtitle'; sub.textContent = currentLabel(job.current) || (job.paths && job.paths.length ? shortPath(job.paths[0]) : job.root_label || 'All scan roots');
    detail.append(title,sub);
    const status = document.createElement('div'); status.className = 'job-status-block';
    const pill = document.createElement('span'); pill.className=`status-pill ${escStatus(job.status)}`; pill.textContent=(job.status || 'queued').replaceAll('_',' ');
    const time=document.createElement('span'); time.className='job-time'; time.textContent=fmtDate(job.finished || job.started || job.created);
    status.append(pill,time);
    card.append(icon,detail,status);
    if (job.status === 'running' || job.status === 'queued') {
      const progress = document.createElement('div'); progress.className='job-progress'; const bar=document.createElement('span'); bar.style.width=`${jobPercent(job)}%`; progress.append(bar); card.append(progress);
      if (job.id) { const cancel=document.createElement('button'); cancel.className='text-button cancel-job'; cancel.textContent='Cancel'; cancel.dataset.jobId=job.id; card.append(cancel); }
    } else if (job.summary) {
      const chips=document.createElement('div'); chips.className='job-summary';
      for (const [key,label] of [['converted','converted'],['cached','already ready'],['pending','pending'],['planned','planned'],['inspected','inspected'],['failed','failed']]) if (Number(job.summary[key] || 0)) { const chip=document.createElement('span'); chip.textContent=`${job.summary[key]} ${label}`; chips.append(chip); }
      if (chips.childNodes.length) card.append(chips);
    }
    const failures=[...(job.error?[job.error]:[]),...(job.notifications||[]).filter(item=>item.ok===false).map(item=>item.message),...(job.results||[]).filter(item=>item.ok===false).map(item=>item.message)].filter(Boolean);
    if (failures.length) { const error=document.createElement('div'); error.className='job-error'; error.textContent=failures.join(' · '); card.append(error); }
    return card;
  };
  const renderJobs = () => {
    const jobs=jobList(), running=state.status?.running;
    const recent=$('#recent-activity'); recent.replaceChildren();
    for (const job of jobs.slice(0,4)) recent.append(renderJobCard(job,true));
    if (!jobs.length) { const empty=document.createElement('div'); empty.className='empty-state compact'; const icon=document.createElement('span'); icon.className='empty-icon'; icon.textContent='◷'; const title=document.createElement('strong'); title.textContent='No activity yet'; const hint=document.createElement('span'); hint.textContent='Start a scan to see progress here.'; empty.append(icon,title,hint); recent.append(empty); }
    const list=$('#jobs-list'); list.replaceChildren();
    for (const job of jobs) list.append(renderJobCard(job));
    if (!jobs.length) { const empty=document.createElement('div'); empty.className='empty-state'; const icon=document.createElement('span'); icon.className='empty-icon'; icon.textContent='◷'; const title=document.createElement('strong'); title.textContent='No jobs yet'; const hint=document.createElement('span'); hint.textContent='Start a scan or extraction to see it here.'; empty.append(icon,title,hint); list.append(empty); }
    setText($('#stat-running'),running ? 'Running' : 'Idle'); setText($('#stat-current'),currentLabel(running?.current) || (running ? 'Working through your library' : 'Nothing running right now'));
    const total=jobs.reduce((sum,job)=>sum+Number(job.summary?.converted || 0),0); setText($('#stat-converted'),total.toLocaleString());
    const queued=(state.status?.queue || []).length; const badge=$('#activity-badge'); badge.textContent=queued ? String(queued) : ''; badge.classList.toggle('hidden',!queued);
  };
  const renderLogs = logs => {
    const box=$('#live-logs'); box.replaceChildren();
    const entries=(logs || []).slice(-50);
    if (!entries.length) { const empty=document.createElement('div'); empty.className='log-empty'; empty.textContent='No recent converter events'; box.append(empty); return; }
    for (const entry of entries) {
      const row=document.createElement('div'); row.className='log-row';
      const time=document.createElement('time'); time.textContent=entry.time ? new Date(entry.time).toLocaleTimeString([], {hour:'2-digit',minute:'2-digit',second:'2-digit'}) : '—';
      const level=document.createElement('span'); level.className=`log-level ${escStatus(entry.level)}`; level.textContent=(entry.level || 'INFO').toUpperCase();
      const message=document.createElement('span'); message.className='log-message'; message.textContent=entry.message || '';
      row.append(time,level,message); box.append(row);
    }
    box.scrollTop=box.scrollHeight;
  };
  const setConnection = good => { $('#connection-dot').classList.toggle('offline',!good); setText($('#connection-text'),good ? 'Service connected' : 'Service unavailable'); };
  const pollStatus = async () => {
    if (state.pollBusy || !state.csrf) return; state.pollBusy=true;
    try { state.status=await api('/api/status'); setConnection(true); renderRootSelectors(state.status.roots || []); renderJobs(); renderLogs(state.status.logs); const inspection=[...(state.status.history||[])].find(job=>job.kind==='inspect'&&Array.isArray(job.results)); if(inspection) { state.inventory=inspection.results; renderInventory(); } setText($('#last-updated'),`Updated ${new Date().toLocaleTimeString([], {hour:'2-digit',minute:'2-digit'})}`); updateHookStates(); }
    catch (error) { setConnection(false); if (!state.status) notify(`Could not load converter status: ${error.message}`,'error'); }
    finally { state.pollBusy=false; }
  };
  const startJob = async ({kind,rootIds=[],paths=[],languages: selected=state.selectedLanguages,retry=true}) => {
    try { const result=await api('/api/jobs',{method:'POST',body:JSON.stringify({kind,root_ids:rootIds,paths,languages:selected,retry})}); notify(result.message || 'Job queued.'); await pollStatus(); return result; }
    catch (error) { notify(error.message,'error'); return null; }
  };
  const selectedRootIds = selector => { const value=$(selector).value; return value ? [value] : []; };
  const renderInventory = () => {
    const body=$('#inventory-body'); body.replaceChildren();
    const filter=$('#inventory-filter').value, root=$('#inventory-root').value;
    let rows=state.inventory.filter(row=>!root || row.root_id===root);
    if (filter==='video'||filter==='sidecar') rows=rows.filter(row=>row.type===filter);
    if (filter==='issues') rows=rows.filter(row=>row.error || row.type==='video' && !(row.tracks || []).length);
    setText($('#inventory-meta'),`${rows.length} item${rows.length===1?'':'s'}${root?' in selected root':''}`);
    if (!rows.length) { const tr=document.createElement('tr'),td=document.createElement('td'); td.colSpan=5; td.className='table-empty'; td.textContent=state.inventory.length ? 'No files match this filter.' : 'No inventory yet. Choose Inspect library to list available subtitle tracks.'; tr.append(td); body.append(tr); return; }
    for (const row of rows) {
      const tr=document.createElement('tr'); const name=document.createElement('td'); name.className='file-cell'; const fileIcon=document.createElement('span'); fileIcon.className=`file-icon ${row.type==='sidecar'?'srt':''}`; fileIcon.textContent=row.type==='sidecar'?'S':'▣'; const filename=document.createElement('span'); filename.className='file-name'; filename.textContent=row.name || row.path; filename.title=row.path || row.name; name.append(fileIcon,filename);
      const type=document.createElement('td'); const badge=document.createElement('span'); badge.className='type-badge'; badge.textContent=row.error?'Issue':row.type==='sidecar'?'Sidecar':'Video'; type.append(badge);
      const tracks=document.createElement('td'); tracks.textContent=row.error ? row.error : `${(row.tracks || []).length} track${(row.tracks || []).length===1?'':'s'}`;
      const lang=document.createElement('td'); lang.className='language-tags'; for (const item of [...new Set((row.tracks || []).map(track=>track.language || 'und'))].slice(0,4)) { const tag=document.createElement('span'); tag.textContent=langLabel(item); lang.append(tag); } if (!lang.childNodes.length) lang.textContent='—';
      const action=document.createElement('td'); if (!row.error && (row.type==='sidecar' || row.type==='video' && row.tracks?.length)) { const button=document.createElement('button'); button.className='row-action'; button.textContent='Extract'; button.title='Extract selected languages from this file'; button.addEventListener('click',()=>startJob({kind:'extract',paths:[row.path],rootIds:row.root_id?[row.root_id]:[]})); action.append(button); } else if (row.error) { const issue=document.createElement('span'); issue.className='issue-dot'; issue.textContent='!'; issue.title=row.error; action.append(issue); } else action.textContent='—';
      tr.append(name,type,tracks,lang,action); body.append(tr);
    }
  };
  const inspect = async () => { const result=await startJob({kind:'inspect',rootIds:selectedRootIds('#inventory-root')}); if (!result) return; if (Array.isArray(result.results)) { state.inventory=result.results; renderInventory(); } };
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
  const openLanguageDialog = target => { state.languageTarget=target; state.modalTrigger=document.activeElement; state.draftLanguages=[...state.selectedLanguages]; $('#language-search').value=''; $('#language-modal').classList.remove('hidden'); renderLanguageList(); $('#language-search').focus(); };
  const closeDialog = backdrop => { if (!backdrop || backdrop.classList.contains('hidden')) return; backdrop.classList.add('hidden'); if (state.modalTrigger?.isConnected) state.modalTrigger.focus(); };
  const addCustomLanguage = () => { const input=$('#custom-language'), code=input.value.trim().toLowerCase(); if (!/^[a-z]{2,3}(?:-[a-z0-9]{2,8})*$/.test(code)) { notify('Enter a valid ISO language code, such as pl or pt-BR.','error'); return; } languages.push([code,code.toUpperCase()]); state.draftLanguages=isAll(state.draftLanguages)?[code]:[...new Set([...state.draftLanguages,code])]; input.value=''; renderLanguageList(); };
  const buildIntegrationCard = (provider, config={}) => {
    const card=$('#integration-template').content.firstElementChild.cloneNode(true); card.dataset.provider=provider;
    const title={sonarr:'Sonarr',radarr:'Radarr',jellyfin:'Jellyfin'}[provider]; const description={sonarr:'Trigger subtitle scans when episodes are imported, and notify Sonarr when sidecars are ready.',radarr:'Trigger subtitle scans when movies are imported, and notify Radarr when sidecars are ready.',jellyfin:'Refresh selected Jellyfin libraries after subtitle extraction.'}[provider];
    setText($('.integration-title',card),title); setText($('.integration-description',card),description);
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
      $('.integration-url',card).placeholder=provider==='sonarr'?'http://sonarr:8989':'http://radarr:7878';
      $('.integration-key',card).placeholder='API key'; renderMappings(card,(state.settings?.path_mappings || []).filter(item=>!item.provider || item.provider===provider));
    }
    $('.test-integration',card).addEventListener('click',()=>testIntegration(provider,card));
    $('.refresh-integration',card).addEventListener('click',()=>refreshIntegration(provider,card));
    $$('.load-libraries',card).forEach(button=>button.addEventListener('click',()=>loadLibraries(card)));
    $('.add-path-map',card).addEventListener('click',()=>{const entries=readMappings(card); entries.push({remote:'',local:''});renderMappings(card,entries);});
    return card;
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
  const persistSettings = async () => { const data=await api('/api/settings',{method:'PUT',body:JSON.stringify(gatherSettings())}); state.settings=data; return data; };
  const loadSettings = async () => {
    try {
      const data=await api('/api/settings'); state.settings=data; state.selectedLanguages=data.languages || ['*'];
      for (const provider of ['sonarr','radarr','jellyfin']) { const node=$(`#${provider}-settings`); node.replaceChildren(buildIntegrationCard(provider,{...integrationDefaults(),...(data.integrations?.[provider] || {})})); }
      updateLanguageLabels(); updateHookStates();
    } catch(error) { notify(`Could not load settings: ${error.message}`,'error'); }
  };
  const gatherSettings = () => {
    const integrations={}; const path_mappings=[];
    for (const provider of ['sonarr','radarr','jellyfin']) {
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
    try { const data=await persistSettings(); for (const provider of ['sonarr','radarr','jellyfin']) {const node=$(`#${provider}-settings`);node.replaceChildren(buildIntegrationCard(provider,{...integrationDefaults(),...(data.integrations?.[provider] || {})}));} state.selectedLanguages=data.languages || state.selectedLanguages; updateLanguageLabels(); setText(note,'Changes saved'); notify('Settings saved.','success'); }
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
  const pageNames={overview:'Overview',library:'Library',activity:'Activity',settings:'Settings'};
  const closeMobileMenu = (restoreFocus=false) => { const sidebar=$('#sidebar'),button=$('#mobile-menu'),backdrop=$('.sidebar-backdrop'); sidebar.classList.remove('mobile-open'); sidebar.inert=window.matchMedia('(max-width: 800px)').matches; button.setAttribute('aria-expanded','false'); button.setAttribute('aria-label','Open navigation'); backdrop.classList.remove('visible'); backdrop.setAttribute('aria-hidden','true'); if(restoreFocus)button.focus(); };
  const toggleMobileMenu = () => { const sidebar=$('#sidebar'),button=$('#mobile-menu'),backdrop=$('.sidebar-backdrop'),open=!sidebar.classList.contains('mobile-open'); sidebar.classList.toggle('mobile-open',open); sidebar.inert=!open; button.setAttribute('aria-expanded',String(open)); button.setAttribute('aria-label',open?'Close navigation':'Open navigation'); backdrop.classList.toggle('visible',open); backdrop.setAttribute('aria-hidden',String(!open)); if(open)$('.nav-link[data-page]',sidebar)?.focus(); else button.focus(); };
  const navigate = page => { if (!pageNames[page]) return; const fromMobileMenu=$('#sidebar').classList.contains('mobile-open'); state.currentPage=page; $$('.page').forEach(node=>node.classList.toggle('active',node.id===`page-${page}`)); $$('.nav-link[data-page]').forEach(node=>node.classList.toggle('active',node.dataset.page===page)); $$('.text-button[data-page]').forEach(node=>node.classList.toggle('active',node.dataset.page===page)); setText($('#page-crumb'),pageNames[page]); closeMobileMenu(); if(fromMobileMenu){const heading=$(`#page-${page} h1`);heading?.setAttribute('tabindex','-1');heading?.focus({preventScroll:true});} if(page==='settings'&&!state.settings)loadSettings(); };
  const cancelJob = async id => { try { const result=await api(`/api/jobs/${encodeURIComponent(id)}/cancel`,{method:'POST',body:'{}'}); notify(result.message || 'Cancellation requested.'); await pollStatus(); } catch(error) { notify(error.message,'error'); } };
  const bind = () => {
    const mobileBackdrop=document.createElement('button'); mobileBackdrop.type='button'; mobileBackdrop.className='sidebar-backdrop'; mobileBackdrop.setAttribute('aria-label','Close navigation'); mobileBackdrop.setAttribute('aria-hidden','true'); document.body.append(mobileBackdrop); $('#sidebar').inert=window.matchMedia('(max-width: 800px)').matches;
    $('.brand')?.addEventListener('click',event=>{event.preventDefault();navigate('overview');});
    $$('.nav-link[data-page],.text-button[data-page]').forEach(node=>node.addEventListener('click',()=>navigate(node.dataset.page)));
    $('#mobile-menu').setAttribute('aria-controls','sidebar'); $('#mobile-menu').setAttribute('aria-expanded','false'); $('#mobile-menu').addEventListener('click',toggleMobileMenu); mobileBackdrop.addEventListener('click',()=>closeMobileMenu(true));
    window.addEventListener('resize',()=>{if(!window.matchMedia('(max-width: 800px)').matches)closeMobileMenu();else if(!$('#sidebar').classList.contains('mobile-open'))$('#sidebar').inert=true;});
    $('#refresh-button').addEventListener('click',()=>pollStatus()); $('#clear-view-button').addEventListener('click',()=>pollStatus());
    $$('[data-action="scan"]').forEach(button=>button.addEventListener('click',()=>startJob({kind:'scan'})));
    $$('[data-action="inspect"]').forEach(button=>button.addEventListener('click',async()=>{navigate('library');await inspect();}));
    $('#quick-form').addEventListener('submit',event=>{event.preventDefault();startJob({kind:'scan',rootIds:selectedRootIds('#quick-root')});});
    $('#inventory-filter').addEventListener('change',renderInventory); $('#inventory-root').addEventListener('change',renderInventory);
    $('#jobs-list').addEventListener('click',event=>{const button=event.target.closest('.cancel-job');if(button)cancelJob(button.dataset.jobId);}); $('#recent-activity').addEventListener('click',event=>{const button=event.target.closest('.cancel-job');if(button)cancelJob(button.dataset.jobId);});
    $('#save-settings').addEventListener('click',saveSettings); $('#language-search').addEventListener('input',renderLanguageList); $('#add-language').addEventListener('click',addCustomLanguage); $('#custom-language').addEventListener('keydown',event=>{if(event.key==='Enter'){event.preventDefault();addCustomLanguage();}});
    $$('[data-open-languages]').forEach(button=>button.addEventListener('click',()=>openLanguageDialog(button.dataset.openLanguages)));
    $('#apply-languages').addEventListener('click',()=>{if(!state.draftLanguages.length){notify('Select at least one language or choose All languages.','error');return;}state.selectedLanguages=[...state.draftLanguages];updateLanguageLabels();const backdrop=$('#language-modal');closeDialog(backdrop);if(state.languageTarget==='settings')setText($('#settings-save-note'),'Save changes to keep this default.');});
    $$('.modal-close').forEach(button=>button.addEventListener('click',()=>closeDialog(button.closest('.modal-backdrop'))));
    $$('.modal-backdrop').forEach(backdrop=>backdrop.addEventListener('click',event=>{if(event.target===backdrop)closeDialog(backdrop);}));
    $$('.settings-tab').forEach(button=>button.addEventListener('click',()=>{const tab=button.dataset.settingTab;$$('.settings-tab').forEach(item=>item.classList.toggle('active',item===button));$$('.settings-section').forEach(item=>item.classList.toggle('active',item.id===`settings-${tab}`));}));
    document.addEventListener('keydown',event=>{
      const modal=$$('.modal-backdrop').find(item=>!item.classList.contains('hidden'));
      if(event.key==='Escape'){if(modal){event.preventDefault();closeDialog(modal);}else if($('#sidebar').classList.contains('mobile-open')){event.preventDefault();closeMobileMenu(true);}return;}
      if(event.key!=='Tab')return;
      if(modal){const focusable=$$('button:not(:disabled),input:not(:disabled),select:not(:disabled),[href],[tabindex]:not([tabindex="-1"])',modal).filter(node=>!node.closest('.hidden'));if(!focusable.length)return;const first=focusable[0],last=focusable.at(-1);if(event.shiftKey&&document.activeElement===first){event.preventDefault();last.focus();}else if(!event.shiftKey&&document.activeElement===last){event.preventDefault();first.focus();}return;}
      if($('#sidebar').classList.contains('mobile-open')){const focusable=$$('button:not(:disabled),a[href],input:not(:disabled),select:not(:disabled)', $('#sidebar')).filter(node=>!node.closest('[inert]'));if(!focusable.length)return;const first=focusable[0],last=focusable.at(-1);if(event.shiftKey&&document.activeElement===first){event.preventDefault();last.focus();}else if(!event.shiftKey&&document.activeElement===last){event.preventDefault();first.focus();}}
    });
  };
  const initialize = async () => { bind(); try { const session=await api('/api/session'); state.csrf=session.csrf_token || ''; await Promise.all([pollStatus(),loadSettings()]); } catch(error) { setConnection(false); notify(`Service initialization failed: ${error.message}`,'error'); } setInterval(pollStatus,2000); };
  initialize();
})();
