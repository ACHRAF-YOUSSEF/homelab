// Only the gateway receives the widget bearer token. Browser events contain safe job summaries.
((factory) => {
  const client = factory();
  if (typeof module === "object" && module.exports) { module.exports = client; return; }
  if (typeof pageData === "undefined" || pageData.slug !== "media") return;
  const start = () => {
    const content = document.querySelector(".widget.media-live-subforge .widget-content");
    if (!content) return false;
    client.mount(content, pageData.baseURL);
    return true;
  };
  const observer = new MutationObserver(() => { if (start()) observer.disconnect(); });
  observer.observe(document.body, { childList: true, subtree: true });
  if (start()) observer.disconnect();
})(() => {
  const finite = value => typeof value === "number" && Number.isFinite(value) && value >= 0;
  const duration = value => {
    if (!finite(value)) return "unavailable";
    value = Math.floor(value);
    return `${value >= 3600 ? `${Math.floor(value / 3600)}h ` : ""}${value >= 60 ? `${Math.floor(value / 60) % 60}m ` : ""}${value % 60}s`;
  };
  const label = value => String(value || "preparing").replaceAll("_", " ");

  function mount(content, baseURL) {
    const doc = content.ownerDocument;
    const host = doc.defaultView;
    const node = (tag, classes = "", text = "") => {
      const element = doc.createElement(tag);
      element.className = classes;
      element.textContent = text;
      return element;
    };
    const publicLink = content.querySelector("[data-subforge-link]")?.getAttribute("href");
    let stream, ticker, ui, healthy = false, receivedAt = 0;
    const rows = new Map();
    const status = node("p", "glance-live-status color-subdue size-h6", "Connecting to live updates…");
    status.setAttribute("role", "status");
    content.append(status);
    function setStatus(text, stale = false) {
      status.textContent = text;
      status.className = `glance-live-status size-h6 ${stale ? "color-negative" : "color-subdue"}`;
    }
    function build() {
      const stats = node("div", "subforge-live-stats");
      const values = ["PROCESSED", "QUEUED", "ROOTS", "LATEST SCAN FAILURES"].map(caption => {
        const cell = node("div");
        const value = node("div", "color-highlight size-h3");
        cell.append(value, node("div", "size-h6", caption));
        stats.append(cell);
        return value;
      });
      const heading = node("h3", "subforge-live-heading size-h6");
      const jobs = node("div", "subforge-live-jobs");
      const empty = node("p", "color-subdue", "No jobs running.");
      const languages = node("div", "subforge-live-languages");
      content.replaceChildren(stats, heading, empty, jobs, node("p", "subforge-live-heading size-h6 color-subdue", "LANGUAGES"), languages);
      if (publicLink) {
        const link = node("a", "block margin-top-15 color-primary", "Open Subforge ↗");
        link.href = publicLink;
        content.append(link);
      }
      content.append(status);
      ui = { values, heading, jobs, empty, languages };
    }
    function createRow(job) {
      const card = node("article", "subforge-live-job");
      card.dataset.jobId = job.id;
      const header = node("div", "subforge-live-job-header");
      const title = node("h4", "color-highlight");
      const state = node("span", "size-h6");
      header.append(title, state);
      const phase = node("p", "color-subdue size-h6");
      const progress = node("div", "subforge-live-progress");
      progress.setAttribute("role", "progressbar");
      progress.setAttribute("aria-valuemin", "0");
      progress.setAttribute("aria-valuemax", "100");
      const bar = node("div");
      progress.append(bar);
      const timing = node("p", "subforge-live-timing size-h6");
      const waiting = node("p", "color-primary size-h6");
      card.append(header, phase, progress, timing, waiting);
      return { card, title, state, phase, progress, bar, timing, waiting, job };
    }
    function timing(row, delta = 0) {
      const job = row.job;
      const elapsed = finite(job.elapsedSeconds) ? job.elapsedSeconds + delta : null;
      const eta = job.status === "running" && job.liveness === "running" && finite(job.etaSeconds)
        ? Math.max(0, job.etaSeconds - delta) : null;
      row.timing.textContent = `Elapsed ${duration(elapsed)} · ${eta === null ? "ETA unavailable" : `Approx. ${duration(eta)} remaining`}`;
    }
    function render(data) {
      if (!ui) build();
      [data.convertedCount, data.queuedCount, data.configuredRootCount, data.recentFailuresCount].forEach((value, index) => {
        ui.values[index].textContent = value.toLocaleString();
      });
      ui.heading.textContent = `ACTIVE JOBS · ${data.jobs.length}`;
      ui.empty.hidden = data.jobs.length !== 0;
      const ids = new Set(data.jobs.map(job => job.id));
      for (const [id, row] of rows) if (!ids.has(id)) { row.card.remove(); rows.delete(id); }
      for (const job of data.jobs) {
        let row = rows.get(job.id);
        if (!row) { row = createRow(job); rows.set(job.id, row); }
        row.job = job;
        row.title.textContent = `${label(job.kind)} · ${job.id.slice(0, 8)}`;
        row.title.title = `Job ${job.id}`;
        const state = job.liveness === "unresponsive" ? "Unresponsive"
          : job.liveness === "stalled" ? "Possibly stalled"
            : job.status === "cancelling" ? "Cancelling"
              : job.liveness === "waiting" ? "Waiting" : label(job.status);
        row.state.textContent = state;
        row.state.className = `size-h6 ${job.liveness === "unresponsive" ? "color-negative" : job.liveness === "running" ? "color-positive" : "color-primary"}`;
        row.phase.textContent = `${label(job.phase)}${job.total > 0 ? ` · ${job.processed}/${job.total} files` : ""}`;
        const percent = finite(job.progressPercent) ? Math.min(100, job.progressPercent) : null;
        row.progress.hidden = percent === null;
        if (percent !== null) {
          row.bar.style.width = `${percent}%`;
          row.progress.setAttribute("aria-valuenow", String(percent));
          row.progress.setAttribute("aria-label", `Job ${job.id}: ${percent}% complete`);
        }
        row.waiting.textContent = job.waitReason || (job.liveness === "stalled" ? "No recent progress reported. Quiet OCR may still be working." : job.liveness === "unresponsive" ? "Coordinator heartbeat overdue. Check Activity in Subforge." : "");
        row.waiting.hidden = !row.waiting.textContent;
        timing(row);
        ui.jobs.append(row.card);
      }
      ui.languages.replaceChildren(...data.selectedLanguages.map(language => node("span", "color-highlight size-h6", language === "*" ? "All languages" : language)));
    }
    function receive(event) {
      try {
        const snapshot = JSON.parse(event.data);
        if (!snapshot || !["ok", "stale", "warming"].includes(snapshot.status)) throw new Error();
        if (snapshot.data !== null && snapshot.data !== undefined) {
          const data = snapshot.data;
          if (![data.convertedCount, data.queuedCount, data.configuredRootCount, data.recentFailuresCount].every(finite)
            || !Array.isArray(data.jobs) || data.jobs.length > 32 || new Set(data.jobs.map(job => job.id)).size !== data.jobs.length
            || !data.jobs.every(job => typeof job.id === "string" && /^[A-Za-z0-9_-]{1,128}$/.test(job.id))
            || !Array.isArray(data.selectedLanguages) || data.selectedLanguages.length > 64
            || !data.selectedLanguages.every(language => typeof language === "string" && language.length <= 64)) throw new Error();
          render(data);
          receivedAt = performance.now();
        }
        healthy = snapshot.status === "ok";
        setStatus(healthy ? "Live" : ui ? "Updates unavailable · showing last data" : "Waiting for Subforge…", !healthy && !!ui);
        status.title = snapshot.updatedAt ? `Updated ${new Date(snapshot.updatedAt).toLocaleTimeString()}` : "";
      } catch {
        healthy = false;
        setStatus("Updates unavailable · showing last data", true);
      }
    }
    function tick() {
      if (!healthy || doc.hidden) return;
      const age = Math.max(0, performance.now() - receivedAt);
      // Polling is configurable up to 60s; allow its 15s request timeout plus margin.
      if (age > 80000) { healthy = false; setStatus("Updates delayed · showing last data", true); return; }
      if (age > 15000) return;
      for (const row of rows.values()) timing(row, Math.floor(age / 1000));
    }
    function close() {
      stream?.close();
      stream = undefined;
      healthy = false;
      clearInterval(ticker);
    }
    function connect() {
      if (doc.hidden || stream || !content.isConnected) return;
      if (typeof EventSource === "undefined") { setStatus("Live updates require EventSource", true); return; }
      const base = String(baseURL || "").replace(/\/$/, "");
      stream = new EventSource(`${base}/live/events?page=media&topic=subforge`);
      stream.addEventListener("subforge", receive);
      stream.onerror = () => { healthy = false; setStatus(ui ? "Reconnecting… · showing last data" : "Reconnecting…", true); };
      ticker = setInterval(tick, 1000);
    }
    doc.addEventListener("visibilitychange", () => {
      if (doc.hidden) { close(); setStatus("Updates paused"); }
      else connect();
    });
    host?.addEventListener("pagehide", close);
    host?.addEventListener("pageshow", connect);
    connect();
    return { receive, tick, connect, close };
  }
  return { mount, duration };
});
