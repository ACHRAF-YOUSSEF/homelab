// Refresh server-rendered widgets without sending provider credentials to browsers.
(function (root, factory) {
  const client = factory();
  if (typeof module === "object" && module.exports) module.exports = client;
  else if (typeof pageData !== "undefined" && pageData.slug === "media") client.mount(root.document, pageData, root);
})(typeof window !== "undefined" ? window : globalThis, function () {
  const names = ["media-live-services", "media-live-jellyfin-library", "media-live-automation", "media-live-seerr"];

  function mount(doc, page, host = doc.defaultView) {
    const entries = new Map();
    let timer;
    let controller;
    let updating = false;
    let stopped = false;
    let revision = 0;
    const unavailable = node => Boolean(node.querySelector('[data-media-provider-state="unavailable"], .widget-error-header'));

    function show(entry, message, stale = false, retry = stale) {
      if (entry.message.textContent !== message) entry.message.textContent = message;
      entry.footer.classList.toggle("color-negative", stale);
      entry.retry.hidden = !retry;
      entry.retry.disabled = updating;
      entry.time.textContent = entry.lastSuccess ? `Last checked ${entry.lastSuccess.toLocaleTimeString()}` : "Not checked yet";
      entry.time.title = "Time this browser successfully checked Glance's rendered widget. Provider data can be cached; this is not the provider's measurement time.";
    }

    function attach() {
      for (const name of names) {
        const content = doc.querySelector(`.widget.${name} .widget-content`);
        if (!content || entries.get(name)?.content === content) continue;
        const footer = doc.createElement("div");
        footer.className = "media-refresh-status color-subdue size-h6";
        const message = doc.createElement("span");
        message.setAttribute("role", "status");
        const time = doc.createElement("span");
        const retry = doc.createElement("button");
        retry.type = "button";
        retry.className = "media-refresh-retry color-primary";
        retry.textContent = "Retry";
        retry.setAttribute("aria-label", `Retry ${name.replace("media-live-", "").replaceAll("-", " ")} refresh`);
        retry.addEventListener("click", () => { void update(); });
        const entry = { content, footer, message, time, retry, html: content.innerHTML, hasGood: !unavailable(content), lastSuccess: null };
        footer.append(message, time, retry);
        content.append(footer);
        entries.set(name, entry);
        show(entry, entry.hasGood ? "Checking updates…" : "Service data unavailable", !entry.hasGood);
      }
    }

    function schedule() {
      host.clearTimeout(timer);
      if (!doc.hidden && !stopped) timer = host.setTimeout(() => { void update(); }, 15000);
    }

    async function update() {
      if (updating || doc.hidden || stopped) return;
      host.clearTimeout(timer);
      attach();
      updating = true;
      const ownRevision = ++revision;
      const request = new host.AbortController();
      controller = request;
      const timeout = host.setTimeout(() => request.abort(), 20000);
      for (const entry of entries.values()) entry.retry.disabled = true;
      try {
        const base = String(page.baseURL || "").replace(/\/$/, "");
        const response = await host.fetch(`${base}/api/pages/media/content/`, { cache: "no-store", signal: request.signal });
        if (!response.ok) throw new Error("Refresh unavailable");
        const nextPage = new host.DOMParser().parseFromString(await response.text(), "text/html");
        if (ownRevision !== revision) return;
        attach();
        for (const [name, entry] of entries) {
          const next = nextPage.querySelector(`.widget.${name} .widget-content`);
          if (!next || !next.innerHTML.trim() || unavailable(next)) {
            show(entry, entry.hasGood ? "Updates unavailable · showing last data" : "Service data unavailable", true);
            continue;
          }
          if (entry.html !== next.innerHTML) {
            entry.html = next.innerHTML;
            entry.content.innerHTML = next.innerHTML;
            entry.content.append(entry.footer);
          }
          entry.hasGood = true;
          entry.lastSuccess = new Date();
          show(entry, "Up to date");
        }
      } catch {
        if (ownRevision !== revision) return;
        for (const entry of entries.values()) show(entry, entry.hasGood ? "Refresh failed · showing last data" : "Refresh failed · no service data", true);
        // Do not log upstream HTML, private links or provider responses.
      } finally {
        host.clearTimeout(timeout);
        if (ownRevision === revision) {
          updating = false;
          controller = undefined;
          for (const entry of entries.values()) entry.retry.disabled = false;
          schedule();
        }
      }
    }

    function pause() {
      revision++;
      controller?.abort();
      controller = undefined;
      updating = false;
      host.clearTimeout(timer);
      for (const entry of entries.values()) show(entry, "Updates paused", true, false);
    }
    doc.addEventListener("visibilitychange", () => {
      if (doc.hidden) pause();
      else { stopped = false; void update(); }
    });
    host.addEventListener("pagehide", () => { stopped = true; pause(); });
    host.addEventListener("pageshow", () => { stopped = false; void update(); });
    const observer = new host.MutationObserver(() => {
      attach();
      if (entries.size === names.length) observer.disconnect();
    });
    observer.observe(doc.body, { childList: true, subtree: true });
    attach();
    if (entries.size === names.length) observer.disconnect();
    void update();
    return { update };
  }
  return { mount };
});
