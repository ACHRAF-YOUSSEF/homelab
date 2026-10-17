(() => {
  'use strict';
  const $ = (selector, root = document) => root.querySelector(selector);
  const $$ = (selector, root = document) => [...root.querySelectorAll(selector)];
  const languages = [
    ['en','English'],['fr','French'],['ar','Arabic'],['de','German'],['es','Spanish'],['it','Italian'],['pt','Portuguese'],['ja','Japanese'],['zh','Chinese'],['ko','Korean'],['ru','Russian'],['nl','Dutch'],['tr','Turkish'],['und','Unknown / untagged'],
    ['af','Afrikaans'],['sq','Albanian'],['am','Amharic'],['hy','Armenian'],['az','Azerbaijani'],['eu','Basque'],['bn','Bengali'],['bs','Bosnian'],['bg','Bulgarian'],['my','Burmese'],['ca','Catalan'],['hr','Croatian'],['cs','Czech'],['da','Danish'],['et','Estonian'],['fi','Finnish'],['el','Greek'],['he','Hebrew'],['hi','Hindi'],['hu','Hungarian'],['id','Indonesian'],['ga','Irish'],['is','Icelandic'],['kk','Kazakh'],['km','Khmer'],['lo','Lao'],['lv','Latvian'],['lt','Lithuanian'],['mk','Macedonian'],['ms','Malay'],['no','Norwegian'],['fa','Persian'],['pl','Polish'],['ro','Romanian'],['sr','Serbian'],['sk','Slovak'],['sl','Slovenian'],['sw','Swahili'],['sv','Swedish'],['th','Thai'],['uk','Ukrainian'],['ur','Urdu'],['vi','Vietnamese']
  ];
  const state = { csrf: '', status: null, settings: null, selectedLanguages: ['*'], draftLanguages: ['*'], languageTarget: 'quick', inventory: [], selectedFile: null, currentPage: 'overview', lastToast: 0, pollBusy: false };
  state.inventorySource='filesystem';
  state.inventoryAuthority={status:'ready'};
  state.defaultLanguages=['*'];
  state.quickLanguagesChanged=false;
  state.settingsDirty=false;
  state.pendingConversion=null;
  state.jellyfinLibraries=[];
  state.sidebarCollapsed=false;
  state.rootFolders={roots:[],mounts:[]};
  state.folderRequest=0;
  state.inventoryPage=1;
  state.inventoryPageSize='auto';
  state.selectedMedia=new Set();
  state.jobsPage=1;
  state.jobsPageSize='auto';
  state.tableAutoSizes={library:10,activity:10};
  state.tableFitMeasurements={};
  state.tableFrame=0;
  state.selectedJobId=null;
  state.inventoryTruncated=false;
  state.inventoryUpdated=null;
  state.inventoryLoaded=false;
  state.inventoryBusy=false;
  state.inventoryRevision=null;
  state.drawerSelection=new Set();
  state.integrationChecks=new Map();
  state.inventoryRestoreRoot=null;
  state.collapsedPanels=new Set();
  state.errorViews=new Map();
  state.executorViews=new Map();
  state.tasks=null;
  state.systemInfo=null;
  state.systemBusy=false;
  state.systemReceivedAt=0;
  state.systemSampleHealthy=false;
  state.workersInfo=null;
  state.workersBusy=false;
  state.workersSampleHealthy=false;
  state.workersReceivedAt=0;
  state.workersDirty=false;
  state.workersSaving=false;
  state.tasksDirty=false;
  state.tasksBusy=false;
  state.tasksLoadedAt=0;
  state.taskRequests=new Set();
  state.retryFailedTracks=false;
  state.liveMode='connecting';
  state.statusReceivedAt=0;
  state.receivedAt=0;
  state.systemSampleAt=0;
  let saveController;
  const settingsDirty = () => {state.settingsDirty=true;saveController?.render();updateSettingsActions();};
  const updateSettingsActions = () => {for(const button of $$('.test-integration,.refresh-integration')){button.disabled=state.settingsDirty;button.title=state.settingsDirty?'Save or discard your changes before using this connection':'Use the saved connection settings';}};
  const integrationDefaults = () => ({enabled:false,url:'',api_key:'',api_key_set:false,path_mappings:[]});
  const setText = (el, value) => { if (el) el.textContent = value == null ? '' : String(value); };
  const escStatus = value => String(value || '').toLowerCase().replace(/[^a-z0-9_-]/g, '');
  const dateMillis = value => {if(!value)return null;const date=new Date(typeof value==='number'&&value<1e12?value*1000:value);return Number.isFinite(date.valueOf())?date.valueOf():null;};
  const absoluteDate = value => {const at=dateMillis(value);return at===null?'Time unavailable':new Intl.DateTimeFormat(undefined,{year:'numeric',month:'short',day:'numeric',hour:'numeric',minute:'2-digit',second:'2-digit',timeZoneName:'short'}).format(at);};
  const fmtDate = value => {
    const at=dateMillis(value);if(at===null)return '—';const delta=Math.floor((Date.now()-at)/1000),seconds=Math.abs(delta);
    if(seconds<10)return delta<0?'now':'just now';
    const amount=seconds<60?`${seconds}s`:seconds<3600?`${Math.floor(seconds/60)}m`:seconds<86400?`${Math.floor(seconds/3600)}h`:seconds<604800?`${Math.floor(seconds/86400)}d`:seconds<2592000?`${Math.floor(seconds/604800)}w`:seconds<31536000?`${Math.floor(seconds/2592000)}mo`:`${Math.floor(seconds/31536000)}y`;
    return delta<0?`in ${amount}`:`${amount} ago`;
  };
  const timeNode = value => {const node=document.createElement('time'),at=dateMillis(value);if(at===null){node.textContent='—';return node;}node.dataset.relativeTime=String(at);node.dateTime=new Date(at).toISOString();node.title=absoluteDate(at);node.textContent=fmtDate(at);return node;};
  const timestamp = (node,value,prefix='',suffix='') => {node.replaceChildren(document.createTextNode(prefix),timeNode(value),document.createTextNode(suffix));};
  const refreshTimes = () => {for(const node of $$('time[data-relative-time]'))setText(node,fmtDate(Number(node.dataset.relativeTime)));};
  const plainCopy = value => String(value||'').replace(/\bocr-rs\b/gi,'subtitle recognition').replace(/\badmitted\b/gi,'started').replace(/\badmission\b/gi,'capacity checks').replace(/\bRust process\b/gi,'Subforge');
  const currentLabel = current => {
    if(!current)return '';if(typeof current!=='object')return String(current);
    const phase=({preparing:'Preparing job',discovering:'Discovering files',syncing_bazarr:'Syncing Bazarr inventory',checking_bazarr_coverage:'Checking subtitle coverage',notifying_bazarr:'Notifying Bazarr',waiting_for_worker:'Waiting for a worker',checking_gpu:'Checking GPU runtime',extracting:'Extracting text',ocr:'Recognizing images',validating:'Validating SRT',track_complete:'Track complete',processing:'Processing file',cached:'Already converted',pending:'Awaiting OCR',failed:'Failed'})[current.status]||current.stage||current.status||'Processing';
    const backend=({ffmpeg:'Text conversion / CPU',tesseract:'Image recognition / CPU',paddle:'Image recognition / GPU',ocr_rs_cpu:'Image recognition / CPU',ocr_rs_cuda:'Image recognition / GPU',seconv:'Subtitle conversion'})[current.backend];
    return [phase,backend,current.track!=null?`track ${current.track}`:'',shortPath(current.path||current.output||'')].filter(Boolean).join(' · ');
  };
  const shortPath = value => { const path = String(value || ''); return path.length > 80 ? `…${path.slice(-77)}` : path; };
  const notify = (message, kind = 'info') => { const toast = $('#toast'); setText(toast, message); toast.className = `toast ${kind}`; clearTimeout(state.lastToast); state.lastToast = setTimeout(() => toast.classList.add('hidden'), 4200); };
  const api = async (path, options = {}) => {
    const headers = new Headers(options.headers || {});
    if (options.body && !(options.body instanceof FormData)) headers.set('Content-Type','application/json');
    if (state.csrf && options.method && options.method !== 'GET') headers.set('X-CSRF-Token', state.csrf);
    // A stalled read must release polling's busy flags so the next sample can recover.
    const readOnly=!options.method||options.method==='GET';
    const response = await fetch(path,{...options,headers,credentials:'same-origin',cache:'no-store',signal:options.signal||(readOnly?AbortSignal.timeout(15000):undefined)});
    const contentType = response.headers.get('content-type') || '';
    const payload = contentType.includes('json') ? await response.json() : await response.text();
    if (!response.ok) throw new Error(typeof payload === 'object' ? payload.error || payload.message || `Request failed (${response.status})` : payload || `Request failed (${response.status})`);
    return payload;
  };
  const activeJobs = () => state.status?.active_jobs?.length?state.status.active_jobs:[state.status?.running].filter(Boolean);
  const jobList = () => { const status=state.status||{},seen=new Set();return [...activeJobs(),...(status.queue||[]),...(status.history||[])].filter(job=>{if(!job)return false;const key=job.id||job;if(seen.has(key))return false;seen.add(key);return true;}); };
  const languageAliases={eng:'en',fra:'fr',fre:'fr',ara:'ar',deu:'de',ger:'de',spa:'es',ita:'it',por:'pt',jpn:'ja',zho:'zh',chi:'zh',kor:'ko',rus:'ru',nld:'nl',dut:'nl',tur:'tr',pol:'pl',heb:'he',hin:'hi',ukr:'uk',swe:'sv',dan:'da',fin:'fi',nor:'no',ces:'cs',cze:'cs',ron:'ro',rum:'ro',ell:'el',gre:'el',hun:'hu',tha:'th',vie:'vi',ind:'id'};
  const languageCode = code => {const value=String(code||'und').toLowerCase(),parts=value.split('-');parts[0]=languageAliases[parts[0]]||parts[0];return parts.join('-');};
  const langLabel = code => { const base = languageCode(code).split('-')[0]; return languages.find(item => item[0] === base)?.[1] || String(code || '').toUpperCase(); };
  const isAll = list => !Array.isArray(list) || list.includes('*');
  const languageDescription = list => isAll(list) ? 'All languages' : `${list.length} ${list.length === 1 ? 'language' : 'languages'} · ${list.slice(0,3).map(langLabel).join(', ')}${list.length > 3 ? ` +${list.length - 3}` : ''}`;
  const iconSvg = type => {
    const paths={overview:'M3 3h7v7H3zM14 3h7v7h-7zM3 14h7v7H3zM14 14h7v7h-7z',library:'M4 4h16v16H4zM8 8h8M8 12h8M8 16h5',activity:'M12 3a9 9 0 1 1-9 9 9 9 0 0 1 9-9M12 7v5l3 2',settings:'M12 8a4 4 0 1 1 0 8 4 4 0 0 1 0-8M12 2v3M12 19v3M2 12h3M19 12h3M5 5l2 2M17 17l2 2M5 19l2-2M17 7l2-2',scan:'M4 4h6M4 4v6m16-6h-6m6 0v6M4 20h6m-6 0v-6m16 6h-6m6 0v-6',inspect:'m21 21-5-5M18 10a8 8 0 1 1-16 0 8 8 0 0 1 16 0',convert:'M4 7h15m-4-4 4 4-4 4M20 17H5m4-4-4 4 4 4',ocr:'M3 5h18v14H3zM6 15l4-5 4 5 3-3 3 4M16 8h.01',backup:'M5 3h14v18H5zM8 3v6h8V3M8 21v-7h8v7',health:'M3 12h4l3-8 4 16 3-8h4',cleanup:'M5 6h14M9 6V3h6v3M7 6l1 15h8l1-15',refresh:'M20 8a8 8 0 1 0 0 8M20 3v5h-5',file:'M6 3h8l4 4v14H6zM14 3v5h4'};
    const svg=document.createElementNS('http://www.w3.org/2000/svg','svg');svg.setAttribute('viewBox','0 0 24 24');svg.setAttribute('aria-hidden','true');
    const path=document.createElementNS(svg.namespaceURI,'path');path.setAttribute('d',paths[type]||paths.file);svg.append(path);return svg;
  };
  const usesBazarr = () => state.inventorySource==='bazarr'||state.settings?.integrations?.bazarr?.enabled===true;
  const targetMatches = (wanted, candidate) => {
    const a=languageCode(wanted.language),b=languageCode(candidate.language);
    return (a===b||!a.includes('-')&&a===b.split('-')[0])&&!!wanted.forced===!!candidate.forced&&!!wanted.hi===!!candidate.hi;
  };
  const coverageState = row => {
    const value=row.coverage?.status;
    if(['covered','already_covered'].includes(value))return 'covered';
    if(['needs_conversion','needed'].includes(value))return 'needed';
    if(['no_convertible_source','no_available_conversion'].includes(value))return 'unavailable';
    if(!value&&row.missing_srt===false&&row.wanted_targets?.length)return 'covered';
    return 'unknown';
  };
  const rowMissingTargets = row => row.coverage?.missing_targets||row.missing_targets||[];
  const rowExistingTargets = row => row.coverage?.existing_targets||row.existing_targets||[];
  const targetLabel = target => `${languageCode(target.language).toUpperCase()}${target.forced?' forced':''}${target.hi?' SDH (deaf and hard of hearing)':''}`;
  const renderInventoryAuthority = () => {
    const bazarr=usesBazarr(),authority=state.inventoryAuthority||{},status=bazarr&&state.inventorySource!=='bazarr'?'stale':authority.status||'ready';
    setText($('#inventory-source'),bazarr?'Bazarr':'Filesystem');
    const blocked=bazarr&&!['ready','refresh_pending'].includes(status);
    const banner=$('#inventory-authority');banner.classList.remove('hidden');
    banner.dataset.authorityState=bazarr?status:state.status?'ready':'loading';
    banner.classList.toggle('authority-error',blocked);
    const pendingCount=Array.isArray(authority.pending_srt_keys)?authority.pending_srt_keys.length:0;
    const pendingDetail=authority.verification_limited===true?' Some published outputs exceed the indexing verification limits.':pendingCount?` ${pendingCount.toLocaleString()} published SRT file${pendingCount===1?' is':'s are'} not yet confirmed by Bazarr.`:'';
    const updated=state.status?.inventory?.updated||state.inventoryUpdated;
    const minutes=updated?Math.max(0,Math.floor((Date.now()-(updated<1e12?updated*1000:updated))/60000)):null;
    const age=minutes===null?'Never synced':`Synced ${Math.floor(minutes/60)}h ${minutes%60}m ago`;
    $('#inventory-authority-copy').title=updated?`Last successful inventory sync: ${absoluteDate(updated)}`:'No successful inventory sync recorded';
    setText($('#inventory-authority-copy'),bazarr?({ready:'Bazarr inventory is ready.',stale:'Bazarr inventory is stale. Sync Bazarr before starting automatic conversion.',unavailable:'Bazarr inventory is unavailable. Sync Bazarr to retry; automatic conversion is blocked.',refresh_pending:`Bazarr inventory synced. Published subtitle indexing is still unconfirmed.${pendingDetail} Conversion is available; existing SRT files are checked before converting.`,reindex_failed:'Subtitles were published, but Bazarr could not reindex them. Check the Bazarr connection and sync again.'}[status]||'Bazarr inventory status is not available. Sync before converting.')+(status!=='ready'&&authority.reason?` ${authority.reason}`:'')+(authority.error?` ${authority.error}`:'')+` ${age}. Saved snapshots become stale when Subforge restarts; there is no age cutoff. A failed sync blocks conversion until a successful retry.`:state.status?'Filesystem inventory. Inspect media to update saved tracks and coverage.':'Checking library status…');
    setText($('#inventory-sync-now'),bazarr?'Sync now':'Inspect now');
    const syncing=jobList().some(job=>job.kind==='inspect'&&['queued','running'].includes(job.status));
    $('#inventory-sync-now').disabled=syncing;$('#inventory-sync-now').title=syncing?'An inventory sync is already queued or running':'Refresh the complete inventory now';
    $$('[data-action="inspect"]').forEach(button=>setText(button,bazarr?'Sync Bazarr inventory':'Inspect media'));
    const overviewNotice=$('#overview-inventory-notice');
    if(overviewNotice){overviewNotice.hidden=!bazarr||status==='ready';overviewNotice.replaceChildren();const copy=document.createElement('span');copy.textContent=$('#inventory-authority-copy').textContent;const sync=document.createElement('button');sync.type='button';sync.className='text-button';sync.textContent='Sync now';sync.disabled=syncing;sync.title=$('#inventory-sync-now').title;sync.addEventListener('click',syncInventoryNow);overviewNotice.append(copy,sync);}
    $$('[data-action="scan"],#quick-form button[type="submit"]').forEach(button=>{button.disabled=blocked;button.title=blocked?'Sync Bazarr before starting automatic conversion':'';});
    updateLanguageLabels();
    syncMediaSelection();
  };
  const trackFormat = track => {
    const codec=String(track.codec||track.codec_id||track.format||'').toLowerCase();
    if(/pgs|hdmv|sup|vobsub|dvd|dvb/.test(codec)||track.image===true||track.kind==='image')return {label:/pgs|hdmv|sup/.test(codec)?'PGS (img)':'VobSub (img)',style:'image',image:true};
    if(/ass|ssa|substation/.test(codec))return {label:'ASS',style:'ass',image:false};
    if(/srt|subrip|utf8/.test(codec))return {label:'SRT',style:'srt',image:false};
    return {label:/vtt|webvtt/.test(codec)?'VTT':codec?codec.replace(/^s_text\//,'').toUpperCase().slice(0,16):'Text',style:'text',image:false};
  };
  const trackBadge = track => {const format=trackFormat(track),pill=document.createElement('span');pill.className=`format-badge format-${format.style}`;pill.textContent=`${languageCode(track.language).toUpperCase()} · ${format.label}`;pill.title=`${langLabel(track.language||'und')} · ${track.codec||track.codec_id||track.format||format.label}`;return pill;};
  const filterLibrary = filter => {$('#inventory-filter').value=filter;$('#inventory-root').value='';$('#inventory-search').value='';state.inventoryPage=1;renderInventory();saveInventoryView();navigate('library');};
  const renderPipeline = () => {
    const data=state.status?.pipeline||{},available=data.available||{},active=activeJobs(),running=active[0],queued=(state.status?.queue||[]).length;
    const count=key=>available[key]===false||!Number.isFinite(data[key])?'—':data[key].toLocaleString();
    setText($('#pipeline-inspected'),count('files_inspected'));setText($('#pipeline-ready'),count('generated_srt_count'));
    $('#pipeline-ready').parentElement.title='Distinct published-output receipts in saved conversion state. Existence is not rescanned here; includes older sources outside the current inventory. This is not a current disk count or an average per inspected item. Library coverage checks current SRT targets.';
    const inventoryStatus=$('#pipeline-status');
    const inventoryUpdated=state.status?.inventory?.updated??state.inventoryUpdated;
    if(inventoryUpdated)timestamp(inventoryStatus,inventoryUpdated,`${usesBazarr()?'Bazarr sync':'Inspected'} `,state.status?.inventory?.truncated?' · Partial snapshot':'');else setText(inventoryStatus,'No inventory yet');
    if(inventoryStatus)inventoryStatus.title=usesBazarr()?'Bazarr profiles define the required languages. Open Library for verified SRT coverage.':'Inspect media to update the saved filesystem inventory.';
    setText($('#attention-pending'),count('pending_ocr_files'));setText($('#attention-failed'),count('failed_files'));setText($('#attention-unreachable'),count('unreachable_roots'));
    for(const [key,value] of [['pending',data.pending_ocr_files],['failed',data.failed_files],['paths',data.unreachable_roots]]){const button=$(`[data-attention="${key}"]`);button.classList.toggle('has-issues',Number(value)>0);button.classList.toggle('is-clear',value===0);button.title=key==='paths'?'Review unavailable media folders':key==='failed'?'Review failures saved in conversion state':'Review files awaiting image subtitle recognition';}
    const idle=!running&&!queued,currentPanel=$('#current-work-title').closest('article'),idleStatus=$('#overview-work-status');
    currentPanel.hidden=idle;currentPanel.classList.toggle('is-idle',idle);if(idleStatus)setText(idleStatus,idle?'No jobs running':running?`${active.length} job${active.length===1?'':'s'} running`:`${queued} queued job${queued===1?'':'s'}`);
    const progress=$('#pipeline-progress');progress.hidden=!running;
    if(running&&Number(running.total)>0){progress.value=jobPercent(running);progress.setAttribute('aria-label',`${running.processed||0} of ${running.total} files processed`);}else progress.removeAttribute('value');
    const content=$('#current-work-content');content.replaceChildren();
    setText($('#current-work-summary'),running?[active.length>1?`${active.length} jobs in progress`:running.total?`${running.processed||0} / ${running.total} files`:'Discovering work',queued?`${queued} queued job${queued===1?'':'s'}`:''].filter(Boolean).join(' · '):queued?`${queued} queued job${queued===1?'':'s'} · waiting for a worker`:'No jobs running');
    const title=document.createElement('strong'),detail=document.createElement('span');detail.className='muted';
    title.textContent=running?({scan:'Converting text subtitles',inspect:'Inspecting media',extract:'Converting selected tracks',ocr:'Recognizing image subtitles'})[running.kind]||'Running maintenance':queued?'Work is queued':'';
    detail.textContent=running?[currentLabel(running.current)||'Preparing files',jobTiming(running)].filter(Boolean).join(' · '):queued?'Review the queue in Activity or check worker capacity below.':'';
    content.append(title,detail);
  };
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
    if(header.classList.contains('inventory-toolbar')){header.firstElementChild.classList.add('inventory-heading-title');const actions=$(':scope > .panel-heading-actions',header)||document.createElement('div');actions.className='panel-heading-actions';actions.append(button);if(!actions.parentElement)header.insertBefore(actions,$('.toolbar-filters',header));}
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
    for(const item of Array.isArray(job.notifications)?job.notifications:[])if(item?.ok===false&&!item.pending)add('notification',item);
    for(const item of Array.isArray(job.results)?job.results:[])if(item?.ok===false&&!item.pending)add('result',item);
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
      if(group.provider){const settings=document.createElement('button');settings.type='button';settings.className='text-button';settings.textContent=`Review ${group.label} settings`;settings.dataset.jobFocus=`${key}:${group.source}:${group.provider}:${group.message}`;settings.addEventListener('click',()=>{closeDialog($('#job-drawer-backdrop'));navigate('settings',group.provider);const card=$(`[data-provider="${group.provider}"]`);if(card?.classList.contains('is-collapsed'))$('.collapse-toggle',card).click();});row.append(settings);}
      list.append(row);
    }
    content.append(list);
    if(Array.isArray(job.failed_paths)&&job.failed_paths.length&& !['running','queued'].includes(job.status)){const retry=document.createElement('button');retry.type='button';retry.className='button button-secondary retry-failed';retry.dataset.jobFocus=`${key}:retry`;retry.textContent='Retry failed';retry.title=`Retry ${job.failed_paths.length} recorded files${job.failed_paths_truncated?' (first 64 failures)':''}`;retry.addEventListener('click',async()=>{retry.disabled=true;await startJob({kind:job.kind==='inspect'?'inspect':'extract',paths:job.failed_paths.slice(0,64),languages:job.languages||['*'],retry:true,includeOcr:job.kind==='ocr'||job.include_ocr===true,trackIds:job.failed_paths.length===1?job.track_ids:undefined});retry.disabled=false;});content.append(retry);}
    details.append(content);details.addEventListener('toggle',()=>{if(details.isConnected)state.errorViews.set(key,{open:details.open,scroll:content.scrollTop});});content.addEventListener('scroll',()=>{if(details.isConnected)state.errorViews.set(key,{open:details.open,scroll:content.scrollTop});});return details;
  };
  const renderRootSelectors = roots => {
    const rootList = $('#root-list'); rootList.replaceChildren();
    const quick = $('#quick-root'), inventoryRoot = $('#inventory-root');
    const keepQuick = quick.value, keepInventory = state.inventoryRestoreRoot ?? inventoryRoot.value;
    quick.replaceChildren(new Option('All scan roots','')); inventoryRoot.replaceChildren(new Option('All roots',''));
    const groups=new Map();
    for (const root of roots || []) {
      const label = root.label || root.path || root.id;
      const key=root.volume_id||'unassigned';let group=groups.get(key);
      if(!group){const section=document.createElement('section');section.className='root-volume-group';const heading=document.createElement('div');heading.className='root-volume-heading';const volume=document.createElement('strong');volume.textContent=root.volume_label==='Storage volume'?`Storage volume ${groups.size+1}`:root.volume_label||'Folders';heading.append(volume);section.append(heading);rootList.append(section);group=section;groups.set(key,group);}
      if(root.volume_id&&Number.isFinite(root.free_bytes)&&!$('.root-volume-space',group)){const free=document.createElement('span');free.className='root-volume-space';free.textContent=`${formatSpace(root.free_bytes)} free`;$('.root-volume-heading',group).append(free);}
      const item = document.createElement('div'); item.className = 'root-item';
      const text = document.createElement('span'); text.className = 'root-item-text'; text.title = root.path || label; text.textContent = label;
      item.append(text); group.append(item);
      quick.add(new Option(label,root.id)); inventoryRoot.add(new Option(label,root.id));
    }
    if ([...quick.options].some(x=>x.value===keepQuick)) quick.value=keepQuick;
    if ([...inventoryRoot.options].some(x=>x.value===keepInventory)) inventoryRoot.value=keepInventory;
    state.inventoryRestoreRoot=null;
    setText($('#stat-roots'),(roots || []).length);
  };
  const updateLanguageLabels = () => {
    const describe=list=>usesBazarr()&&isAll(list)?'Bazarr profile languages':isAll(list)?'All tagged languages':languageDescription(list);
    setText($('#quick-languages'),describe(state.selectedLanguages));
    setText($('#settings-languages'),describe(state.defaultLanguages));
    const policy=usesBazarr()?'Bazarr profiles set required languages and normal, forced, and SDH variants. Your language selection filters those targets; it cannot add languages outside a profile.':'All tagged languages exports detected languages; untagged tracks require explicit selection.';
    setText($('#language-policy-hint'),`${policy} Preferences supply the default for scheduled scans and Quick conversion. A Quick selection overrides that default for manual jobs in this browser session.`);
    setText($('#quick-language-policy'),`${state.quickLanguagesChanged?'Using your Quick selection.':'Using saved Preferences.'} ${policy} Existing valid SRT targets are skipped.`);
    setText($('#stat-languages'),isAll(state.selectedLanguages)?'All':state.selectedLanguages.length);
  };
  const jobPercent = job => {
    if(Number.isFinite(job.telemetry?.progress_percent))return Math.max(0,Math.min(100,job.telemetry.progress_percent));
    const total=Number(job.total || 0), processed=Number(job.processed || 0);
    if (total>0) return Math.max(0,Math.min(100,Math.round(100*processed/total)));
    const summary = job.summary || {};
    const counts = ['converted','cached','pending','failed','planned'].reduce((sum,key)=>sum + Number(summary[key] || 0),0);
    if (Number.isFinite(Number(job.progress))) return Math.max(0,Math.min(100,Number(job.progress)));
    const done = Number(summary.converted || 0) + Number(summary.cached || 0) + Number(summary.failed || 0);
    return counts ? Math.round(100 * done / counts) : job.status === 'completed' ? 100 : 0;
  };
  const duration = value => {
    const seconds=Math.max(0,Math.floor(Number(value)||0)),hours=Math.floor(seconds/3600),minutes=Math.floor(seconds%3600/60);
    return hours?`${hours}h ${minutes}m`:minutes?`${minutes}m ${seconds%60}s`:`${seconds}s`;
  };
  const jobTiming = job => {
    const telemetry=job.telemetry;
    if(!telemetry)return '';
    const parts=Number.isFinite(telemetry.elapsed_seconds)?[`${job.status==='queued'?'Queued':'Elapsed'} ${duration(telemetry.elapsed_seconds)}`]:[];
    if(job.status==='running')parts.push(Number.isFinite(telemetry.eta_seconds)?`~${duration(telemetry.eta_seconds)} remaining`:['waiting','stalled','unresponsive'].includes(telemetry.liveness)?'ETA unavailable':'ETA estimating');
    if(telemetry.liveness==='waiting')parts.push(telemetry.wait_reason||'Waiting for worker capacity');
    if(telemetry.liveness==='stalled')parts.push(`No progress for ${duration(telemetry.idle_seconds)}; may be stalled`);
    if(telemetry.liveness==='unresponsive')parts.push(`No heartbeat for ${duration(telemetry.heartbeat_age_seconds)}; coordinator may be unresponsive`);
    return parts.join(' · ');
  };
  const jobProgressLabel = job => {
    const known=Number(job.total)>0||Number.isFinite(job.telemetry?.progress_percent);
    const phase=currentLabel(job.current)||(job.status==='queued'?'Waiting for a worker':'Preparing job');
    return `${known?`${jobPercent(job)}% · `:''}${phase}`;
  };
  const jobProgressBar = job => {
    const bar=document.createElement('progress');bar.className='job-meter';bar.max=100;
    if(Number(job.total)>0||Number.isFinite(job.telemetry?.progress_percent))bar.value=jobPercent(job);
    bar.setAttribute('aria-label',Number(job.total)>0?`${job.processed||0} of ${job.total} files processed`:'Preparing job; progress not yet available');
    return bar;
  };
  const jobTitle = job => job.kind==='refresh'?`${({sonarr:'Sonarr',radarr:'Radarr',jellyfin:'Jellyfin',bazarr:'Bazarr'})[job.provider]||'Integration'} ${job.provider==='bazarr'?'inventory check':'library refresh'}`:job.task_id==='inventory'?'Media inventory':({scan:'Library discovery',inspect:'Media inventory',extract:'Convert selected subtitles',ocr:'Subtitle OCR',health:'Health check',cleanup:'Cache cleanup',backup:'State backup'})[job.kind]||'Converter job';
  const mediaJobLabel = path => {
    const name=String(path||'').split(/[\\/]/).pop()?.replace(/\.[^.]+$/,'')||'';
    const episode=name.match(/^(.*?)[ ._-]+(S\d{1,3}E\d{1,3}(?:E\d{1,3})?)/i);
    return episode?`${episode[1].replace(/[._]/g,' ').trim()} · ${episode[2].toUpperCase()}`:name.replace(/[._]/g,' ');
  };
  const maintenanceResult = job => {
    const result=job.maintenance||job.result||{};
    if(typeof result.message==='string')return result.message;
    if(job.kind==='backup'&&Number.isFinite(result.count_bytes))return `Backup created · ${formatSpace(result.count_bytes)}`;
    if(job.kind==='cleanup'&&Number.isFinite(result.removed))return `${result.removed} stale cache entries removed`;
    if(job.kind==='health'&&Array.isArray(result.checks))return `${result.checks.length} checks completed`;
    return '';
  };
  const jobStatus = job => {
    if(job.status==='running'){
      const health=({waiting:['queued','Waiting'],stalled:['queued','Possibly stalled'],unresponsive:['failed','Unresponsive']})[job.telemetry?.liveness];
      if(health)return {style:health[0],label:health[1]};
    }
    const pending=job.status==='completed'&&job.notifications?.some(item=>item?.pending===true);
    return {style:pending?'queued':escStatus(job.status),label:pending?'Converted · indexing pending':({completed_with_errors:'Completed with errors',running:'Running',queued:'Queued',completed:'Completed',failed:'Failed',cancelled:'Cancelled'})[job.status]||job.status||'Queued'};
  };
  const fittedPageSize = (page,requested,measure=true) => {
    if(requested!=='auto')return requested;
    // Library rows have natural, variable heights; keep pagination useful and scroll the pane.
    if(page==='library'){setText($('#inventory-page-size option[value="auto"]'),'Fit (25)');return 25;}
    const pane=$(page==='library'?'.inventory-table-wrap':'.jobs-table-wrap');
    if(pane?.clientHeight>0){
      const table=$('table',pane),header=table?.tHead?.getBoundingClientRect().height||0;
      const rows=measure?$$('tbody > tr',pane).filter(row=>!$('.table-empty',row)):[];
      const measured=rows.length?Math.max(...rows.map(row=>row.getBoundingClientRect().height)):0;
      const context=page==='library'
        ?JSON.stringify([state.inventoryUpdated,state.inventoryRevision,$('#inventory-filter').value,$('#inventory-root').value,$('#inventory-search').value])
        :JSON.stringify([$('#activity-status').value,$('#activity-kind').value]);
      let fit=state.tableFitMeasurements[page];
      if(!fit||fit.width!==pane.clientWidth||fit.context!==context){
        fit={width:pane.clientWidth,context,rowHeight:0};
        state.tableFitMeasurements[page]=fit;
      }
      // Keep tall rows in the estimate after pagination removes them, avoiding a resize/render loop.
      fit.rowHeight=Math.max(fit.rowHeight,measured);
      const fallback=page==='library'?(innerWidth<=800?176:72):(innerWidth<=800?144:64);
      const height=Math.max(fit.rowHeight,parseFloat(getComputedStyle(pane).getPropertyValue('--table-row-height'))||fallback);
      state.tableAutoSizes[page]=Math.max(1,Math.min(100,Math.floor((pane.clientHeight-header-2)/height)));
    }
    const size=state.tableAutoSizes[page];
    const option=$(`${page==='library'?'#inventory-page-size':'#jobs-page-size'} option[value="auto"]`);
    if(option)option.textContent=`Fit (${size})`;
    return size;
  };
  const scheduleTableFit = () => {
    if(state.tableFrame)return;
    state.tableFrame=requestAnimationFrame(()=>{
      state.tableFrame=0;
      if(state.currentPage==='library'&&state.inventoryPageSize==='auto'&&fittedPageSize('library','auto')!==state.inventoryEffectiveSize)renderInventory();
      if(state.currentPage==='activity'&&state.jobsPageSize==='auto'&&fittedPageSize('activity','auto')!==state.jobsEffectiveSize)renderJobs();
    });
  };
  const renderJobExecutors = (job,compact) => {
    const executors=Array.isArray(job.executors)?job.executors.slice(0,64).filter(executor=>executor&&['cpu','cpu_ocr','gpu'].includes(executor.resource)&&['local','remote'].includes(executor.execution)&&typeof executor.node_name==='string'&&executor.node_name.trim()):[];
    if(!executors.length&&job.executors_truncated!==true)return null;
    const list=document.createElement('ul');list.className='job-executors';list.setAttribute('aria-label','Job executors');
    const label=executor=>{
      const resource=({cpu:'CPU',cpu_ocr:'CPU OCR',gpu:'GPU OCR'})[executor.resource];
      const node=executor.node_name.trim();
      return `${resource} · ${node}${executor.execution==='remote'?' (remote)':''}`;
    };
    const addItem=(parent,text,className='')=>{const item=document.createElement('li');if(className)item.className=className;item.textContent=text;parent.append(item);return item;};
    executors.slice(0,4).forEach(executor=>addItem(list,label(executor)));
    if(executors.length>4){
      const item=document.createElement('li');item.className='job-executors-more';
      const details=document.createElement('details'),summary=document.createElement('summary');summary.textContent=`${executors.length-4} more workers`;
      const key=errorViewKey(job,compact);details.dataset.executorKey=key;details.open=state.executorViews.get(key)===true;summary.dataset.jobFocus=`${key}:executors`;
      details.addEventListener('toggle',()=>{if(details.isConnected)state.executorViews.set(key,details.open);});
      const remaining=document.createElement('ul');executors.slice(4).forEach(executor=>addItem(remaining,label(executor)));
      details.append(summary,remaining);item.append(details);list.append(item);
    }
    if(job.executors_truncated===true)addItem(list,'Additional workers not shown','job-executors-truncated');
    return list;
  };
  const renderJobCard = (job, compact = false) => {
    const card = document.createElement('article'); card.className = `job-card${compact ? ' job-card-compact' : ''}`;
    const icon = document.createElement('div'),type=job.kind==='extract'?'convert':job.kind; icon.className = `job-icon job-${escStatus(type)}`; icon.append(iconSvg(type));
    const detail = document.createElement('div'); detail.className = 'job-detail';
    const title = document.createElement('strong'); title.textContent = jobTitle(job);
    const sub = document.createElement('span'); sub.className = 'job-subtitle'; const mediaPath=job.current?.path||job.paths?.[0];sub.textContent=mediaPath?mediaJobLabel(mediaPath):job.root_label||'All media folders';sub.title=mediaPath||'';
    detail.append(title,sub);
    const status = document.createElement('div'); status.className = 'job-status-block';
    const statusValue=jobStatus(job),pill = document.createElement('span'); pill.className=`status-pill ${statusValue.style}`; pill.textContent=statusValue.label;
    const time=timeNode(job.finished || job.telemetry?.heartbeat_at || job.started || job.created); time.className='job-time';
    status.append(pill,time);
    card.append(icon,detail,status);
    if(job.result_text||maintenanceResult(job)){const result=document.createElement('p');result.className='job-result';result.textContent=job.result_text||maintenanceResult(job);card.append(result);}
    const executors=renderJobExecutors(job,compact);if(executors)card.append(executors);
    const timing=jobTiming(job);if(timing){const info=document.createElement('p');info.className='job-timing';info.textContent=timing;card.append(info);}
    if (job.status === 'running' || job.status === 'queued') {
      const phase=document.createElement('p');phase.className='job-timing';phase.textContent=jobProgressLabel(job);card.append(phase,jobProgressBar(job));
      if (job.id) { const cancel=document.createElement('button'); cancel.className='text-button cancel-job'; cancel.textContent='Cancel'; cancel.dataset.jobId=job.id; cancel.dataset.jobFocus=`${errorViewKey(job,compact)}:cancel`; status.append(cancel); }
    } else if (job.summary) {
      const chips=document.createElement('div'); chips.className='job-summary';
      for (const [key,label] of [['converted','converted'],['cached','already ready'],['pending','pending'],['planned','planned'],['inspected','inspected'],['failed','failed']]) if (Number(job.summary[key] || 0)) { const chip=document.createElement('span'); chip.textContent=`${job.summary[key]} ${label}`; chips.append(chip); }
      if (chips.childNodes.length) card.append(chips);
    }
    const successfulResults=[...(job.kind==='refresh'&&Array.isArray(job.results)?job.results:[]),...(Array.isArray(job.notifications)?job.notifications.filter(item=>item?.provider==='bazarr'):[])].filter(item=>item?.ok===true&&typeof item.message==='string');
    for(const item of successfulResults){const result=document.createElement('p');result.className='job-result';result.textContent=item.message;card.append(result);}
    for(const item of Array.isArray(job.notifications)?job.notifications.filter(item=>item?.pending===true):[]) {
      const notice=document.createElement('div');notice.className='job-result integration-result result-pending';
      const message=document.createElement('span');message.textContent=item.message||'Bazarr indexing has not been confirmed yet.';
      const check=document.createElement('button');check.type='button';check.className='text-button';check.textContent='Check Bazarr indexing';check.dataset.jobFocus=`${errorViewKey(job,compact)}:indexing`;
      check.addEventListener('click',()=>startJob({kind:'inspect',rootIds:[],languages:['*']}));notice.append(message,check);card.append(notice);
    }
    const errors=renderErrors(job,compact);if(errors)card.append(errors);
    return card;
  };
  const updateJobList = (container,jobs,compact) => {
    const signature=JSON.stringify(jobs);if(container.jobSignature===signature)return;
    const focusKey=container.contains(document.activeElement)?document.activeElement.dataset.jobFocus:null;
    for(const details of $$('.job-error',container))state.errorViews.set(details.dataset.errorKey,{open:details.open,scroll:$('.job-error-content',details).scrollTop});
    for(const details of $$('[data-executor-key]',container))state.executorViews.set(details.dataset.executorKey,details.open);
    container.jobSignature=signature;container.replaceChildren();for(const job of jobs)container.append(renderJobCard(job,compact));
    if(!jobs.length){const empty=document.createElement('div');empty.className=`empty-state${compact?' compact':''}`;const icon=document.createElement('span');icon.className='empty-icon';icon.textContent='◷';const title=document.createElement('strong');title.textContent=compact?'No activity yet':'No jobs yet';const hint=document.createElement('span');hint.textContent=compact?'Start a scan to see progress here.':'Start a scan or conversion to see it here.';empty.append(icon,title,hint);container.append(empty);}
    if(focusKey)$$('[data-job-focus]',container).find(node=>node.dataset.jobFocus===focusKey)?.focus({preventScroll:true});
    for(const details of $$('.job-error',container))$('.job-error-content',details).scrollTop=state.errorViews.get(details.dataset.errorKey)?.scroll || 0;
  };
  const openJobDetails = job => {
    state.selectedJobId=job.id;
    state.selectedJobSnapshot=job;
    state.modalTrigger=document.activeElement;
    setText($('#job-drawer-title'),jobTitle(job));
    updateJobList($('#job-drawer-content'),[job],false);
    showDialog($('#job-drawer-backdrop'));
    $('#close-job-drawer').focus();
  };
  const renderJobRows = jobs => {
    const body=$('#jobs-list'),signature=JSON.stringify(jobs);
    if(body.jobSignature===signature)return;
    const focus=body.contains(document.activeElement)?document.activeElement.dataset.jobFocus:null;
    body.jobSignature=signature;body.replaceChildren();
    if(!jobs.length){const row=document.createElement('tr'),cell=document.createElement('td');cell.colSpan=8;cell.className='table-empty';cell.textContent='No jobs match these filters.';row.append(cell);body.append(row);return;}
    for(const job of jobs){
      const row=document.createElement('tr');row.className='job-row';row.dataset.jobId=job.id;
      const cell=(name,className)=>{const value=document.createElement('td');value.dataset.label=name;value.className=className;row.append(value);return value;};
      const name=cell('Job','job-name'),title=document.createElement('strong'),scope=document.createElement('small');title.textContent=jobTitle(job);scope.textContent=job.paths?.length?mediaJobLabel(job.paths[0])+(job.paths.length>1?` +${job.paths.length-1} files`:''):job.root_label||'All media folders';scope.title=job.paths?.[0]||job.root_label||'';name.append(title,scope);
      const status=cell('Status','job-row-status'),value=jobStatus(job),pill=document.createElement('span');pill.className=`status-pill ${value.style}`;pill.textContent=value.label;status.append(pill);
      const workers=cell('Worker','job-row-workers'),executors=(Array.isArray(job.executors)?job.executors.slice(0,64):[]).filter(item=>item&&['cpu','cpu_ocr','gpu'].includes(item.resource)&&['local','remote'].includes(item.execution)&&typeof item.node_name==='string'&&item.node_name.trim());
      const workerLabels=executors.map(item=>`${({cpu:'CPU',cpu_ocr:'CPU OCR',gpu:'GPU OCR'})[item.resource]} · ${item.node_name}`);workers.textContent=workerLabels.slice(0,2).join(', ')+(workerLabels.length>2?` +${workerLabels.length-2}`:'')||(['backup','health','cleanup'].includes(job.kind)?'Local maintenance':job.status==='queued'?'Awaiting assignment':'Not recorded');workers.title=workerLabels.join('\n');
      const result=cell('Result','job-row-result'),summary=job.summary||{},parts=[];
      for(const [key,label] of [['converted','converted'],['cached','ready'],['inspected','inspected'],['failed','failed']])if(Number(summary[key]))parts.push(`${Number(summary[key]).toLocaleString()} ${label}`);
      const live=['running','queued'].includes(job.status),copy=document.createElement('span');copy.className='job-result-copy';copy.textContent=live?jobProgressLabel(job):job.result_text||maintenanceResult(job)||parts.join(', ')||((job.notifications||[]).some(item=>item?.pending===true)?'Indexing pending':job.error?'Review error details':job.kind==='refresh'?(job.results||[]).map(item=>item.message).filter(Boolean).join('; ')||'Refresh completed':job.status==='cancelled'?'Cancelled before completion':job.status==='failed'?'Review error details':'No subtitle changes');result.append(copy);
      if(live)result.append(jobProgressBar(job));
      const timing=jobTiming(job);if(timing){const info=document.createElement('small');info.className='job-timing';info.textContent=timing;result.append(info);}result.title=[copy.textContent,timing].filter(Boolean).join('\n');
      const started=cell('Started','job-row-started');if(job.started)started.append(timeNode(job.started));else started.textContent=job.status==='queued'?'Waiting':'Not recorded';
      const wait=cell('Queue wait','job-row-wait'),waitSeconds=job.telemetry?.queue_wait_seconds??(job.started&&job.created?Math.max(0,job.started-job.created):null);wait.textContent=Number.isFinite(waitSeconds)?duration(waitSeconds):job.status==='queued'?duration(Date.now()/1000-Number(job.created||Date.now()/1000)):'Not recorded';
      const updated=cell('Updated','job-row-time');updated.append(timeNode(job.finished||job.telemetry?.heartbeat_at||job.started||job.created));
      const actionsCell=cell('Actions','job-row-actions'),actions=document.createElement('div');actions.className='job-actions';actionsCell.append(actions);const details=document.createElement('button');details.className='row-action';details.type='button';details.textContent='Details';details.dataset.viewJob=job.id;details.dataset.jobFocus=`${job.id}:details`;details.setAttribute('aria-label',`Details for ${jobTitle(job)}`);details.addEventListener('click',()=>openJobDetails(job));actions.append(details);
      if(['running','queued'].includes(job.status)&&job.id){const cancel=document.createElement('button');cancel.type='button';cancel.className='text-button cancel-job';cancel.textContent='Cancel';cancel.dataset.jobId=job.id;cancel.dataset.jobFocus=`${job.id}:cancel`;actions.append(cancel);}
      body.append(row);
    }
    if(focus)$$('[data-job-focus]',body).find(node=>node.dataset.jobFocus===focus)?.focus({preventScroll:true});
    scheduleTableFit();
  };
  const saveJobsView = () => {try{sessionStorage.setItem('subforge.jobs-view',JSON.stringify({page:state.jobsPage,pageSize:state.jobsPageSize,status:$('#activity-status').value,kind:$('#activity-kind').value}));}catch{}};
  const restoreJobsView = () => {try{const view=JSON.parse(sessionStorage.getItem('subforge.jobs-view')||'{}');state.jobsPage=Number.isInteger(view.page)&&view.page>0?Math.min(view.page,200):1;state.jobsPageSize=[10,25,50].includes(view.pageSize)?view.pageSize:'auto';$('#jobs-page-size').value=String(state.jobsPageSize);if(['all','running','queued','issues','completed','cancelled'].includes(view.status))$('#activity-status').value=view.status;if(['subtitle','all','maintenance'].includes(view.kind))$('#activity-kind').value=view.kind;}catch{}};
  const renderJobs = () => {
    const jobs=jobList(),running=state.status?.running,maintenance=job=>['health','backup','cleanup'].includes(job.kind);
    const status=$('#activity-status').value,kind=$('#activity-kind').value;
    const filtered=jobs.filter(job=>(kind==='all'||(kind==='maintenance')===maintenance(job))&&(status==='all'||status==='issues'&&['failed','completed_with_errors'].includes(job.status)||status===job.status));
    const size=fittedPageSize('activity',state.jobsPageSize,false),pages=Math.max(1,Math.ceil(filtered.length/size));state.jobsEffectiveSize=size;state.jobsPage=Math.max(1,Math.min(state.jobsPage,pages));const start=(state.jobsPage-1)*size,visible=filtered.slice(start,start+size);
    renderJobRows(visible);
    setText($('#jobs-range'),filtered.length?`${start+1}–${start+visible.length} of ${filtered.length} jobs`:'0 jobs');setText($('#jobs-page-label'),`Page ${state.jobsPage} of ${pages}`);$('#jobs-previous').disabled=state.jobsPage===1;$('#jobs-next').disabled=state.jobsPage===pages;
    if(!$('#job-drawer-backdrop').classList.contains('hidden')){const selected=jobs.find(job=>job.id===state.selectedJobId)||state.selectedJobSnapshot;if(selected){setText($('#job-drawer-title'),jobTitle(selected));updateJobList($('#job-drawer-content'),[selected],false);}}
    setText($('#activity-count'),`${filtered.length} of ${jobs.length} jobs`);
    const validKeys=new Set(jobs.flatMap(job=>[errorViewKey(job,true),errorViewKey(job,false)]));for(const views of [state.errorViews,state.executorViews])for(const key of views.keys())if(!validKeys.has(key))views.delete(key);
    const queued=(state.status?.queue||[]).length,badge=$('#activity-badge');badge.textContent=queued?String(queued):'';badge.classList.toggle('hidden',!queued);
    setText($('#stat-running'),running?'Running':'Idle');
  };
  const renderLogs = logs => {
    const box=$('#live-logs'),signature=JSON.stringify(logs||[]);if(box.logSignature===signature)return;box.logSignature=signature;const atEnd=box.scrollTop+box.clientHeight>=box.scrollHeight-20;box.replaceChildren();
    const entries=(logs || []).slice(-50);
    if (!entries.length) { const empty=document.createElement('div'); empty.className='log-empty'; empty.textContent='No recent converter events'; box.append(empty); return; }
    for (const entry of entries) {
      const row=document.createElement('div'); row.className='log-row';
      const time=timeNode(entry.time);
      const level=document.createElement('span'); level.className=`log-level ${escStatus(entry.level)}`; level.textContent=(entry.level || 'INFO').toUpperCase();
      const message=document.createElement('span'); message.className='log-message'; message.textContent=plainCopy(entry.message);
      row.append(time,level,message); box.append(row);
    }
    if(atEnd)box.scrollTop=box.scrollHeight;
  };
  const renderLiveState = () => {
    const mode=state.liveMode,at=state.receivedAt,label=({connected:'Live',connecting:'Connecting',reconnecting:'Reconnecting',paused:'Paused',unsupported:'Manual refresh',invalid:'Stale',offline:'Offline'})[mode]||'Offline';
    const text=at?`${label} · ${fmtDate(at)}`:label,title=at?`Last dashboard update: ${absoluteDate(at)}`:'Waiting for the first dashboard update';
    for(const node of [$('#last-updated'),$('#connection-text'),...$$('[data-live-state]')]){setText(node,text);node.title=title;node.dataset.liveMode=mode;}
    $('#connection-dot').classList.toggle('offline',mode!=='connected');
    const resources=$('#system-sample-state');
    if(resources){const stale=mode==='connected'&&!state.systemSampleHealthy;setText(resources,stale?(state.systemSampleAt?`Stale · ${fmtDate(state.systemSampleAt)}`:'Waiting for measurements'):text);resources.title=state.systemSampleAt?`Last resource measurement: ${absoluteDate(state.systemSampleAt)}`:'Waiting for resource measurements';}
  };
  const setConnection = good => {state.liveMode=good?'connected':'offline';renderLiveState();};
  const loadInventory = async summary => {
    if(summary?.authority)state.inventoryAuthority=summary.authority;if(summary?.source)state.inventorySource=summary.source;renderInventoryAuthority();
    if(state.currentPage!=='library'||state.inventoryBusy)return;
    if(state.inventoryLoaded&&state.inventoryUpdated===summary?.updated&&state.inventoryRevision===summary?.state_revision)return;
    state.inventoryBusy=true;
    try {const data=await api('/api/library');state.inventory=Array.isArray(data.items)?data.items:[];state.inventoryUpdated=data.updated;state.inventoryRevision=data.state_revision;state.inventoryTruncated=!!data.truncated;state.inventoryLoaded=true;state.inventorySource=data.source||'filesystem';state.inventoryAuthority=data.authority||{status:'ready'};renderInventoryAuthority();renderInventory();renderPipeline();}
    catch(error){notify(`Could not load library inventory: ${error.message}`,'error');}
    finally{state.inventoryBusy=false;}
  };
  const saveInventoryView = () => {
    try {sessionStorage.setItem('subforge.inventory-view',JSON.stringify({page:state.inventoryPage,pageSize:state.inventoryPageSize,search:$('#inventory-search').value,root:$('#inventory-root').value,filter:$('#inventory-filter').value}));}catch{}
  };
  const restoreInventoryView = () => {
    try {const view=JSON.parse(sessionStorage.getItem('subforge.inventory-view')||'{}');state.inventoryPage=Number.isInteger(view.page)&&view.page>0?Math.min(2000,view.page):1;state.inventoryPageSize=[25,50,100].includes(view.pageSize)?view.pageSize:'auto';$('#inventory-page-size').value=String(state.inventoryPageSize);$('#inventory-search').value=typeof view.search==='string'?view.search.slice(0,256):'';$('#inventory-filter').value=['all','needed','covered','unavailable','video','sidecar','issues','has_tracks','pending','failed','ready','metadata'].includes(view.filter)?view.filter:'all';state.inventoryRestoreRoot=typeof view.root==='string'?view.root.slice(0,64):'';}catch{}
  };
  const applyStatus = data => {
    clearTimeout(state.bootstrapFallback);
    state.status=data;state.statusReceivedAt=Date.now();state.receivedAt=state.statusReceivedAt;renderLiveState();renderRootSelectors(data.roots||[]);renderJobs();renderLogs(data.logs);renderPipeline();
    void loadInventory(data.inventory);updateHookStates();updateIntegrationUsage();updateIntegrationDots();updateVersion(data.version,data.edition);
    if(data.tasks&&Array.isArray(data.tasks.tasks)){state.tasks=data.tasks;state.tasksLoadedAt=Date.now();renderTasks();}
  };
  // One-shot snapshots support initialization, explicit refresh, and completed user actions.
  const pollStatus = async () => {
    if (state.pollBusy) return; state.pollBusy=true;
    try { if(!state.csrf){const session=await api('/api/session');state.csrf=session.csrf_token||'';if(!state.csrf)throw new Error('Session is unavailable');} applyStatus(await api('/api/status')); }
    catch (error) { setConnection(false); if (!state.status) notify(`Could not load converter status: ${error.message}`,'error'); }
    finally { state.pollBusy=false; }
  };
  const startJob = async ({kind,rootIds=[],paths=[],languages: selected=state.selectedLanguages,retry=false,trackIds,includeOcr,force=false}) => {
    const payload={kind,root_ids:rootIds,paths,languages:selected,retry};if(trackIds!==undefined)payload.track_ids=trackIds;if(includeOcr!==undefined)payload.include_ocr=includeOcr;if(force)payload.force=true;
    try { const result=await api('/api/jobs',{method:'POST',body:JSON.stringify(payload)}); notify(result.message || 'Job queued.'); await pollStatus(); return result; }
    catch (error) { notify(error.message,'error'); return null; }
  };
  const selectedRootIds = selector => { const value=$(selector).value; return value ? [value] : []; };
  const mediaSelectable = row => !!row.path&&!row.error&&!['covered','unavailable'].includes(coverageState(row));
  const syncMediaSelection = () => {
    for(const path of state.selectedMedia){if(!state.inventory.some(row=>row.path===path&&mediaSelectable(row)))state.selectedMedia.delete(path);}
    const count=state.selectedMedia.size,boxes=$$('.media-select:not(:disabled)');
    for(const box of boxes)box.checked=state.selectedMedia.has(box.value);
    const all=$('#inventory-select-all');all.disabled=!boxes.length;all.title=boxes.length?'Select convertible media on this page, up to 64 total':'No convertible media on this page';all.checked=!!boxes.length&&boxes.every(box=>box.checked);all.indeterminate=boxes.some(box=>box.checked)&&!all.checked;
    $('#inventory-bulk-bar').hidden=!count;setText($('#inventory-selection-count'),`${count} media selected · maximum 64`);
    const blocked=usesBazarr()&&(state.inventorySource!=='bazarr'||!['ready','refresh_pending'].includes(state.inventoryAuthority?.status));
    $('#inventory-bulk-convert').disabled=!count||blocked;$('#inventory-bulk-convert').title=blocked?'Sync Bazarr before converting selected media':'Review conversion for the selected files';
  };
  const syncInventoryNow = async () => {
    const button=$('#inventory-sync-now');button.disabled=true;
    try{await startJob({kind:'inspect'});}finally{renderInventoryAuthority();}
  };
  const renderInventory = () => {
    const body=$('#inventory-body');
    const filter=$('#inventory-filter').value, root=$('#inventory-root').value, query=$('#inventory-search').value.trim().toLowerCase();
    let rows=state.inventory.filter(row=>!root || row.root_id===root);
    if(['needed','covered','unavailable'].includes(filter))rows=rows.filter(row=>coverageState(row)===filter);
    if (filter==='video'||filter==='sidecar') rows=rows.filter(row=>row.type===filter);
    if (filter==='issues') rows=rows.filter(row=>coverageState(row)!=='covered'&&(row.error || row.processing?.failed || row.processing?.pending_ocr || row.type==='video' && !(row.tracks || []).length));
    if(filter==='has_tracks')rows=rows.filter(row=>row.tracks?.length);
    if(filter==='pending')rows=rows.filter(row=>coverageState(row)!=='covered'&&row.processing?.pending_ocr);
    if(filter==='failed')rows=rows.filter(row=>coverageState(row)!=='covered'&&(row.processing?.failed||row.error));
    if(filter==='ready')rows=rows.filter(row=>coverageState(row)==='covered'||row.processing?.generated_srt_count>0);
    if(filter==='metadata')rows=rows.filter(row=>row.metadata_pending);
    if (query) rows=rows.filter(row=>`${row.name || ''}\n${row.path || ''}`.toLowerCase().includes(query));
    const pageSize=fittedPageSize('library',state.inventoryPageSize,false);state.inventoryEffectiveSize=pageSize;
    const count=rows.length, pages=Math.max(1,Math.ceil(count/pageSize));
    if(state.inventoryLoaded)state.inventoryPage=Math.max(1,Math.min(state.inventoryPage,pages));
    const start=(state.inventoryPage-1)*pageSize, visibleRows=rows.slice(start,start+pageSize);
    setText($('#inventory-meta'),`${count.toLocaleString()} ${count===1?'match':'matches'} in ${state.inventory.length.toLocaleString()} ${usesBazarr()?'Bazarr media items':'inspected items'}`);
    const globalCount=filter==='failed'?state.status?.pipeline?.failed_files:filter==='pending'?state.status?.pipeline?.pending_ocr_files:null;
    setText($('#inventory-scope-note'),[state.inventoryTruncated?'The server reports a bounded partial inventory.':'',Number.isFinite(globalCount)?`${count.toLocaleString()} ${count===1?'match':'matches'} in this snapshot; ${globalCount.toLocaleString()} ${filter==='failed'?'failed':'awaiting OCR'} files across saved conversion state.`:'',state.inventoryUpdated?`${usesBazarr()?'Last Bazarr sync':'Last inspection'}: ${fmtDate(state.inventoryUpdated)}.`:''].filter(Boolean).join(' ')||(state.inventoryLoaded?'No saved inventory. Inspect media to load tracks.':'Loading saved inventory…'));
    $('#inventory-scope-note').title=state.inventoryUpdated?absoluteDate(state.inventoryUpdated):'';
    const range=count?`${(start+1).toLocaleString()}–${(start+visibleRows.length).toLocaleString()} of ${count.toLocaleString()} items`:'0 items';
    if ($('#inventory-range').textContent!==range) setText($('#inventory-range'),range);
    const pageSelect=$('#inventory-page');
    if (pageSelect.options.length!==pages) { pageSelect.replaceChildren();for(let page=1;page<=pages;page++)pageSelect.add(new Option(`Page ${page} of ${pages}`,String(page))); }
    pageSelect.value=String(state.inventoryPage);pageSelect.disabled=!count;
    $('#inventory-prev').disabled=state.inventoryPage===1;$('#inventory-next').disabled=state.inventoryPage===pages;
    $('#inventory-limit-note').classList.toggle('hidden',!state.inventoryTruncated);setText($('#inventory-limit-note'),usesBazarr()?'Bazarr returned a bounded partial inventory. Resolve the reported limit before a complete conversion pass.':'This inspection shows the first 2,000 files. Inspect a smaller media folder to see other files.');
    const refreshing=state.status?.running?.kind==='inspect'||(state.status?.queue||[]).some(job=>job.kind==='inspect');
    setText($('#inventory-snapshot-note'),state.inventoryUpdated?`${refreshing?'Refreshing… Showing the saved inventory. ':''}${usesBazarr()?'Bazarr sync':'Last inspection'}: ${fmtDate(state.inventoryUpdated)}${state.status?.inventory?.partial?' · Partial inventory':''}`:refreshing?'Building your first inventory…':'');
    const emptyMessage=state.inventory.length?'No files match your search or filters.':'No inventory yet. Choose Inspect library to read available subtitle tracks.';
    const viewKey=JSON.stringify([visibleRows,visibleRows.length?'':emptyMessage]);
    if (viewKey===state.inventoryViewKey) {syncMediaSelection();return;}
    state.inventoryViewKey=viewKey;body.replaceChildren();
    if (!count) { const tr=document.createElement('tr'),td=document.createElement('td'); td.colSpan=6; td.className='table-empty'; td.textContent=emptyMessage; tr.append(td); body.append(tr);syncMediaSelection(); return; }
    for (const row of visibleRows) {
      const tr=document.createElement('tr');tr.tabIndex=0;tr.classList.toggle('inventory-row-empty',!row.tracks?.length&&!row.error);tr.setAttribute('aria-label',`Subtitle details: ${row.name||row.path}`);tr.addEventListener('click',()=>openTrackDrawer(row));tr.addEventListener('keydown',event=>{if(event.target===tr&&['Enter',' '].includes(event.key)){event.preventDefault();openTrackDrawer(row);}});const name=document.createElement('td'); name.className='file-cell'; const fileIcon=document.createElement('span'); fileIcon.className=`file-icon ${row.type==='sidecar'?'srt':''}`;fileIcon.append(iconSvg('file')); const filename=document.createElement('span'); filename.className='file-name'; filename.textContent=row.name || row.path; filename.title=row.path || row.name; const copy=document.createElement('span');copy.className='file-cell-copy';copy.append(filename);const context=document.createElement('small');context.textContent=(row.path||'').replace(/[/\\][^/\\]+$/,'');copy.append(context);const fileContent=document.createElement('div');fileContent.className='file-cell-content';fileContent.append(fileIcon,copy);name.append(fileContent);
      const type=document.createElement('td');type.className='processing-cell';const badge=document.createElement('span'),processing=row.processing||{},coverage=coverageState(row),failed=coverage!=='covered'&&(!!row.error||!!processing.failed);badge.className=`status-pill ${coverage==='covered'?'completed':failed?'failed':coverage==='needed'?'queued':processing.pending_ocr?'queued':'cancelled'}`;badge.textContent=coverage==='covered'?'Already covered':failed?'Failed':coverage==='needed'?'Missing SRT':coverage==='unavailable'?'No matching source':processing.pending_ocr?'Awaiting OCR':processing.generated_srt_count>0?'Ready':row.metadata_pending?'Needs inspection':'Inspected';type.append(badge);if(row.coverage?.reason){const reason=document.createElement('small');reason.textContent=row.coverage.reason;reason.title=row.coverage.reason;type.append(reason);}const problem=failureMessage(row);if(failed&&problem){const hint=document.createElement('small');hint.textContent=problem.length>140?`${problem.slice(0,137)}…`:problem;hint.title=problem;type.append(hint);}
      const tracks=document.createElement('td');tracks.className='track-count';tracks.textContent=row.error ? row.error : `${(row.tracks || []).length} track${(row.tracks || []).length===1?'':'s'}`;
      const langCell=document.createElement('td');langCell.className='coverage-cell';const lang=document.createElement('div');lang.className='language-tags';langCell.append(lang);for(const target of rowExistingTargets(row)){const pill=document.createElement('span');pill.className='format-badge coverage-existing';pill.textContent=`${targetLabel(target)} · SRT exists`;lang.append(pill);}for(const target of rowMissingTargets(row)){const pill=document.createElement('span');pill.className='format-badge coverage-missing';pill.textContent=`${targetLabel(target)} · SRT missing`;lang.append(pill);}for(const track of (row.tracks||[]).slice(0,3))lang.append(trackBadge(track));if(row.tracks?.length>3){const more=document.createElement('span');more.className='muted';more.textContent=`+${row.tracks.length-3}`;lang.append(more);}if(!lang.childNodes.length)lang.textContent='—';
      const action=document.createElement('td'),button=document.createElement('button');action.className='file-action';button.type='button';button.className='row-action';button.textContent='Details';button.title='Review tracks and choose which to convert';button.addEventListener('click',event=>{event.stopPropagation();openTrackDrawer(row);});action.append(button);
      if(coverage==='unavailable'){
        const retry=document.createElement('button');retry.type='button';retry.className='row-action retry-inventory';retry.textContent='Retry inventory';retry.title='Sync inventory and inspect this file again after adding a matching subtitle source';retry.addEventListener('click',async event=>{event.stopPropagation();retry.disabled=true;try{await startJob({kind:'inspect',paths:[row.path],rootIds:row.root_id?[row.root_id]:[]});}finally{retry.disabled=false;}});action.append(retry);
      }
      const selection=document.createElement('td'),check=document.createElement('input');selection.className='selection-cell';check.type='checkbox';check.className='media-select';check.value=row.path||'';check.disabled=!mediaSelectable(row);check.setAttribute('aria-label',`Select ${row.name||row.path}`);check.title=check.disabled?coverage==='covered'?'This media already has SRT coverage':coverage==='unavailable'?'Add a matching subtitle source, then retry inventory':'Inspect this media to resolve its error':'Select this media for conversion';selection.title=check.title;selection.append(check);selection.addEventListener('click',event=>event.stopPropagation());check.addEventListener('change',()=>{if(check.checked&&state.selectedMedia.size>=64){check.checked=false;notify('Select up to 64 media files per conversion.','error');}else if(check.checked)state.selectedMedia.add(row.path);else state.selectedMedia.delete(row.path);syncMediaSelection();});
      tr.append(selection,name,type,tracks,langCell,action); body.append(tr);
    }
    syncMediaSelection();
    scheduleTableFit();
  };
  const failureMessage = row => {
    if(coverageState(row)==='covered')return '';
    const value=row.error||row.processing?.last_error||row.processing?.error||row.processing?.error_detail;
    return typeof value==='string'?value:row.processing?.failed?'A conversion failed. Open Activity to review the recorded track errors.':'';
  };
  const syncDrawerSelection = () => {
    const count=state.drawerSelection.size;$('#convert-selected-tracks').disabled=!count||count>64;
    $('#convert-selected-tracks').title=count>64?'Select up to 64 tracks per conversion':!count?'Select at least one supported track to convert':'Convert the selected subtitle tracks';
    setText($('#convert-selected-tracks'),`Convert ${count} track${count===1?'':'s'}`);
    const selected=$$('#track-drawer-list input:checked'),image=selected.some(input=>input.dataset.image==='true');
    setText($('#track-drawer-hint'),count>64?'Select up to 64 tracks per conversion.':!count&&coverageState(state.selectedFile||{})==='covered'?'No extraction needed: valid SRT files already cover the wanted languages.':`${count} selected · ${image?'Image tracks use the configured OCR backend; text tracks use CPU extraction.':'Text tracks use CPU extraction.'}`);
  };
  const openTrackDrawer = row => {
    state.selectedFile=row;$('#force-covered-tracks').checked=false;state.modalTrigger=document.activeElement;state.drawerSelection=new Set();const list=$('#track-drawer-list');list.replaceChildren();
    setText($('#track-drawer-title'),row.name||'Subtitle tracks');setText($('#track-drawer-path'),row.path);const failure=$('#track-drawer-failure'),message=failureMessage(row);failure.replaceChildren();failure.classList.toggle('hidden',!message);if(message){const text=document.createElement('p');text.textContent=message;const activity=document.createElement('button');activity.className='text-button';activity.type='button';activity.textContent='Review errors in Activity';activity.addEventListener('click',()=>{closeDialog($('#track-drawer-backdrop'));$('#activity-status').value='issues';$('#activity-kind').value='subtitle';renderJobs();navigate('activity');});failure.append(text,activity);}
    const tracks=row.tracks||[],counts=new Map();for(const track of tracks){const id=track.id??track.number;if(Number.isSafeInteger(id))counts.set(id,(counts.get(id)||0)+1);}
    tracks.forEach((track,index)=>{const id=track.id??track.number,format=trackFormat(track),single=tracks.length===1&&row.type==='sidecar'&&id==null,key=single?'source':id;
      const supported=single||Number.isSafeInteger(id)&&counts.get(id)===1,permitted=!format.image||state.settings?.permissions?.ocr===true;
      const label=document.createElement('label');label.className='track-choice';const input=document.createElement('input');input.type='checkbox';const target={language:track.language||'und',forced:!!track.forced,hi:!!(track.hi||track.sdh||track.hearing_impaired)},covered=rowExistingTargets(row).some(existing=>targetMatches(target,existing)),needed=usesBazarr()?rowMissingTargets(row).some(wanted=>targetMatches(wanted,target)):true;input.dataset.baseDisabled=String(!supported||!permitted||!!row.error);input.dataset.covered=String(covered);input.disabled=!supported||!permitted||!!row.error||covered;input.dataset.image=String(format.image);input.checked=!input.disabled&&needed&&languageCode(track.language)!=='und'&&(isAll(state.selectedLanguages)||state.selectedLanguages.some(code=>languageCode(code)===languageCode(track.language)||languageCode(code)===languageCode(track.language).split('-')[0]));if(input.checked)state.drawerSelection.add(key);input.dataset.trackKey=String(key);input.setAttribute('aria-label',`Convert track ${id??index+1}, ${langLabel(track.language||'und')}, ${format.label}`);input.addEventListener('change',()=>{if(input.checked)state.drawerSelection.add(key);else state.drawerSelection.delete(key);syncDrawerSelection();});
      const copy=document.createElement('span'),title=document.createElement('strong'),source=document.createElement('small');title.textContent=`Track ${id??index+1} · ${langLabel(track.language||'und')}`;source.className='track-source';source.textContent=`${covered?'SRT already covers this target · ':''}${track.codec||track.codec_id||track.format||format.label} · ${row.type==='sidecar'?'Sidecar':'Embedded'}${!supported?' · Inspect again to obtain an unambiguous track ID':!permitted?' · OCR permission locked':''}`;copy.append(title,source);
      const flags=document.createElement('span');flags.className='track-flags';flags.append(trackBadge(track));for(const [key,text] of [['forced','Forced'],['hi','SDH'],['sdh','SDH'],['hearing_impaired','SDH']])if(track[key]&&!(text==='SDH'&&flags.textContent.includes('SDH'))){const flag=document.createElement('span');flag.className='type-badge';flag.textContent=text;flags.append(flag);}label.append(input,copy,flags);list.append(label);
    });
    if(!tracks.length){const empty=document.createElement('p');empty.className='muted';empty.textContent=row.metadata_pending?'Track metadata is pending. Inspect this file to read its tracks.':'No subtitle tracks are recorded for this file.';list.append(empty);}
    syncDrawerSelection();showDialog($('#track-drawer-backdrop'));$('.modal-close',$('#track-drawer-backdrop')).focus();
  };
  const convertDrawerSelection = async () => {
    const row=state.selectedFile;if(!row||!state.drawerSelection.size)return;const button=$('#convert-selected-tracks');button.disabled=true;
    const trackIds=state.drawerSelection.has('source')?undefined:[...state.drawerSelection];const result=await startJob({kind:'extract',paths:[row.path],rootIds:row.root_id?[row.root_id]:[],languages:['*'],trackIds,retry:row.processing?.failed===true,force:$('#force-covered-tracks').checked});
    if(result)closeDialog($('#track-drawer-backdrop'));else syncDrawerSelection();
  };
  const inspect = async () => { const result=await startJob({kind:'inspect',rootIds:selectedRootIds('#inventory-root')}); if (!result) return; if (Array.isArray(result.results)) { state.inventory=result.results; state.inventoryTruncated=!!result.truncated; renderInventory(); } };
  const renderLanguageList = () => {
    const list=$('#language-list'), term=$('#language-search').value.trim().toLowerCase(); list.replaceChildren();
    const allLabel=document.createElement('label'); allLabel.className='language-option all-option'; const all=document.createElement('input'); all.type='checkbox'; all.checked=isAll(state.draftLanguages); all.addEventListener('change',()=>{state.draftLanguages=all.checked?['*']:[];renderLanguageList();}); const allCopy=document.createElement('span'); const allTitle=document.createElement('strong'); allTitle.textContent=usesBazarr()?'Bazarr profile languages':'All tagged languages'; const allHint=document.createElement('small'); allHint.textContent=usesBazarr()?'Fill missing language and normal / forced / SDH targets from each media profile':'Use tagged tracks; untagged tracks require explicit selection'; allCopy.append(allTitle,allHint); allLabel.append(all,allCopy); list.append(allLabel);
    const codes=new Set(languages.map(x=>x[0].toLowerCase())); for (const code of state.draftLanguages) if (code!=='*' && !codes.has(code.toLowerCase())) languages.push([code,`${langLabel(code)} · ${code.toUpperCase()}`]);
    for (const [code,name] of languages) {
      if (term && !`${name} ${code}`.toLowerCase().includes(term)) continue;
      const label=document.createElement('label'); label.className='language-option'; const input=document.createElement('input'); input.type='checkbox'; input.value=code; input.checked=!isAll(state.draftLanguages) && state.draftLanguages.some(item=>item.toLowerCase()===code.toLowerCase()); input.addEventListener('change',()=>{if(isAll(state.draftLanguages)){state.draftLanguages=[];$('.all-option input',list).checked=false;}state.draftLanguages=input.checked?[...new Set([...state.draftLanguages,code])]:state.draftLanguages.filter(item=>item!==code);updateLanguageCount();}); const copy=document.createElement('span'); const title=document.createElement('strong'); title.textContent=name; const detail=document.createElement('small'); detail.textContent=code.toUpperCase(); copy.append(title,detail); label.append(input,copy); list.append(label);
    }
    updateLanguageCount();
  };
  const updateLanguageCount = () => {setText($('#language-count'),isAll(state.draftLanguages)?usesBazarr()?'Bazarr profile targets selected':'All tagged languages selected':`${state.draftLanguages.length} language${state.draftLanguages.length===1?'':'s'} selected`);$('#apply-languages').disabled=!state.draftLanguages.length;};
  const showDialog = backdrop => {backdrop.classList.remove('hidden');$('.shell').inert=true;document.body.classList.add('modal-open');};
  const openLanguageDialog = target => { state.languageTarget=target; state.modalTrigger=document.activeElement; state.draftLanguages=[...(target==='settings'?state.defaultLanguages:state.selectedLanguages)];setText($('#apply-languages'),target==='settings'?'Apply default languages':'Use for this conversion');setText($('#language-modal .modal-copy'),usesBazarr()?'Bazarr profiles define required language and normal, forced, and SDH targets. Select all profile languages, or narrow the targets to specific languages.':'Select tagged languages to convert. Untagged tracks require explicit selection. Existing valid SRT targets are skipped.'); $('#language-search').value='';showDialog($('#language-modal')); renderLanguageList(); $('#language-list').scrollTop=0; $('.language-modal-body').scrollTop=0; $('#language-search').focus(); };
  const closeDialog = backdrop => { if (!backdrop || backdrop.classList.contains('hidden')) return; backdrop.classList.add('hidden');$('.shell').inert=false;document.body.classList.remove('modal-open'); const jobTrigger=backdrop.id==='job-drawer-backdrop'?$$('#jobs-list [data-view-job]').find(button=>button.dataset.viewJob===state.selectedJobId):null;if (state.modalTrigger?.isConnected) state.modalTrigger.focus();else if(jobTrigger)jobTrigger.focus({preventScroll:true});else{const heading=$(`#page-${state.currentPage} h1`);heading?.setAttribute('tabindex','-1');heading?.focus({preventScroll:true});} };
  const addCustomLanguage = () => { const input=$('#custom-language'), code=input.value.trim().toLowerCase(); if (!/^[a-z]{2,3}(?:-[a-z0-9]{2,8})*$/.test(code)) { notify('Enter a valid ISO language code, such as pl or pt-BR.','error'); return; } languages.push([code,code.toUpperCase()]); state.draftLanguages=isAll(state.draftLanguages)?[code]:[...new Set([...state.draftLanguages,code])]; input.value=''; renderLanguageList(); };
  const buildIntegrationCard = (provider, config={}) => {
    const card=$('#integration-template').content.firstElementChild.cloneNode(true); card.dataset.provider=provider;
    const title={sonarr:'Sonarr',radarr:'Radarr',jellyfin:'Jellyfin',bazarr:'Bazarr'}[provider]; const description={sonarr:'Trigger subtitle scans when episodes are imported, and notify Sonarr when sidecars are ready.',radarr:'Trigger subtitle scans when movies are imported, and notify Radarr when sidecars are ready.',jellyfin:'Refresh selected Jellyfin libraries after subtitle conversion.',bazarr:'Automatically sync and scan affected media after each conversion batch. Bazarr may download missing subtitles according to its own rules during sync.'}[provider];
    setText($('.integration-title',card),title); setText($('.integration-description',card),description);
    const usage=document.createElement('div');usage.className='path-map-field';
    const usageTitle=document.createElement('strong');usageTitle.textContent='How this integration is used';
    const usageCopy=document.createElement('p');usageCopy.className='field-explainer';usageCopy.textContent={sonarr:'An optional Sonarr Connect webhook starts conversion when episodes are imported or renamed. New SRT files trigger a rescan of the matching series.',radarr:'An optional Radarr Connect webhook starts conversion when movies are imported or renamed. New SRT files trigger a rescan of the matching movie.',jellyfin:'New SRT files trigger metadata refreshes within your selected movie and series libraries. Refresh selected libraries runs a broader manual refresh.',bazarr:'Profiles define the wanted subtitle languages and variants. New SRT files trigger a scan of matching media and an inventory refresh. Check inventory reads Bazarr and reports its current media counts.'}[provider];
    const last=document.createElement('p');last.className='field-explainer integration-last-result';last.textContent='No conversion notification recorded yet.';usage.append(usageTitle,usageCopy,last);$('.integration-fields',card).append(usage);
    $('.integration-enabled',card).checked=!!config.enabled; $('.integration-url',card).value=config.url || '';
    const key=$('.integration-key',card),saved=$('.api-key-saved',card),replace=$('.replace-integration-key',card);key.value='';key.classList.toggle('hidden',!!config.api_key_set);saved?.classList.toggle('hidden',!config.api_key_set);setText(saved,config.api_key_set?'••••• saved':'');replace?.classList.toggle('hidden',!config.api_key_set);setText($('.api-key-hint',card),'');
    replace?.addEventListener('click',()=>{key.classList.remove('hidden');saved.classList.add('hidden');replace.classList.add('hidden');key.placeholder='Enter replacement API key';key.focus();});
    if (provider==='jellyfin') {
      $('.api-key-field',card).querySelector('span').childNodes[0].textContent='API key ';
      $('.integration-url',card).placeholder='http://jellyfin:8096'; $('.integration-key',card).placeholder='Jellyfin API key';
      $('.jellyfin-fields',card).classList.remove('hidden');
      $('.integration-url',card).readOnly=true; $('.integration-key',card).readOnly=true;
      key.value='';key.classList.add('managed-value','hidden');saved?.classList.remove('hidden');setText(saved,config.api_key_set?'••••• saved · managed by environment':'Not configured · managed by environment');replace?.classList.add('hidden');
      for(const input of [$('.integration-url',card),key]){input.setAttribute('aria-readonly','true');input.title='Locked: managed by deployment environment';}
      for(const lock of $$('.managed-lock',card)){lock.classList.remove('hidden');lock.title='Managed by deployment environment';}
      setText($('.api-key-hint',card),'URL and key are managed by JELLYFIN_INTERNAL_URL and JELLYFIN_API_KEY.');
      renderMappings(card,(state.settings?.path_mappings || []).filter(item=>!item.provider || item.provider==='jellyfin'));
      populateLibraryOptions(card,state.jellyfinLibraries.length?state.jellyfinLibraries.filter(item=>/movie/i.test(item.type)):config.movie_libraries || [],state.jellyfinLibraries.length?state.jellyfinLibraries.filter(item=>/series|tv/i.test(item.type)):config.series_libraries || [],config.movie_libraries || [],config.series_libraries || []);
      $('.refresh-movies',card).checked=!!config.refresh_movies; $('.refresh-series',card).checked=!!config.refresh_series; $('.refresh-mode',card).value=config.refresh_mode || 'immediate';
      $('.refresh-integration',card).textContent='Refresh selected libraries';
    } else {
      $('.integration-url',card).placeholder={sonarr:'http://sonarr:8989',radarr:'http://radarr:7878',bazarr:'http://bazarr:6767'}[provider];
      $('.integration-key',card).placeholder='API key'; renderMappings(card,(state.settings?.path_mappings || []).filter(item=>!item.provider || item.provider===provider));
      if(provider==='bazarr')$('.refresh-integration',card).textContent='Check inventory';
    }
    if(provider==='bazarr'){const callout=$('.bazarr-auto-download-note',card);callout.classList.remove('hidden');callout.setAttribute('role','note');callout.textContent='Bazarr may automatically download subtitles. After conversion, Subforge asks Bazarr to scan affected media. Downloads follow Bazarr’s profiles and download rules; review those in Bazarr if you only want local conversion.';}
    $('.test-integration',card).addEventListener('click',()=>testIntegration(provider,card));
    $('.refresh-integration',card).addEventListener('click',()=>refreshIntegration(provider,card));
    $$('.load-libraries',card).forEach(button=>button.addEventListener('click',()=>loadLibraries(card)));
    $('.add-path-map',card).addEventListener('click',()=>{const entries=readMappings(card); entries.push({remote:'',local:''});renderMappings(card,entries);settingsDirty();});
    $('.test-path-mapping',card)?.addEventListener('click',()=>testPathMapping(provider,card));
    const trackEdit=event=>{if(!event.target.matches('.mapping-test-path'))settingsDirty();};card.addEventListener('input',trackEdit);card.addEventListener('change',trackEdit);
    for(const input of $$('.switch input',card))input.setAttribute('role','switch');
    card.dataset.collapse=`integration-${provider}`;makePanelCollapsible(card);return card;
  };
  const populateLibraryOptions = (card,movies,series,selectedMovies=movies,selectedSeries=series) => {
    for (const [selector,values,selected] of [['.movie-libraries',movies,selectedMovies],['.series-libraries',series,selectedSeries]]) {
      const container=$(selector,card); container.replaceChildren();
      if (!values.length) { const muted=document.createElement('span'); muted.className='muted'; muted.textContent='Load libraries to choose where refreshes apply.'; container.append(muted); continue; }
      for (const [index,item] of values.entries()) { const label=document.createElement('label'); label.className='library-check'; const input=document.createElement('input'); input.type='checkbox'; input.value=typeof item==='string'?item:item.id; input.checked=selected.some(value=>(typeof value==='string'?value:value.id)===input.value);const cached=state.jellyfinLibraries.find(library=>library.id===input.value),copy=document.createElement('span'),text=document.createElement('strong');text.textContent=(typeof item==='object'?item.name:null)||cached?.name||`${selector==='.movie-libraries'?'Movie library':'TV library'} ${index+1} (load names)`;copy.append(text);label.append(input,copy); container.append(label); }
    }
  };
  const renderMappings = (card,entries) => {
    const list=$('.path-map-list',card); list.replaceChildren();
    for (const mapping of entries) {
      const row=document.createElement('div'); row.className='path-map-row'; const remote=document.createElement('input'); remote.className='input map-remote'; remote.value=mapping.remote || ''; remote.placeholder='Remote path, e.g. /tv'; remote.setAttribute('aria-label','Remote path'); const arrow=document.createElement('span'); arrow.textContent='→'; const local=document.createElement('input'); local.className='input map-local'; local.value=mapping.local || ''; local.placeholder='Local path, e.g. /media/tv'; local.setAttribute('aria-label','Local path'); const remove=document.createElement('button'); remove.type='button'; remove.className='icon-button remove-map'; remove.setAttribute('aria-label','Remove path mapping'); remove.title='Remove path mapping';remove.textContent='×'; remove.addEventListener('click',()=>{row.remove();settingsDirty();}); row.append(remote,arrow,local,remove); list.append(row);
    }
  };
  const readMappings = card => $$('.path-map-row',card).map(row=>({remote:$('.map-remote',row).value.trim(),local:$('.map-local',row).value.trim()})).filter(item=>item.remote || item.local);
  const testPathMapping = async (provider,card) => {
    const button=$('.test-path-mapping',card),result=$('.mapping-test-result',card),path=$('.mapping-test-path',card).value.trim();
    if(!path){setText(result,'Enter a full file path as reported by the server.');return;}
    button.disabled=true;setText(result,'Testing draft mapping…');
    try{const data=await api(`/api/integrations/${provider}/mapping-test`,{method:'POST',body:JSON.stringify({path,mappings:readMappings(card)})});setText(result,data.matched?`${data.mapped_path} · ${data.accessible?'Accessible in a configured media folder':'Not accessible in configured media folders'}`:'No mapping matches this path. Check the server path prefix.');}
    catch(error){setText(result,error.message);}finally{button.disabled=false;}
  };
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
    const groups=new Map();for(const root of roots){const key=root.volume_id||root.id||root.path;if(!groups.has(key))groups.set(key,[]);groups.get(key).push(root);}
    let volumeNumber=0;for(const members of groups.values()){
      volumeNumber++;const first=members[0],heading=document.createElement('tr'),summary=document.createElement('td');heading.className='root-volume-summary';summary.colSpan=4;const name=document.createElement('strong');name.textContent=first.volume_label==='Storage volume'?`Storage volume ${volumeNumber}`:first.volume_label||'Storage volume';const free=members.find(root=>Number.isFinite(root.free_bytes));summary.append(name,document.createTextNode(` · ${free?`${formatSpace(free.free_bytes)} free`:'Free space unavailable'} · ${members.length===1?'1 folder':`shared by ${members.length} folders`}`));heading.append(summary);body.append(heading);
    for(const root of members){
      const row=document.createElement('tr'),path=document.createElement('td'),space=document.createElement('td'),status=document.createElement('td'),actions=document.createElement('td');
      const pathButton=document.createElement('button');pathButton.type='button';pathButton.className='root-folder-path';pathButton.textContent=root.path;pathButton.title='Browse this folder';pathButton.disabled=root.status!=='ready';pathButton.addEventListener('click',()=>openRootFolderDialog(root.path));path.append(pathButton);
      space.textContent=root.volume_label||'Storage volume';
      const badge=document.createElement('span');badge.className=`status-pill ${root.status==='ready'?'success':'failed'}`;badge.textContent=root.status==='ready'?'Available':'Unavailable';status.append(badge);
      const group=document.createElement('div');group.className='root-folder-actions';
      const scan=document.createElement('button');scan.type='button';scan.className='icon-button scan-root-folder';scan.textContent='↻';scan.title='Scan this folder';scan.setAttribute('aria-label',`Scan ${root.path}`);scan.disabled=root.status!=='ready';scan.addEventListener('click',()=>startJob({kind:'scan',rootIds:[root.id]}));
      const remove=document.createElement('button');remove.type='button';remove.className='icon-button remove-root-folder';remove.textContent='×';remove.title='Remove from scan folders';remove.setAttribute('aria-label',`Remove ${root.path}`);remove.addEventListener('click',()=>saveRootFolders(roots.filter(item=>item.path!==root.path).map(item=>item.path)));
      group.append(scan,remove);actions.append(group);row.append(path,space,status,actions);body.append(row);
    }
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
    showDialog($('#root-folder-modal'));$('#folder-path').focus();await browseRootFolder(selected);
  };
  const persistSettings = async () => { const data=await api('/api/settings',{method:'PUT',body:JSON.stringify(gatherSettings())}); state.settings=data;if(state.status)state.status.settings={...state.status.settings,integrations:data.integrations};state.integrationChecks.clear(); return data; };
  const loadSettings = async () => {
    try {
      const data=await api('/api/settings'); state.settings=data;state.defaultLanguages=data.languages||['*'];if(!state.quickLanguagesChanged)state.selectedLanguages=[...state.defaultLanguages];
      const ocrEnabled=data.permissions?.ocr===true;
      setText($('#ocr-permission-state'),ocrEnabled?'Enabled':'Locked');
      setText($('#ocr-permission-note'),ocrEnabled?'Image subtitle conversion is enabled for this service.':'Text subtitles are enabled. To include image subtitles, enable CONVERTER_ENABLE_OCR and use a 1536 MiB memory limit.');
      for (const provider of ['sonarr','radarr','jellyfin','bazarr']) { const node=$(`#${provider}-settings`); node.replaceChildren(buildIntegrationCard(provider,{...integrationDefaults(),...(data.integrations?.[provider] || {})})); }
      updateLanguageLabels();renderInventoryAuthority(); updateHookStates(); updateIntegrationUsage();updateIntegrationDots();
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
    return {languages:state.defaultLanguages,integrations,path_mappings};
  };
  const saveSettings = async () => {
    const button=$('#save-settings'), note=$('#settings-save-note'); button.disabled=true; setText(note,'Saving…');
    try { await persistSettings();discardSettings();notify('Settings saved.','success'); }
    catch(error) { setText(note,error.message); notify(`Settings not saved: ${error.message}`,'error'); }
    finally { button.disabled=false;saveController?.render();setTimeout(()=>setText(note,''),5000); }
  };
  const rememberLibraries = libraries => {state.jellyfinLibraries=(libraries||[]).filter(item=>typeof item?.id==='string'&&typeof item.name==='string').slice(0,100).map(({id,name,type})=>({id,name,type}));try{sessionStorage.setItem('subforge.jellyfin-libraries',JSON.stringify(state.jellyfinLibraries));}catch{}};
  const setIntegrationResult = (node,text,kind) => {setText(node,text);node.className=`test-result integration-result result-${kind}`;node.setAttribute('role','status');};
  const updateIntegrationDots = () => {
    for(const dot of $$('[data-integration-dot]')){const provider=dot.dataset.integrationDot,config=state.status?.settings?.integrations?.[provider]||state.settings?.integrations?.[provider]||{},check=state.integrationChecks.get(provider);let latest=null;
      for(const job of jobList()){for(const result of job.notifications||[]){const at=Number(result?.recorded_at||result?.sent_at||0)*1000;if(result?.provider===provider&&typeof result.ok==='boolean'&&at>0&&(!latest||at>latest.at))latest={ok:result.ok,at};}}
      const observed=check&&(!latest||check.at>=latest.at)?check:latest,stale=state.liveMode!=='connected'||!state.statusReceivedAt||Date.now()-state.statusReceivedAt>30000||observed&&Date.now()-observed.at>300000||provider==='bazarr'&&state.inventoryAuthority?.status!=='ready',kind=!config.enabled?'disabled':observed?observed.ok?stale?'stale':'connected':'error':'unknown';dot.dataset.state=kind;dot.textContent=({connected:'✓',stale:'!',error:'!',disabled:'—',unknown:'?'})[kind];dot.setAttribute('role','img');const label=kind==='disabled'?'Integration disabled':observed?`Last ${observed===check?'connection test':'notification result'} ${observed.ok?'succeeded':'failed'} · ${absoluteDate(observed.at)}. ${kind==='stale'?'Status is stale; test again.':'Green means a successful response within 5 minutes, with current dashboard data; it does not guarantee future requests.'}`:'Configured; connection has not been checked';dot.title=label;dot.setAttribute('aria-label',label);
    }
  };
  const testIntegration = async (provider,card) => { const result=$('.test-result',card),button=$('.test-integration',card); button.disabled=true;setIntegrationResult(result,'Testing…','pending');
    try { if(state.settingsDirty)throw new Error('Save or discard changes before using this connection.'); const response=await api(`/api/integrations/${provider}/test`,{method:'POST',body:'{}'}),ok=response.connected!==false;state.integrationChecks.set(provider,{ok,at:Date.now()});setIntegrationResult(result,ok?`Connected · ${{sonarr:'Sonarr',radarr:'Radarr',jellyfin:'Jellyfin',bazarr:'Bazarr'}[provider]}${response.version?` ${response.version}`:''}`:`Failed: ${response.message||'Connection rejected'}`,ok?'success':'error');if(provider==='jellyfin'&&Array.isArray(response.libraries)){rememberLibraries(response.libraries);const config=state.settings?.integrations?.jellyfin||{};populateLibraryOptions(card,state.jellyfinLibraries.filter(x=>/movie/i.test(x.type)),state.jellyfinLibraries.filter(x=>/series|tv/i.test(x.type)),config.movie_libraries||[],config.series_libraries||[]);} }
    catch(error) {state.integrationChecks.set(provider,{ok:false,at:Date.now()});setIntegrationResult(result,`Failed: ${error.message}`,'error');}
    finally { button.disabled=false;updateSettingsActions();updateIntegrationDots(); }
  };
  const refreshIntegration = async (provider,card) => { if(provider==='jellyfin'&&!window.confirm('Refresh all selected Jellyfin libraries? This can trigger a broad metadata scan and increase server load.'))return;const result=$('.test-result',card),button=$('.refresh-integration',card); button.disabled=true; setText(result,'Queueing refresh…'); result.className='test-result muted';
    try { if(state.settingsDirty)throw new Error('Save or discard changes before using this connection.'); const response=await api(`/api/integrations/${provider}/refresh`,{method:'POST',body:'{}'}); setText(result,response.message || 'Refresh job queued.'); result.className='test-result success-text'; notify(response.message || `${provider} refresh queued.`,'success'); await pollStatus(); }
    catch(error) { setText(result,error.message); result.className='test-result error-text'; notify(error.message,'error'); }
    finally { button.disabled=false;updateSettingsActions(); }
  };
  const loadLibraries = async card => { try { const data=await api('/api/integrations/jellyfin/libraries');rememberLibraries(data.libraries); const config=state.settings?.integrations?.jellyfin || {}; populateLibraryOptions(card,state.jellyfinLibraries.filter(x=>/movie/i.test(x.type)),state.jellyfinLibraries.filter(x=>/series|tv/i.test(x.type)),config.movie_libraries||[],config.series_libraries||[]); notify('Jellyfin libraries loaded.','success'); } catch(error) { notify(error.message,'error'); } };
  const updateHookStates = () => { const integrations=state.status?.settings?.integrations || state.settings?.integrations || {}; for(const name of ['sonarr','radarr']) { const enabled=!!integrations[name]?.webhook_enabled; const node=$(`#${name}-hook-state`); if(node){node.className=`connection-state ${enabled?'is-on':''}`;setText(node,enabled?'READY':'TOKEN REQUIRED');} } };
  const updateIntegrationUsage = () => {
    for(const provider of ['sonarr','radarr','jellyfin','bazarr']) {
      const node=$(`[data-provider="${provider}"] .integration-last-result`);if(!node)continue;
      const receipts=jobList().flatMap(job=>job.notifications||[]).filter(item=>item?.provider===provider&&typeof item.ok==='boolean'&&(!Object.hasOwn(item,'sent_at')||Number.isFinite(item.sent_at)&&item.sent_at>0));
      receipts.sort((a,b)=>(b.sent_at||0)-(a.sent_at||0));const match=receipts[0];
      if(match?.sent_at)timestamp(node,match.sent_at,`Last notification sent: ${match.pending?'Indexing pending':match.ok?'Succeeded':'Failed'} · `);else setText(node,match?`Last notification sent: ${match.ok?'Succeeded':'Failed'} · Time unavailable (legacy record)`:'No notification sent yet. Connection tests and inventory reads are recorded separately.');
    }
  };
  const editionLabel = edition => ({cpu:'CPU only',cuda:'CUDA'})[edition] || 'Unavailable';
  const updateEdition = edition => {
    if(!['cpu','cuda'].includes(edition))return;
    state.edition=edition;
    const node=$('#header-edition');setText(node,editionLabel(edition));node.hidden=false;node.dataset.edition=edition;node.title=edition==='cuda'?'GPU processing is available when a device and worker slots are configured.':'CPU processing; GPU workers are unavailable.';
  };
  const updateVersion = (version,edition) => {updateEdition(edition);if(typeof version==='string'&&/^\d+\.\d+\.\d+(?:[-+][a-zA-Z0-9.-]+)?$/.test(version)){setText($('#version-label'),`v${version}`);$('#version-label').title=`Subforge ${version} (${editionLabel(state.edition)})`;$('.brand').title=$('#version-label').title;}};
  const intervalLabel = seconds => seconds%604800===0?`Every ${seconds/604800===1?'week':`${seconds/604800} weeks`}`:seconds%86400===0?`Every ${seconds/86400===1?'day':`${seconds/86400} days`}`:`Every ${seconds/3600===1?'hour':`${seconds/3600} hours`}`;
  const markTasksDirty = () => {state.tasksDirty=true;saveController?.render();};
  const renderTasks = () => {
    if(!state.tasks)return;
    updateVersion(state.tasks.version,state.tasks.edition);
    const failed=state.tasks.pipeline?.failed_files??state.status?.pipeline?.failed_files;
    setText($('#task-failure-scope'),`${Number.isFinite(failed)?failed.toLocaleString():'Unknown'} saved file failures across conversion state. Task results below count failures in that run only. Review saved failures →`);
    const paused=!!state.tasks.paused;setText($('#scheduler-state'),paused?'Automation paused':'Automation on');$('#scheduler-state').className=`status-pill ${paused?'cancelled':'completed'}`;
    if(state.tasksDirty){for(const task of state.tasks.tasks||[]){const run=$(`#tasks-body tr[data-task-id="${task.id}"] .task-run`);if(run){run.disabled=task.available===false||['running','queued'].includes(task.status)||state.taskRequests.has(task.id);run.textContent=state.taskRequests.has(task.id)?'Queueing…':task.status==='running'?'Running…':task.status==='queued'?'Queued':'Run now';}}return;}
    $('#pause-schedules').checked=paused;
    const retry=$('#ocr-retry-failed');if(retry)state.retryFailedTracks=retry.checked;
    const retryBox=$('.task-retry-controls');if(retryBox)retryBox.remove();
    const body=$('#tasks-body');body.replaceChildren();
    for(const task of state.tasks.tasks || []) {
      const row=document.createElement('tr');row.dataset.taskId=task.id;
      const name=document.createElement('td'),copy=document.createElement('div');copy.className='task-name';const title=document.createElement('strong'),description=document.createElement('small');title.textContent=({discovery:'Library discovery',ocr:'Subtitle OCR',inventory:'Media inventory',health:'Health check',cleanup:'Cache cleanup',backup:'State backup'})[task.id]||task.name;description.textContent=task.description;copy.append(title,description);name.append(copy);
      if(task.id==='ocr'){const label=document.createElement('label'),check=document.createElement('input'),text=document.createElement('span');label.className='check-row task-retry-choice';check.type='checkbox';check.id='ocr-retry-failed';check.checked=state.retryFailedTracks;check.addEventListener('change',()=>{state.retryFailedTracks=check.checked;});text.textContent='Retry unchanged failed tracks in this manual run';label.title='Automatic OCR always skips unchanged failures. Retrying can repeat costly recognition work.';label.append(check,text);copy.append(label);}
      const schedule=document.createElement('td'),controls=document.createElement('div');schedule.dataset.label='Schedule';controls.className='task-schedule';const label=document.createElement('label');label.className='switch';const enabled=document.createElement('input');enabled.type='checkbox';enabled.setAttribute('role','switch');enabled.className='task-enabled';enabled.checked=!!task.enabled;enabled.disabled=task.available===false;enabled.setAttribute('aria-label',`Automatically run ${task.name}`);const labelText=document.createElement('b');labelText.textContent='Automatic';label.append(enabled,document.createElement('span'),labelText);
      const interval=document.createElement('select');interval.className='input compact-input task-interval';interval.setAttribute('aria-label',`${task.name} interval`);interval.disabled=task.available===false;
      const choices=[...new Set([3600,21600,43200,86400,172800,604800,1209600,2592000,task.interval_seconds])].filter(value=>value>=task.min_interval_seconds).sort((a,b)=>a-b);
      for(const value of choices)interval.add(new Option(intervalLabel(value),String(value)));interval.value=String(task.interval_seconds);enabled.addEventListener('change',markTasksDirty);interval.addEventListener('change',markTasksDirty);controls.append(label);
      const mode=document.createElement('select');mode.className='input compact-input task-schedule-mode';mode.setAttribute('aria-label',`${task.name} schedule type`);mode.add(new Option('Repeat interval','interval'));mode.add(new Option('Days and time','calendar'));mode.value=task.schedule?'calendar':'interval';mode.disabled=task.available===false;controls.append(mode,interval);
      const calendar=document.createElement('div');calendar.className='task-calendar';const at=document.createElement('input');at.type='time';at.className='input task-time';at.value=task.schedule?.time_of_day||'03:00';at.setAttribute('aria-label',`${task.name} time of day`);calendar.append(at);
      const offset=document.createElement('select');offset.className='input task-offset';offset.setAttribute('aria-label',`${task.name} fixed UTC offset`);const currentOffset=task.schedule?.utc_offset_minutes??-new Date().getTimezoneOffset();for(const value of [...new Set([...Array.from({length:113},(_,i)=>i*15-840),currentOffset])].sort((a,b)=>a-b)){offset.add(new Option(`UTC${value<0?'−':'+'}${String(Math.floor(Math.abs(value)/60)).padStart(2,'0')}:${String(Math.abs(value)%60).padStart(2,'0')}`,String(value)));}offset.value=String(currentOffset);calendar.append(offset);
      const days=document.createElement('div');days.className='task-weekdays';for(const [day,text] of ['Sun','Mon','Tue','Wed','Thu','Fri','Sat'].entries()){const dayLabel=document.createElement('label'),check=document.createElement('input');check.type='checkbox';check.value=String(day);check.className='task-day';check.checked=(task.schedule?.days_of_week||[0,1,2,3,4,5,6]).includes(day);check.setAttribute('aria-label',`${task.name} on ${text}`);dayLabel.append(check,document.createTextNode(text));days.append(dayLabel);}calendar.append(days);const offsetHint=document.createElement('small');offsetHint.textContent='Fixed UTC offset; adjust for daylight saving time.';calendar.append(offsetHint);controls.append(calendar);const toggleMode=()=>{calendar.hidden=mode.value!=='calendar';interval.hidden=mode.value==='calendar';};toggleMode();mode.addEventListener('change',()=>{toggleMode();markTasksDirty();});calendar.addEventListener('change',markTasksDirty);for(const input of $$('input,select',calendar))input.disabled=task.available===false;schedule.append(controls);
      const next=document.createElement('td');next.dataset.label='Next execution';next.className='task-next';if(task.available!==false&&!paused&&task.enabled&&task.next_run)next.append(timeNode(task.next_run));else next.textContent=task.available===false?'Permission disabled':paused?'Paused':task.enabled?'Waiting for next run':'Manual only';
      const last=document.createElement('td');last.dataset.label='Last result';last.className='task-last-result';const result=document.createElement('span');result.textContent=plainCopy(task.last_result)||'Not run yet';last.append(result);if(task.last_run)last.append(document.createElement('br'),timeNode(task.last_run));
      const action=document.createElement('td'),run=document.createElement('button');run.type='button';run.className='button button-secondary task-run';run.textContent=state.taskRequests.has(task.id)?'Queueing…':task.status==='running'?'Running…':task.status==='queued'?'Queued':'Run now';run.setAttribute('aria-label',`Run ${task.name}`);run.title=task.blocked_reason||`Run ${task.name} now`;run.disabled=task.available===false||['running','queued'].includes(task.status)||state.taskRequests.has(task.id);run.addEventListener('click',()=>runTask(task.id));action.append(run);row.append(name,schedule,next,last,action);body.append(row);
    }
    setText($('#task-schedule-note'),'Manual Run buttons remain available while schedules are paused. Pause does not cancel current work.');
  };
  const loadTasks = async () => {
    if(state.tasksBusy||!state.csrf)return;state.tasksBusy=true;
    try{state.tasks=await api('/api/tasks');state.tasksLoadedAt=Date.now();renderTasks();}
    catch(error){notify(`Could not load scheduled tasks: ${error.message}`,'error');}
    finally{state.tasksBusy=false;}
  };
  const loadBackups = async () => {
    const list=$('#backup-list');if(!list||state.backupsBusy)return;state.backupsBusy=true;
    try{const data=await api('/api/backups');setText($('#backup-location'),data.location||'/data/backups');list.replaceChildren();
      if(data.restore_pending){const pending=document.createElement('p');pending.className='backup-restore-status';pending.setAttribute('role','status');pending.textContent='Restore staged. Restart Subforge to apply it. Changes and new jobs are blocked until restart; the backup may replace dashboard credentials.';list.append(pending);}
      const note=document.createElement('p');note.className='form-hint';note.textContent='The latest 3 local backups are retained. Restore requires idle workers and a service restart; files absent from the backup are preserved. Media files are unchanged.';list.append(note);
      for(const item of data.items||[]){const row=document.createElement('div');row.className='backup-row';const copy=document.createElement('div'),name=document.createElement('strong'),detail=document.createElement('small');name.append(timeNode(item.created_at));detail.textContent=`${formatSpace(item.count_bytes)} · ${item.file_count} files`;copy.append(name,detail);const download=document.createElement('a');download.className='button button-secondary backup-download';download.textContent='Download';download.href=`/api/backups/${encodeURIComponent(item.id)}/download`;download.setAttribute('download','');const restore=document.createElement('button');restore.type='button';restore.className='button button-secondary backup-restore';restore.textContent='Restore';restore.addEventListener('click',async()=>{if(state.settingsDirty||state.tasksDirty||state.workersDirty){notify('Save or discard unsaved changes before restoring a backup.','error');return;}if(!window.confirm(`Restore the backup from ${absoluteDate(item.created_at)}? It replaces saved application state at the next service restart. Newer settings and conversion history will be replaced. Media files are unchanged.`))return;restore.disabled=true;try{const result=await api(`/api/backups/${encodeURIComponent(item.id)}/restore`,{method:'POST',body:JSON.stringify({confirm:item.id})});notify(result.message||'Restore staged. Restart Subforge to apply it.','success');const status=document.createElement('p');status.className='form-hint backup-restore-status';status.setAttribute('role','status');status.textContent=result.message||'Restore staged. Restart Subforge to apply this backup.';list.prepend(status);}catch(error){notify(error.message,'error');}finally{restore.disabled=false;}});row.append(copy,download,restore);list.append(row);}
      if(!(data.items||[]).length){const empty=document.createElement('p');empty.textContent='No backups yet. Run State backup above to create one.';list.append(empty);}
    }catch(error){setText(list,`Could not load backups: ${error.message}`);}finally{state.backupsBusy=false;}
  };
  const saveTasks = async () => {
    const tasks=$$('#tasks-body tr[data-task-id]').map(row=>({id:row.dataset.taskId,enabled:$('.task-enabled',row).checked,interval_seconds:Number($('.task-interval',row).value),schedule:$('.task-schedule-mode',row).value==='calendar'?{time_of_day:$('.task-time',row).value,days_of_week:$$('.task-day:checked',row).map(input=>Number(input.value)),utc_offset_minutes:Number($('.task-offset',row).value)}:null}));
    $('#save-task-schedules').disabled=true;
    try{state.tasks=await api('/api/tasks',{method:'PUT',body:JSON.stringify({paused:$('#pause-schedules').checked,tasks})});state.tasksDirty=false;renderTasks();notify('Task schedules saved.','success');}
    catch(error){$('#save-task-schedules').disabled=false;notify(error.message,'error');}
  };
  const runTask = async id => {
    if(['discovery','ocr','inventory'].includes(id)&&!window.confirm(`${({discovery:'Run Library discovery across all media folders? This scans files and converts missing text subtitles.',ocr:`Run Subtitle OCR across all media folders? Image recognition can take a long time.${state.retryFailedTracks?' Unchanged failed tracks will be retried.':''}`,inventory:'Refresh the entire media inventory? This scans all configured folders and may take time.'})[id]}`))return;
    if(state.taskRequests.has(id))return;state.taskRequests.add(id);renderTasks();
    try{const body=id==='ocr'?{retry:state.retryFailedTracks}:{};const result=await api(`/api/tasks/${encodeURIComponent(id)}/run`,{method:'POST',body:JSON.stringify(body)});if(id==='ocr'){state.retryFailedTracks=false;$('#ocr-retry-failed').checked=false;}notify(result.message||'Task queued.');await pollStatus();await loadTasks();if(id==='backup')await loadBackups();}
    catch(error){notify(error.message,'error');}
    finally{state.taskRequests.delete(id);renderTasks();}
  };
  const renderWorkerNodes = remote => {
    const target=$('#worker-nodes');target.replaceChildren();
    const add=(name,description,status)=>{const row=document.createElement('div');row.className='worker-node';const copy=document.createElement('div'),title=document.createElement('strong'),detail=document.createElement('small'),pill=document.createElement('span');title.textContent=name;detail.textContent=description;pill.className=`status-pill ${status==='online'?'completed':status==='busy'?'running':'cancelled'}`;pill.textContent=status;copy.append(title,detail);row.append(copy,pill);target.append(row);};
    const summary=$('#worker-nodes-summary');
    if(!remote){setText(summary,'Node status unavailable');const hint=document.createElement('p');hint.className='muted';hint.textContent='Remote-node status is unavailable. Refresh nodes to retry.';target.append(hint);return;}
    if(!remote.enabled){setText(summary,'Remote workers disabled');const hint=document.createElement('p');hint.className='muted';hint.textContent='Remote workers are disabled. Expand the setup instructions to connect another machine.';target.append(hint);return;}
    const nodes=Array.isArray(remote.nodes)?remote.nodes:[];setText(summary,`${nodes.length} remote node${nodes.length===1?'':'s'} · ${Number(remote.active||0)} active`);
    for(const node of nodes){const status=({working:'busy',idle:'online'})[node.status]||node.status||'online';add(node.name||node.id,`${node.resource==='gpu'?'GPU OCR':node.resource==='cpu_ocr'?'CPU OCR':'CPU'} · ${Number(node.active||0)} active · last seen ${node.last_seen_seconds??'—'}s ago`,status);}
    if(!nodes.length){const hint=document.createElement('p');hint.className='muted';hint.textContent='No remote workers connected. Expand the setup instructions to add a machine.';target.append(hint);}
  };
  const loadWorkerNodes = async () => {try{renderWorkerNodes(await api('/api/nodes'));}catch(error){notify(`Could not load worker nodes: ${error.message}`,'error');}};
  const reviewConversion = (rootIds,paths=[]) => {
    if(usesBazarr()&&(state.inventorySource!=='bazarr'||!['ready','refresh_pending'].includes(state.inventoryAuthority?.status))){notify('Sync Bazarr before converting.','error');return;}
    state.pendingConversion={kind:'scan',rootIds,paths:[...paths],languages:[...state.selectedLanguages],includeOcr:state.settings?.permissions?.ocr===true};state.modalTrigger=document.activeElement;
    const scope=$('#conversion-scope');scope.replaceChildren();
    const roots=(state.status?.roots||[]).filter(root=>!rootIds.length||rootIds.includes(root.id));
    if(paths.length){const dt=document.createElement('dt'),dd=document.createElement('dd');dt.textContent='Selected media';dd.textContent=`${paths.length} files: ${paths.map(path=>state.inventory.find(row=>row.path===path)?.name||path).join(', ')}`;scope.append(dt,dd);}
    for(const [key,value] of [['Folders',roots.map(root=>root.label||root.path||root.id).join(', ')||'All configured media folders'],['Languages',usesBazarr()&&isAll(state.selectedLanguages)?'Each media item’s Bazarr profile':languageDescription(state.selectedLanguages)],['Operation',usesBazarr()?'Fill missing SRT targets from Bazarr profiles; covered files are skipped':'Convert only languages not already covered by valid SRT files'],['Image subtitles',state.settings?.permissions?.ocr===true?'Run OCR using available CPU or GPU slots':'OCR is disabled by service permissions']]){const dt=document.createElement('dt'),dd=document.createElement('dd');dt.textContent=key;dd.textContent=value;scope.append(dt,dd);}
    showDialog($('#conversion-modal'));$('#confirm-conversion').focus();
  };
  const workersVisible = () => state.currentPage==='overview'&&!document.hidden;
  const workerFields={cpu_workers:'#worker-cpu-count',ocr_workers:'#worker-ocr-count',gpu_workers:'#worker-gpu-count'};
  const workerDirty = () => {state.workersDirty=true;saveController?.render();renderWorkerSteppers();};
  const renderWorkerSteppers = () => {
    const data=state.workersInfo;if(!data)return;
    for(const [kind,selector] of Object.entries(workerFields)){
      const input=$(selector),ceiling=Number(data.max_requested??data.deployment?.[`max_${kind}`]??0);
      input.max=String(ceiling);
      input.disabled=ceiling===0||state.workersSaving||kind==='gpu_workers'&&data.edition==='cpu';
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
    setText($('#worker-pool-state'),!state.workersSampleHealthy?'Stale':paused?'Paused':total?`${total} active`:'Ready');$('#worker-pool-state').className=`status-pill ${!state.workersSampleHealthy||paused?'cancelled':total?'running':'completed'}`;
    const allowance=measuredNumber(cpu.effective_cores)??measuredNumber(detected.cpu_allowance_cores);
    setText($('#worker-cpu-allowance'),allowance===null?'Unavailable':`${allowance.toLocaleString()} CPU${allowance===1?'':'s'}`);
    setText($('#worker-cpu-usage'),!state.systemSampleHealthy?'Stale · reconnecting':measuredNumber(cpu.quota_usage_percent)!==null?`${cpu.quota_usage_percent.toFixed(1)}% in use`:'Sampling usage…');
    setText($('#worker-ram'),!state.systemSampleHealthy?'Stale · reconnecting':measuredNumber(memory.cgroup_current_bytes)===null?'Unavailable':`${formatBytes(memory.cgroup_current_bytes)}${measuredNumber(memory.cgroup_limit_bytes)!==null?` / ${formatBytes(memory.cgroup_limit_bytes)}`:''}`);
    const meter=$('#worker-memory-meter');meter.value=memory.cgroup_limit_bytes>0?Math.min(1,(memory.cgroup_current_bytes||0)/memory.cgroup_limit_bytes):0;meter.hidden=!state.systemSampleHealthy||measuredNumber(memory.cgroup_current_bytes)===null||!(memory.cgroup_limit_bytes>0);
    setText($('#worker-gpu-availability'),data.edition==='cpu'?'CPU only':detected.gpu_available?(detected.gpu_verification==='verified'?'GPU ready':'GPU enabled'):'Unavailable');
    const gpuReason=$('#worker-gpu-reason');setText(gpuReason,detected.gpu_available?'Verified before each image subtitle file':plainCopy(detected.gpu_reason)||'GPU recognition is unavailable.');gpuReason.title=plainCopy(detected.gpu_reason)||'The GPU, available memory, and recognition models are checked before each file.';
    for(const [kind,resource,selector,limit] of [['cpu_workers','cpu','#worker-cpu-capacity','cpu_limit_reason'],['ocr_workers','cpu_ocr','#worker-ocr-capacity','ocr_limit_reason'],['gpu_workers','gpu','#worker-gpu-capacity','gpu_limit_reason']]){
      const capacity=$(selector),admission=data.admission?.[resource],limited=Number(effective[kind]??0)<Number(allocation[kind]||0),reason=admission?.allowed===false?admission.reason:limited?data[limit]:'';
      setText(capacity,`${active[kind]||0} active · ${effective[kind]??0} slots${reason?` · ${plainCopy(reason)}`:''}`);capacity.classList.toggle('capacity-blocked',admission?.allowed===false);capacity.title=plainCopy([`${allocation[kind]||0} requested`,data[limit],admission?.reason].filter(Boolean).join('. '));
    }
    if(!state.workersDirty&&!state.workersSaving)setText($('#worker-allocation-note'),paused?'Paused. Resume and save to accept new files.':'Changes apply to new files. Running work keeps its allocation.');
    const body=$('#workers-body');body.replaceChildren();
    for(const worker of (data.workers||[])){
      const row=document.createElement('tr');const elapsed=Math.max(0,Math.floor(Date.now()/1000-Number(worker.started||Date.now()/1000)));
      for(const [label,value] of [['Worker',`#${worker.id}`],['Processor',worker.resource==='gpu'?'GPU OCR':worker.resource==='cpu_ocr'?'CPU OCR':'CPU'],['Media file',shortPath(worker.path||'')],['Status',worker.status||'Processing'],['Elapsed',`${Math.floor(elapsed/60)}m ${elapsed%60}s`]]){const cell=document.createElement('td');cell.dataset.label=label;cell.textContent=value;if(label==='Media file'){cell.className='worker-file';cell.title=worker.path||'';}row.append(cell);}body.append(row);
    }
    const hasWorkers=body.children.length>0;$('#active-workers-panel').hidden=!hasWorkers;
    if(!hasWorkers){const row=document.createElement('tr'),cell=document.createElement('td');cell.colSpan=5;cell.className='table-empty';cell.textContent='No file workers running.';row.append(cell);body.append(row);}
    setText($('#worker-live-state'),`${(data.workers||[]).length} active`);
  };
  const loadWorkers = async (quiet=false) => {
    if(!state.csrf||state.workersBusy||state.workersSaving)return;state.workersBusy=true;
    try{state.workersInfo=await api('/api/workers');state.workersReceivedAt=performance.now();state.workersSampleHealthy=true;state.receivedAt=Date.now();renderLiveState();renderWorkers();renderWorkerNodes(state.workersInfo.remote);}
    catch(error){state.workersSampleHealthy=false;renderWorkers();setText($('#worker-live-state'),'Disconnected · retrying');if(!quiet)notify(`Could not load workers: ${error.message}`,'error');}
    finally{state.workersBusy=false;}
  };
  const saveWorkers = async event => {
    event.preventDefault();if(state.workersSaving||!state.workersInfo)return;
    const allocation={paused:$('#worker-paused').checked};
    for(const [kind,selector] of Object.entries(workerFields)){const input=$(selector),value=Number(input.value);if(!Number.isInteger(value)||value<Number(input.min)||value>Number(input.max)){notify('Enter a whole requested worker count within the supported range.','error');input.focus();return;}allocation[kind]=value;}
    state.workersSaving=true;$('#save-worker-allocation').disabled=true;renderWorkerSteppers();
    try{state.workersInfo=await api('/api/workers',{method:'PUT',body:JSON.stringify(allocation)});state.workersDirty=false;renderWorkerNodes(state.workersInfo.remote);notify('Worker allocation saved. It applies to new files.','success');}
    catch(error){notify(error.message,'error');}
    finally{state.workersSaving=false;$('#save-worker-allocation').disabled=!state.workersDirty;renderWorkers();if(systemVisible())loadSystem(true);}
  };
  const systemVisible = () => (state.currentPage==='overview'||state.currentPage==='settings'&&state.settingsSection==='status')&&!document.hidden;
  const discardSettings = () => {
    const data=state.settings;if(!data)return;
    state.defaultLanguages=[...(data.languages||['*'])];state.settingsDirty=false;
    for(const provider of ['sonarr','radarr','jellyfin','bazarr'])$(`#${provider}-settings`).replaceChildren(buildIntegrationCard(provider,{...integrationDefaults(),...(data.integrations?.[provider]||{})}));
    setText($('#settings-save-note'),'');updateLanguageLabels();updateSettingsActions();updateIntegrationUsage();updateIntegrationDots();renderInventoryAuthority();
  };
  const createSaveController = () => {
    const checkedSave = async (save,isDirty) => {await save();if(isDirty())throw new Error('Changes could not be saved. Review the error and try again.');};
    saveController=SubforgeSettingsSave.create({groups:{
      settings:{isDirty:()=>state.settingsDirty,save:()=>checkedSave(saveSettings,()=>state.settingsDirty),discard:discardSettings},
      schedules:{isDirty:()=>state.tasksDirty,save:()=>checkedSave(saveTasks,()=>state.tasksDirty),discard:()=>{state.tasksDirty=false;renderTasks();}},
      workers:{isDirty:()=>state.workersDirty,save:()=>checkedSave(()=>saveWorkers({preventDefault(){}}),()=>state.workersDirty),discard:()=>{state.workersDirty=false;renderWorkers();}}
    },onError:error=>notify(error.message,'error'),onStateChange:({saving})=>{for(const node of $$('.settings-section,#worker-allocation-form'))node.inert=saving;updateSettingsActions();}});
    saveController.render();
  };
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
    if(!systemVisible())return;
    if(state.workersSampleHealthy&&performance.now()-state.workersReceivedAt>15000){state.workersSampleHealthy=false;if(workersVisible())renderWorkers();}
    if(!state.systemSampleHealthy||!state.systemInfo)return;
    const elapsed=(performance.now()-state.systemReceivedAt)/1000;
    if(elapsed>15){state.systemSampleHealthy=false;renderLiveState();if(workersVisible())renderWorkers();return;}
    if(state.currentPage!=='settings'||state.settingsSection!=='status')return;
    setText($('#system-uptime'),formatUptime(Number(state.systemInfo.uptime_seconds||0)+elapsed));
  };
  const renderSystem = () => {
    const info=state.systemInfo,policy=info.resource_policy||{},usage=info.resource_usage||{},cpu=usage.cpu||{},memory=usage.memory||{},cache=usage.metadata_cache||{},worker=usage.worker||{};
    updateVersion(info.version,info.edition);
    renderSystemRows($('#system-about'),[
      ['Subforge version',info.version?`v${info.version}`:'Unavailable'],
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
    const setBar=(name,value)=>{const bar=$(`#system-${name}-bar`),label=$(`#system-${name}-bar-label`);if(measuredNumber(value)===null){bar.removeAttribute('value');bar.hidden=true;setText(label,'—');bar.title='Measurement unavailable';}else{bar.hidden=false;bar.value=Math.min(100,value);setText(label,`${Math.round(value)}%`);bar.title=`${value.toFixed(1)}% of allowance`;}};
    setBar('cpu',percent);setBar('ram',limit>0&&measuredNumber(memory.cgroup_current_bytes)!==null?100*memory.cgroup_current_bytes/limit:null);
    const slots=Number(policy.conversion_workers||0)+Number(policy.gpu_workers||0);setBar('workers',slots>0&&measuredNumber(worker.active)!==null?100*worker.active/slots:null);
    setBar('cache',cache.max_bytes>0&&measuredNumber(cache.accounted_bytes)!==null?100*cache.accounted_bytes/cache.max_bytes:null);
    renderSystemRows($('#system-resource-usage'),[
      ['Effective CPU allowance',allowance,'system-cpu-allowance'],['Container CPU usage',cpuUsage,'system-cpu-usage'],
      ['Total memory use',ram,'system-container-ram'],['Subforge memory use',formatBytes(memory.process_rss_bytes),'system-process-ram'],
      ['Active file workers',measuredNumber(worker.active)===null?'Unavailable':`${worker.active} / ${Number(policy.conversion_workers||0)+Number(policy.gpu_workers||0)}${worker.kind?` · ${worker.kind}`:''}`,'system-active-workers'],
      ['Subforge threads',measuredNumber(usage.process?.threads)??'Unavailable','system-process-threads'],
      ['Cached media files',cacheCount===null?'Unavailable':`${cacheCount.toLocaleString()} / ${cacheMaximum?.toLocaleString()??'—'}`,'system-cache-entries'],
      ['Estimated cache size',`${formatBytes(cache.accounted_bytes)}${measuredNumber(cache.max_bytes)!==null?` / ${formatBytes(cache.max_bytes)}`:''}`,'system-cache-bytes']
    ]);
    renderSystemRows($('#system-limits'),[
      ['Effective CPU workers',policy.conversion_workers??'Unavailable'],['Effective CPU OCR slots',policy.ocr_workers??'Unavailable'],['Effective GPU OCR slots',policy.gpu_workers??0],['CPU threads per tool',policy.ocr_threads??'Unavailable'],
      ['Metadata cache budget',`${formatBytes(policy.metadata_cache_max_bytes)} · ${measuredNumber(policy.metadata_cache_max_entries)?.toLocaleString()??'—'} files maximum`]
    ]);
    renderLiveState();
    setText($('#system-policy'),`Active file workers counts running files across CPU and GPU slots (${Number(policy.conversion_workers||0)} CPU + ${Number(policy.gpu_workers||0)} GPU). Effective CPU workers counts only CPU slots; CPU OCR shares those slots. Low memory can reduce parallel work. Set simultaneous file limits on Overview. Automatic discovery converts text subtitles; image subtitles follow the OCR schedule.${policy.ocr_permission?'':' Image recognition is currently disabled.'} CPU and memory ceilings come from deployment settings. Recreate the container after changing those ceilings.`);
  };
  const loadSystem = async (quiet=false) => {
    if(!state.csrf||state.systemBusy)return;state.systemBusy=true;
    try{state.systemInfo=await api('/api/system');state.systemReceivedAt=performance.now();state.systemSampleAt=Date.now();state.receivedAt=state.systemSampleAt;state.systemSampleHealthy=true;renderSystem();if(workersVisible())renderWorkers();}
    catch(error){state.systemSampleHealthy=false;renderLiveState();if(workersVisible())renderWorkers();if(!quiet)notify(`Could not load system status: ${error.message}`,'error');}
    finally{state.systemBusy=false;}
  };
  const syncSystemMonitor = () => {
    clearTimeout(state.systemTimer);clearInterval(state.systemClock);
    if(!systemVisible())return;
    tickSystemUptime();state.systemClock=setInterval(tickSystemUptime,1000);
  };
  const pageNames={overview:'Overview',library:'Library',activity:'Activity',settings:'Settings'};
  const settingsSections=['general','media','tasks','status','sonarr','radarr','jellyfin','bazarr'];
  state.settingsSection='general';
  const routeHash = (page,section) => `#/${page}${page==='settings'?`/${section}`:''}`;
  const readRoute = () => {
    const parts=location.hash.replace(/^#\/?/,'').replace(/\/+$/,'').split('/');
    const page=parts[0] || 'overview';
    if(page==='settings'&&parts[1]==='workers'&&parts.length===2)return {page:'overview',section:'general'};
    if(page==='system'&&parts.length<=2&&['tasks','status'].includes(parts[1]||'tasks'))return {page:'settings',section:parts[1]||'tasks'};
    if(!Object.hasOwn(pageNames,page))return {page:'overview',section:'general'};
    if(page==='settings') {
      const section=parts[1] || 'general';
      if(parts.length<=2&&settingsSections.includes(section))return {page,section};
    } else if(parts.length===1)return {page,section:'general'};
    return {page:'overview',section:'general'};
  };
  const syncNavigation = () => {const mobile=window.matchMedia('(max-width: 800px)').matches,open=$('#sidebar').classList.contains('mobile-open'),button=$('#mobile-menu'),collapse=$('#sidebar-collapse');$('.shell').classList.toggle('sidebar-collapsed',!mobile&&state.sidebarCollapsed);$('#sidebar').inert=mobile&&!open;const mobileLabel=open?'Close navigation':'Open navigation',desktopLabel=state.sidebarCollapsed?'Show sidebar':'Hide sidebar';button.setAttribute('aria-expanded',String(mobile&&open));button.setAttribute('aria-label',mobileLabel);button.title=mobileLabel;collapse.setAttribute('aria-expanded',String(!state.sidebarCollapsed));collapse.setAttribute('aria-label',desktopLabel);collapse.title=`${desktopLabel}; stays on the current page`;};
  const closeMobileMenu = (restoreFocus=false) => { const sidebar=$('#sidebar'),button=$('#mobile-menu'),backdrop=$('.sidebar-backdrop'); sidebar.classList.remove('mobile-open');backdrop.classList.remove('visible');backdrop.setAttribute('aria-hidden','true');syncNavigation();if(restoreFocus)button.focus(); };
  const toggleMobileMenu = () => {if(!window.matchMedia('(max-width: 800px)').matches){state.sidebarCollapsed=!state.sidebarCollapsed;try{localStorage.setItem('subforge.sidebar-collapsed',String(state.sidebarCollapsed));}catch{}syncNavigation();return;}const sidebar=$('#sidebar'),button=$('#mobile-menu'),backdrop=$('.sidebar-backdrop'),open=!sidebar.classList.contains('mobile-open');sidebar.classList.toggle('mobile-open',open);backdrop.classList.toggle('visible',open);backdrop.setAttribute('aria-hidden',String(!open));syncNavigation();if(open)$('.nav-link[data-page]',sidebar)?.focus();else button.focus();};
  const showRoute = ({page,section}) => {
    const fromMobileMenu=$('#sidebar').classList.contains('mobile-open');
    const previousPage=document.activeElement.closest('.page');
    const changed=state.currentPage!==page||(page==='settings'&&state.settingsSection!==section);
    const leavesSettings=state.currentPage!=='settings'||page!=='settings';
    if(changed&&leavesSettings&&saveController&&!saveController.guardNavigation()){history.replaceState(null,'',routeHash(state.currentPage,state.settingsSection));return;}
    state.currentPage=page;
    $('#workspace-scroll').classList.toggle('has-table-page',['library','activity'].includes(page));
    if(page==='settings')state.settingsSection=section;
    $$('.page').forEach(node=>node.classList.toggle('active',node.id===`page-${page}`));
    $$('.nav-link[data-page],.text-button[data-page]').forEach(node=>{const active=node.dataset.page===page;node.classList.toggle('active',active);if(active)node.setAttribute('aria-current','page');else node.removeAttribute('aria-current');});
    $$('.settings-tab').forEach(node=>{const active=node.dataset.settingTab===state.settingsSection;node.classList.toggle('active',active);if(active)node.setAttribute('aria-current','page');else node.removeAttribute('aria-current');});
    $$('.settings-section').forEach(node=>node.classList.toggle('active',node.id===`settings-${state.settingsSection}`));
    setText($('#page-crumb'),page==='settings'?`Settings / ${$('.settings-tab.active')?.textContent||'Preferences'}`:pageNames[page]);closeMobileMenu();if(changed)$('#workspace-scroll').scrollTo({top:0,behavior:'instant'});
    if(changed||fromMobileMenu||(previousPage&&previousPage.id!==`page-${page}`)) {
      const heading=$(`#page-${page} h1`);heading?.setAttribute('tabindex','-1');heading?.focus({preventScroll:true});
    }
    if(changed)state.pendingConversion=null;
    if(page==='settings'&&state.csrf) {
      if(!state.settings)loadSettings();
      if(changed&&section==='media')loadRootFolders();
    }
    const administration=page==='settings'&&['tasks','status','media'].includes(section);$('#refresh-system').classList.toggle('hidden',!administration);
    if(administration&&state.csrf&&changed){if(section==='tasks'){loadTasks();loadBackups();}else if(section!=='media')loadSystem();}
    if(page==='overview'&&state.csrf&&changed){loadSystem();loadWorkers();}
    if(page==='library'){renderInventory();if(state.status)void loadInventory(state.status.inventory);}if(page==='activity')renderJobs();scheduleTableFit();
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
    createSaveController();
    renderLiveState();
    setInterval(()=>{if(!document.hidden){refreshTimes();renderLiveState();updateIntegrationDots();tickSystemUptime();}},1000);
    $('#refresh-backups')?.addEventListener('click',loadBackups);
    $('#view-operation-log')?.addEventListener('click',()=>{const panel=$('#activity-log-panel'),open=panel.classList.contains('hidden');panel.classList.toggle('hidden',!open);$('.jobs-panel').classList.toggle('hidden',open);$('.activity-filters').classList.toggle('hidden',open);setText($('#view-operation-log'),open?'View jobs':'View operation log');$('#view-operation-log').setAttribute('aria-expanded',String(open));if(open&&panel.classList.contains('is-collapsed'))$('.collapse-toggle',panel)?.click();scheduleTableFit();});
    for(const input of $$('.switch input'))input.setAttribute('role','switch');
    $$('.nav-link[data-page]').forEach(button=>$('.nav-icon',button)?.replaceChildren(iconSvg(button.dataset.page)));
    $$('button[aria-label]').forEach(button=>{if(!button.title)button.title=button.getAttribute('aria-label');});
    try{const libraries=JSON.parse(sessionStorage.getItem('subforge.jellyfin-libraries')||'[]');if(Array.isArray(libraries))rememberLibraries(libraries);}catch{}
    try{const saved=JSON.parse(localStorage.getItem('subforge.collapsed-panels')||'[]');if(Array.isArray(saved))state.collapsedPanels=new Set(saved.filter(key=>typeof key==='string'));}catch{}
    $$('[data-collapse]').forEach(makePanelCollapsible);
    const mobileBackdrop=document.createElement('button'); mobileBackdrop.type='button'; mobileBackdrop.className='sidebar-backdrop'; mobileBackdrop.setAttribute('aria-label','Close navigation'); mobileBackdrop.setAttribute('aria-hidden','true'); document.body.append(mobileBackdrop);try{state.sidebarCollapsed=localStorage.getItem('subforge.sidebar-collapsed')==='true';}catch{}syncNavigation();
    $('.brand')?.addEventListener('click',event=>{event.preventDefault();navigate('overview');});
    $$('.nav-link[data-page],.text-button[data-page]').forEach(node=>node.addEventListener('click',()=>navigate(node.dataset.page)));
    $('#mobile-menu').addEventListener('click',toggleMobileMenu); $('#sidebar-collapse').addEventListener('click',toggleMobileMenu); mobileBackdrop.addEventListener('click',()=>closeMobileMenu(true));
    window.addEventListener('resize',()=>{if(!window.matchMedia('(max-width: 800px)').matches)closeMobileMenu();else syncNavigation();});
    $('#refresh-button').addEventListener('click',()=>{pollStatus();if(systemVisible())loadSystem();if(workersVisible())loadWorkers();if(state.currentPage==='settings'&&state.settingsSection==='tasks')loadTasks();if(state.currentPage==='settings'&&state.settingsSection==='media')loadRootFolders();}); $('#clear-view-button').addEventListener('click',()=>pollStatus());
    $$('[data-attention]').forEach(button=>button.addEventListener('click',()=>button.dataset.attention==='paths'?navigate('settings','media'):filterLibrary(button.dataset.attention)));
    $('#convert-selected-tracks').addEventListener('click',convertDrawerSelection);
    $('#inventory-sync-now').addEventListener('click',syncInventoryNow);
    $('#task-failure-scope').addEventListener('click',()=>filterLibrary('failed'));
    $('#inventory-select-all').addEventListener('change',event=>{const checked=event.target.checked;for(const box of $$('.media-select:not(:disabled)')){if(!checked)state.selectedMedia.delete(box.value);else if(state.selectedMedia.size<64)state.selectedMedia.add(box.value);}syncMediaSelection();});
    $('#inventory-clear-selection').addEventListener('click',()=>{state.selectedMedia.clear();syncMediaSelection();});
    $('#inventory-bulk-convert').addEventListener('click',()=>{syncMediaSelection();if(state.selectedMedia.size)reviewConversion([],[...state.selectedMedia]);});
    $('#force-covered-tracks').addEventListener('change',()=>{const force=$('#force-covered-tracks').checked;$$('#track-drawer-list input').forEach(input=>{input.disabled=input.dataset.baseDisabled==='true'||input.dataset.covered==='true'&&!force;if(input.disabled&&input.checked){input.checked=false;state.drawerSelection.delete(input.dataset.trackKey==='source'?'source':Number(input.dataset.trackKey));}});syncDrawerSelection();});
    for(const [selector,checked] of [['#select-all-tracks',true],['#select-no-tracks',false]])$(selector).addEventListener('click',()=>{$$('#track-drawer-list input:not(:disabled)').forEach(input=>{if(input.checked!==checked){input.checked=checked;input.dispatchEvent(new Event('change'));}});});
    $('#inspect-drawer-file').addEventListener('click',async()=>{const row=state.selectedFile;if(!row)return;const result=await startJob({kind:'inspect',paths:[row.path],rootIds:row.root_id?[row.root_id]:[]});if(result)closeDialog($('#track-drawer-backdrop'));});
    $$('[data-action="scan"]').forEach(button=>button.addEventListener('click',()=>reviewConversion(selectedRootIds(state.currentPage==='library'?'#inventory-root':'#quick-root'))));
    $$('[data-action="inspect"]').forEach(button=>button.addEventListener('click',async()=>{const root=state.currentPage==='library'?$('#inventory-root').value:$('#quick-root').value;navigate('library');$('#inventory-root').value=root;await inspect();}));
    $('#quick-form').addEventListener('submit',event=>{event.preventDefault();reviewConversion(selectedRootIds('#quick-root'));});
    $('#confirm-conversion').addEventListener('click',async()=>{if(!state.pendingConversion)return;const button=$('#confirm-conversion');button.disabled=true;try{const result=await startJob(state.pendingConversion);if(result){state.pendingConversion=null;state.selectedMedia.clear();syncMediaSelection();closeDialog($('#conversion-modal'));}}finally{button.disabled=false;}});
    const resetJobsPage=()=>{state.jobsPage=1;$('.jobs-table-wrap').scrollTop=0;renderJobs();saveJobsView();};
    $('#activity-status').addEventListener('change',resetJobsPage);$('#activity-kind').addEventListener('change',resetJobsPage);$('#refresh-worker-nodes').addEventListener('click',loadWorkerNodes);
    restoreJobsView();
    $('#jobs-page-size').addEventListener('change',()=>{const size=$('#jobs-page-size').value;state.jobsPageSize=size==='auto'?'auto':Number(size);resetJobsPage();});
    for(const [selector,delta] of [['#jobs-previous',-1],['#jobs-next',1]])$(selector).addEventListener('click',()=>{state.jobsPage+=delta;$('.jobs-table-wrap').scrollTop=0;renderJobs();saveJobsView();});
    restoreInventoryView();
    const resetInventoryPage=()=>{state.inventoryPage=1;$('.inventory-table-wrap').scrollTop=0;renderInventory();saveInventoryView();};
    $('#inventory-filter').addEventListener('change',resetInventoryPage); $('#inventory-root').addEventListener('change',resetInventoryPage); $('#inventory-search').addEventListener('input',resetInventoryPage);
    $('#inventory-page-size').addEventListener('change',()=>{const size=$('#inventory-page-size').value;state.inventoryPageSize=size==='auto'?'auto':Number(size);resetInventoryPage();});
    const showInventoryPage = page => {state.inventoryPage=page;$('.inventory-table-wrap').scrollTop=0;renderInventory();saveInventoryView();};
    $('#inventory-prev').addEventListener('click',()=>showInventoryPage(state.inventoryPage-1));$('#inventory-next').addEventListener('click',()=>showInventoryPage(state.inventoryPage+1));$('#inventory-page').addEventListener('change',()=>showInventoryPage(Number($('#inventory-page').value)));
    for(const selector of ['#jobs-list','#job-drawer-content'])$(selector).addEventListener('click',event=>{const button=event.target.closest('.cancel-job');if(button)cancelJob(button.dataset.jobId);});
    $('#save-settings').addEventListener('click',()=>saveController.save()); $('#language-search').addEventListener('input',renderLanguageList); $('#add-language').addEventListener('click',addCustomLanguage); $('#custom-language').addEventListener('keydown',event=>{if(event.key==='Enter'){event.preventDefault();addCustomLanguage();}});
    $$('[data-open-languages]').forEach(button=>button.addEventListener('click',()=>openLanguageDialog(button.dataset.openLanguages)));
    $('#apply-languages').addEventListener('click',()=>{if(!state.draftLanguages.length){notify('Select at least one language or choose All languages.','error');return;}if(state.languageTarget==='settings'){state.defaultLanguages=[...state.draftLanguages];settingsDirty();}else{state.selectedLanguages=[...state.draftLanguages];state.quickLanguagesChanged=true;}updateLanguageLabels();closeDialog($('#language-modal'));});
    $$('.modal-close').forEach(button=>button.addEventListener('click',()=>closeDialog(button.closest('.modal-backdrop'))));
    $$('.modal-backdrop').forEach(backdrop=>backdrop.addEventListener('click',event=>{if(event.target===backdrop)closeDialog(backdrop);}));
    $('#add-root-folder').addEventListener('click',()=>openRootFolderDialog());$('#refresh-root-folders').addEventListener('click',loadRootFolders);
    $('#folder-mount').addEventListener('change',()=>browseRootFolder($('#folder-mount').value));$('#browse-root-folder').addEventListener('click',()=>browseRootFolder($('#folder-path').value.trim()));$('#folder-path').addEventListener('input',()=>{state.folderRequest++;state.currentFolder=null;$('#select-root-folder').disabled=true;setText($('#folder-browser-note'),'Browse to check this folder before adding it.');});$('#folder-path').addEventListener('keydown',event=>{if(event.key==='Enter'){event.preventDefault();browseRootFolder($('#folder-path').value.trim());}});
    $('#folder-parent-button').addEventListener('click',()=>{if(state.folderParent)browseRootFolder(state.folderParent);});$('#select-root-folder').addEventListener('click',()=>{if(state.currentFolder)saveRootFolders([...state.rootFolders.roots.map(root=>root.path),state.currentFolder]);});
    $$('.settings-tab').forEach(button=>button.addEventListener('click',()=>navigate('settings',button.dataset.settingTab)));
    $('#refresh-system').addEventListener('click',()=>{if(state.settingsSection==='tasks')loadTasks();else{loadSystem();if(workersVisible())loadWorkers();}});
    $('#worker-allocation-form').addEventListener('submit',event=>{event.preventDefault();void saveController.save();});
    $('#worker-paused').addEventListener('change',workerDirty);
    for(const selector of Object.values(workerFields))$(selector).addEventListener('input',workerDirty);
    $$('[data-worker-step]').forEach(button=>button.addEventListener('click',()=>{const input=$(workerFields[button.dataset.workerStep]),next=Number(input.value)+Number(button.dataset.delta);input.value=String(Math.max(Number(input.min),Math.min(Number(input.max),next)));workerDirty();}));
    $('#pause-schedules').addEventListener('change',markTasksDirty);
    $('#save-task-schedules').addEventListener('click',()=>saveController.save());
    window.addEventListener('popstate',restoreRoute);window.addEventListener('hashchange',restoreRoute);
    window.addEventListener('resize',scheduleTableFit);
    state.tableObserver=new ResizeObserver(scheduleTableFit);
    for(const pane of $$('.inventory-table-wrap,.jobs-table-wrap'))state.tableObserver.observe(pane);
    document.addEventListener('keydown',event=>{
      const modal=$$('.modal-backdrop').find(item=>!item.classList.contains('hidden'));
      if(event.key==='Escape'){if(modal){event.preventDefault();closeDialog(modal);}else if($('#sidebar').classList.contains('mobile-open')){event.preventDefault();closeMobileMenu(true);}return;}
      if(event.key!=='Tab')return;
      if(modal){const focusable=$$('summary,button:not(:disabled),input:not(:disabled),select:not(:disabled),[href],[tabindex]:not([tabindex="-1"])',modal).filter(node=>node.getClientRects().length);if(!focusable.length)return;const first=focusable[0],last=focusable.at(-1);if(event.shiftKey&&document.activeElement===first){event.preventDefault();last.focus();}else if(!event.shiftKey&&document.activeElement===last){event.preventDefault();first.focus();}return;}
      if($('#sidebar').classList.contains('mobile-open')){const focusable=$$('button:not(:disabled),a[href],input:not(:disabled),select:not(:disabled)', $('#sidebar')).filter(node=>!node.closest('[inert]')&&node.getClientRects().length);if(!focusable.length)return;const first=focusable[0],last=focusable.at(-1);if(event.shiftKey&&document.activeElement===first){event.preventDefault();last.focus();}else if(!event.shiftKey&&document.activeElement===last){event.preventDefault();first.focus();}}
    });
  };
  const dashboardLive = SubforgeLive.create({
    status: applyStatus,
    system(data) {state.systemInfo=data;state.systemReceivedAt=performance.now();state.systemSampleAt=Date.now();state.receivedAt=state.systemSampleAt;state.systemSampleHealthy=true;renderLiveState();if(systemVisible())renderSystem();if(workersVisible())renderWorkers();},
    workers(data) {if(state.workersSaving)return;state.workersInfo=data;state.workersReceivedAt=performance.now();state.receivedAt=Date.now();state.workersSampleHealthy=true;renderLiveState();if(workersVisible()){renderWorkers();renderWorkerNodes(data.remote);}},
    connection(mode) {
      state.liveMode=mode;
      if(mode==='connected'){renderLiveState();return;}
      state.systemSampleHealthy=false;state.workersSampleHealthy=false;renderLiveState();
      if(workersVisible())renderWorkers();
    }
  });
  const initialize = async () => {
    bind();restoreRoute();
    try {
      const session=await api('/api/session');state.csrf=session.csrf_token||'';
      if(typeof EventSource!=='undefined'){
        dashboardLive.start();
        // A single bootstrap fallback keeps a capacity-limited stream from leaving the screen empty.
        state.bootstrapFallback=setTimeout(()=>{if(!state.status&&!document.hidden)void pollStatus();},5000);
        await loadSettings();
      }else{
        await Promise.all([pollStatus(),loadSettings()]);
        if(systemVisible())await loadSystem();if(workersVisible())await loadWorkers();
      }
      if(state.currentPage==='settings'&&state.settingsSection==='media')await loadRootFolders();
      if(state.currentPage==='settings'&&state.settingsSection==='tasks')await Promise.all([loadTasks(),loadBackups()]);
    } catch(error) {setConnection(false);notify(`Service initialization failed: ${error.message}`,'error');}
    dashboardLive.start();syncSystemMonitor();
    document.addEventListener('visibilitychange',()=>syncSystemMonitor());
  };
  initialize();
})();
