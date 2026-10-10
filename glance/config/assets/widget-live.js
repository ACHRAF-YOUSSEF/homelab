// Service credentials stay in the relay. This client receives display fields only.
(() => {
  if (typeof pageData === "undefined" || !["media", "downloads"].includes(pageData.slug)) return;
  const topic = pageData.slug === "media" ? "jellyfin" : "qbittorrent";
  const widgetClass = topic === "jellyfin" ? "media-live-jellyfin" : "downloads-live-qbittorrent";
  let stream;
  let content;
  let ui;
  let status;
  let healthy = false;
  let lastSnapshot;
  let progressTimer;
  const rows = new Map();

  function element(tag, classes = "", text = "") {
    const node = document.createElement(tag);
    node.className = classes;
    node.textContent = text;
    return node;
  }
  const number = (value) => Number.isFinite(Number(value)) ? Number(value) : 0;
  const clamp = (value, max) => Math.max(0, Math.min(max, number(value)));
  function speed(value, empty = false) {
    value = Math.max(0, number(value));
    if (empty && value < 1024) return "--";
    return value < 1048576 ? `${(value / 1024).toFixed(0)} KiB/s` : `${(value / 1048576).toFixed(1)} MiB/s`;
  }
  function eta(value) {
    value = number(value);
    if (value >= 8640000) return "∞";
    if (value <= 0) return "--";
    return value >= 3600 ? `${Math.floor(value / 3600)}h ${Math.floor(value / 60) % 60}m` : `${Math.floor(value / 60)}m`;
  }
  function statistic(parent, label) {
    const block = element("div");
    const value = element("div", "color-highlight size-h3");
    value.dataset.liveStat = label.toLowerCase();
    const caption = element("div", "size-h6", label);
    block.append(value, caption);
    parent.append(block);
    return { value, caption };
  }
  function setStatus(message, stale = false) {
    if (!status) return;
    status.textContent = message;
    status.classList.toggle("color-negative", stale);
  }
  function mount() {
    const next = document.querySelector(`.widget.${widgetClass} .widget-content`);
    if (!next) return false;
    if (next !== content) {
      content = next;
      ui = undefined;
      rows.clear();
      status = element("div", "glance-live-status color-subdue size-h6", "Connecting…");
      status.setAttribute("role", "status");
      content.append(status);
    }
    return true;
  }
  function initQbit() {
    const options = content.querySelector("[data-live-view]");
    const detailed = options?.dataset.liveView !== "basic";
    const upload = detailed && options?.dataset.liveMode === "upload";
    let expanded = Boolean(content.querySelector(".container-expanded"));
    const summary = element("div", "flex justify-between text-center");
    const download = statistic(summary, "DOWNLOADING");
    const uploading = upload ? statistic(summary, "UPLOADING") : null;
    const seeding = statistic(summary, "SEEDING");
    const leeching = upload ? null : statistic(summary, "LEECHING");
    const list = element("ul", "list glance-live-torrents");
    list.id = "glance-live-torrent-list";
    const button = element("button", "expand-toggle-button");
    const label = document.createTextNode("");
    button.type = "button";
    button.setAttribute("aria-controls", list.id);
    button.append(label, element("span", "expand-toggle-button-icon"));
    function toggle() {
      list.hidden = !expanded;
      button.classList.toggle("container-expanded", expanded);
      button.setAttribute("aria-expanded", String(expanded));
      label.nodeValue = expanded ? "Show less" : "Show more";
    }
    button.addEventListener("click", () => { expanded = !expanded; toggle(); });
    toggle();
    const detail = element("div", "glance-live-details");
    detail.append(list, button);
    detail.hidden = !detailed;
    content.replaceChildren(summary, detail, status);
    ui = { download, uploading, seeding, leeching, list, button, detail, detailed };
  }
  function qbitRow() {
    const node = element("li", "flex items-center glance-live-torrent");
    const icon = element("div", "size-h4 glance-live-torrent-icon");
    const center = element("div", "glance-live-torrent-main");
    const name = element("div", "text-truncate color-highlight");
    const track = element("div", "glance-live-torrent-progress");
    const bar = element("div");
    track.append(bar);
    center.append(name, track);
    const end = element("div", "glance-live-torrent-end");
    const rate = element("div", "size-sm color-paragraph");
    const remaining = element("div", "size-sm color-paragraph");
    end.append(rate, remaining);
    node.append(icon, center, end);
    return { node, icon, name, track, bar, rate, remaining };
  }
  function torrentIcon(torrent) {
    if (number(torrent.progress) >= 1) return "✔";
    if (["downloading", "forcedDL", "metaDL", "forcedMetaDL"].includes(torrent.state)) return "↓";
    if (["error", "missingFiles"].includes(torrent.state)) return "!";
    if (torrent.state === "checkingResumeData") return "⟳";
    if (["checkingDL", "checkingUP", "allocating"].includes(torrent.state)) return "…";
    return "❚❚";
  }
  // Keep rows and controls alive when values change, including their order and focus.
  function reconcile(items, create, update) {
    const ids = new Set();
    let previous = null;
    for (const item of items) {
      const id = String(item.id);
      if (ids.has(id)) continue;
      ids.add(id);
      let row = rows.get(id);
      if (!row) {
        row = create();
        row.node.dataset.liveId = id;
        rows.set(id, row);
      }
      update(row, item);
      const expected = previous ? previous.nextSibling : ui.list.firstChild;
      if (row.node !== expected) ui.list.insertBefore(row.node, expected);
      previous = row.node;
    }
    for (const [id, row] of rows) {
      if (!ids.has(id)) { row.node.remove(); rows.delete(id); }
    }
  }
  function renderQbit(data) {
    if (!ui) initQbit();
    ui.download.value.textContent = speed(data.downloadSpeed);
    if (ui.uploading) ui.uploading.value.textContent = speed(data.uploadSpeed);
    ui.seeding.value.textContent = String(Math.max(0, number(data.seedingCount)));
    if (ui.leeching) ui.leeching.value.textContent = String(Math.max(0, number(data.downloadingCount)));
    const torrents = Array.isArray(data.torrents) ? data.torrents : [];
    if (ui.detailed) reconcile(torrents, qbitRow, (row, torrent) => {
      row.icon.textContent = torrentIcon(torrent);
      row.name.textContent = torrent.name;
      const percent = clamp(torrent.progress, 1) * 100;
      row.track.title = `${percent.toFixed(1)}%`;
      row.bar.style.width = `${percent}%`;
      row.rate.textContent = speed(torrent.downloadSpeed, true);
      row.remaining.textContent = eta(torrent.eta);
    });
    ui.detail.hidden = !ui.detailed || !torrents.length;
  }
  function initJellyfin() {
    const summary = element("div", "text-center");
    const count = statistic(summary, "SESSIONS");
    const empty = element("p", "color-subdue margin-top-20", "Nothing is playing right now.");
    const list = element("div", "jellyfin-streams");
    content.replaceChildren(summary, empty, list, status);
    ui = { count, empty, list };
  }
  function sessionRow() {
    const node = element("div", "card gap-5");
    const head = element("div", "flex justify-between items-center gap-10");
    const user = element("strong", "color-primary text-truncate");
    const state = element("span", "color-subdue");
    head.append(user, state);
    const title = element("div", "text-truncate");
    const episode = element("div", "text-truncate color-subdue");
    const footer = element("div", "flex justify-between gap-10 color-subdue size-h6");
    const device = element("span", "text-truncate");
    const method = element("span");
    footer.append(device, method);
    const track = element("div", "jellyfin-progress");
    const bar = element("div");
    track.append(bar);
    node.append(head, title, episode, footer, track);
    return { node, user, state, title, episode, device, method, track, bar };
  }
  function renderJellyfin(data) {
    if (!ui) initJellyfin();
    const sessions = Array.isArray(data.sessions) ? data.sessions : [];
    ui.count.value.textContent = String(sessions.length);
    ui.count.value.classList.toggle("color-positive", sessions.length > 0);
    ui.count.value.classList.toggle("color-highlight", !sessions.length);
    ui.count.caption.classList.toggle("color-positive", sessions.length > 0);
    ui.empty.hidden = sessions.length > 0;
    ui.list.hidden = !sessions.length;
    reconcile(sessions, sessionRow, (row, session) => {
      row.user.textContent = session.userName;
      row.state.textContent = session.isPaused ? "Paused" : "Playing";
      const episode = session.type === "Episode";
      row.title.textContent = episode ? session.seriesName : session.title;
      row.episode.hidden = !episode;
      row.episode.textContent = `S${String(number(session.season)).padStart(2, "0")}E${String(number(session.episode)).padStart(2, "0")} · ${session.title}`;
      row.device.textContent = `${session.client} · ${session.deviceName}`;
      row.method.textContent = session.playMethod;
      row.track.hidden = number(session.durationTicks) <= 0;
      row.session = session;
      row.receivedAt = performance.now();
      updateProgress(row);
    });
  }
  function updateProgress(row) {
    const session = row.session;
    const elapsedMs = healthy && !document.hidden && !session.isPaused
      ? Math.min(15000, Math.max(0, performance.now() - row.receivedAt)) : 0;
    const position = Math.max(0, number(session.positionTicks)) + elapsedMs * 10000 * clamp(session.playbackRate ?? 1, 4);
    const percent = number(session.durationTicks) > 0 ? clamp(position / number(session.durationTicks), 1) * 100 : 0;
    row.bar.style.width = `${percent}%`;
    row.track.title = `${percent.toFixed(1)}%`;
  }
  function receive(event) {
    try {
      const snapshot = JSON.parse(event.data);
      if (!snapshot || !["ok", "stale", "warming"].includes(snapshot.status)) return;
      if (!mount()) return;
      healthy = snapshot.status === "ok";
      lastSnapshot = snapshot;
      if (snapshot.data) {
        if (topic === "jellyfin") renderJellyfin(snapshot.data);
        else renderQbit(snapshot.data);
      }
      setStatus(healthy ? "Live" : snapshot.data ? "Updates unavailable · showing last data" : "Waiting for service…", !healthy);
      status.title = snapshot.updatedAt ? `Updated ${new Date(snapshot.updatedAt).toLocaleTimeString()} · ${snapshot.source}` : "";
    } catch {
      // Never log event contents, torrent names or personal session information.
      healthy = false;
      setStatus("Updates unavailable", true);
    }
  }
  function close() {
    stream?.close();
    stream = undefined;
    healthy = false;
    clearInterval(progressTimer);
  }
  function connect() {
    if (document.hidden || stream || !mount()) return;
    if (typeof EventSource === "undefined") { setStatus("Live updates require EventSource", true); return; }
    const base = String(pageData.baseURL || "").replace(/\/$/, "");
    stream = new EventSource(`${base}/live/events?page=${encodeURIComponent(pageData.slug)}`);
    stream.addEventListener(topic, receive);
    stream.onopen = () => { if (!lastSnapshot) setStatus("Waiting for service…"); };
    stream.onerror = () => { healthy = false; setStatus("Reconnecting… · showing last data", true); };
    if (topic === "jellyfin") progressTimer = setInterval(() => {
      if (healthy && !document.hidden) for (const row of rows.values()) updateProgress(row);
    }, 1000);
  }
  document.addEventListener("visibilitychange", () => {
    if (document.hidden) { close(); setStatus("Updates paused"); }
    else connect();
  });
  window.addEventListener("pagehide", close);
  window.addEventListener("pageshow", connect);
  // Glance inserts widgets asynchronously after fetching page content.
  const observer = new MutationObserver(() => {
    if (mount()) { observer.disconnect(); connect(); }
  });
  observer.observe(document.body, { childList: true, subtree: true });
  if (mount()) { observer.disconnect(); connect(); }
})();
