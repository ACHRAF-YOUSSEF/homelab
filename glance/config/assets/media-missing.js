// Read-only inventory from the same-origin gateway; provider keys stay server-side.
((factory) => {
  const missing = factory();
  if (typeof module === "object" && module.exports) {
    module.exports = missing;
    return;
  }
  if (typeof pageData === "undefined" || pageData.slug !== "media") return;
  const mount = () => {
    const root = document.querySelector("[data-media-missing]");
    if (!root || root.dataset.ready) return;
    root.dataset.ready = "true";
    missing.mount(root, pageData.baseURL);
  };
  new MutationObserver(mount).observe(document.body, { childList: true, subtree: true });
  mount();
})(() => {
  const count = (number, singular, plural = `${singular}s`) => `${number} ${number === 1 ? singular : plural}`;
  const pad = value => String(value).padStart(2, "0");

  function groupItems(items) {
    const shows = new Map(), movies = [];
    for (const item of items) {
      if (item.kind === "movie") {
        movies.push(item);
        continue;
      }
      const key = `series:${item.seriesId}`;
      if (!shows.has(key)) shows.set(key, { key, title: item.title, episodes: [], seasons: new Map() });
      const show = shows.get(key);
      show.episodes.push(item);
      if (!show.seasons.has(item.season)) show.seasons.set(item.season, []);
      show.seasons.get(item.season).push(item);
    }
    for (const show of shows.values()) {
      show.seasons = [...show.seasons].sort(([a], [b]) => a - b).map(([number, episodes]) => ({
        number, key: `${show.key}:season:${number}`, episodes: episodes.sort((a, b) => a.episode - b.episode || a.id - b.id),
      }));
    }
    return {
      shows: [...shows.values()].sort((a, b) => a.title.localeCompare(b.title) || a.key.localeCompare(b.key)),
      movies: movies.sort((a, b) => a.title.localeCompare(b.title) || (a.year ?? 0) - (b.year ?? 0) || a.id - b.id),
    };
  }

  function mount(root, baseURL) {
    const doc = root.ownerDocument;
    const node = (tag, className = "", text = "") => {
      const element = doc.createElement(tag);
      element.className = className;
      element.textContent = text;
      return element;
    };
    const status = node("p", "media-missing-status color-subdue", "Loading missing media…");
    status.setAttribute("role", "status");
    const retry = node("button", "media-missing-retry", "Retry");
    retry.type = "button";
    retry.hidden = true;
    retry.addEventListener("click", () => void load());
    const content = node("div", "media-missing-columns");
    const note = node("p", "media-missing-note color-subdue", "Monitored items only. Episodes appear after airing; movies include unreleased titles. Based on the latest Sonarr and Radarr scans.");
    root.replaceChildren(status, retry, content, note);
    let timer, controller, revision = 0, signature = "", loaded = false;

    function render(data) {
      const { shows, movies } = groupItems(data.items);
      const episodes = shows.reduce((sum, show) => sum + show.episodes.length, 0);
      status.textContent = `${count(episodes, "episode")} in ${count(shows.length, "show")} · ${count(movies.length, "movie")}`;
      const nextSignature = JSON.stringify(data);
      if (signature === nextSignature) return;
      signature = nextSignature;
      const expanded = new Set([...content.querySelectorAll("details")].filter(detail => detail.open).map(detail => detail.dataset.group));
      const scrollPositions = [...content.querySelectorAll(".media-missing-list")].map(list => list.scrollTop);
      const focus = content.contains(doc.activeElement) ? doc.activeElement.dataset.focus : null;
      const disclosure = (key, title, total) => {
        const detail = node("details", "media-missing-group");
        detail.dataset.group = key;
        detail.open = expanded.has(key);
        const summary = node("summary");
        summary.dataset.focus = key;
        summary.append(node("span", "color-highlight", title), node("span", "media-missing-count color-subdue", count(total, "episode")));
        detail.append(summary);
        return detail;
      };
      const link = (item, single = false) => {
        const anchor = node("a", "media-missing-item");
        anchor.dataset.focus = `${item.kind}:${item.id}`;
        anchor.href = `${item.kind === "episode" ? root.dataset.seriesBase : root.dataset.movieBase}${encodeURIComponent(item.slug)}`;
        const number = item.kind === "episode" ? `S${pad(item.season)}E${pad(item.episode)}` : "";
        anchor.append(node("span", "color-primary", item.kind === "movie" || single ? item.title : number));
        const subtitle = item.kind === "movie" ? (item.year ? String(item.year) : "")
          : `${single ? `${number} · ` : ""}${item.subtitle || "Untitled episode"}`;
        if (subtitle) anchor.append(node("span", "media-missing-subtitle color-subdue", subtitle));
        return anchor;
      };
      const section = (title, configured, empty) => {
        const panel = node("section", "media-missing-panel");
        panel.append(node("h3", "media-missing-heading color-highlight", title));
        const list = node("div", "media-missing-list");
        panel.append(list);
        if (!configured) list.append(node("p", "color-subdue", `${empty} API key is not configured.`));
        return { panel, list };
      };
      const tv = section("TV shows", data.providers.sonarr, "Sonarr");
      for (const show of shows) {
        if (show.episodes.length === 1) {
          tv.list.append(link(show.episodes[0], true));
          continue;
        }
        const detail = disclosure(show.key, show.title, show.episodes.length);
        for (const season of show.seasons) {
          const seasonDetail = disclosure(season.key, season.number === 0 ? "Specials" : `Season ${season.number}`, season.episodes.length);
          const entries = node("div", "media-missing-episodes");
          entries.append(...season.episodes.map(item => link(item)));
          seasonDetail.append(entries);
          detail.append(seasonDetail);
        }
        tv.list.append(detail);
      }
      if (!shows.length && data.providers.sonarr) tv.list.append(node("p", "color-subdue", "No missing aired episodes."));
      const film = section("Movies", data.providers.radarr, "Radarr");
      film.list.append(...movies.map(item => link(item)));
      if (!movies.length && data.providers.radarr) film.list.append(node("p", "color-subdue", "No missing movies."));
      content.replaceChildren(tv.panel, film.panel);
      [tv.list, film.list].forEach((list, index) => { list.scrollTop = scrollPositions[index] || 0; });
      if (focus) [...content.querySelectorAll("[data-focus]")].find(element => element.dataset.focus === focus)?.focus({ preventScroll: true });
    }

    async function load() {
      clearTimeout(timer);
      controller?.abort();
      const ownRevision = ++revision;
      if (!root.isConnected || doc.hidden) return;
      controller = new AbortController();
      const requestController = controller;
      const timeout = setTimeout(() => requestController.abort(), 65000);
      root.setAttribute("aria-busy", "true");
      retry.hidden = true;
      try {
        const response = await fetch(`${baseURL}/live/missing`, { cache: "no-store", signal: requestController.signal });
        if (!response.ok) throw new Error("Missing media unavailable");
        const data = await response.json();
        if (ownRevision !== revision) return;
        if (!Array.isArray(data.items) || typeof data.providers?.sonarr !== "boolean" || typeof data.providers?.radarr !== "boolean") throw new Error("Invalid inventory");
        render(data);
        loaded = true;
      } catch {
        if (ownRevision !== revision) return;
        status.textContent = loaded ? "Refresh failed. Showing the last successful update."
          : "Could not load missing media. Check Sonarr, Radarr, API keys and gateway response limits, then retry.";
        retry.hidden = false;
      } finally {
        clearTimeout(timeout);
        if (ownRevision === revision) {
          root.setAttribute("aria-busy", "false");
          if (root.isConnected && !doc.hidden) timer = setTimeout(load, 30000);
        }
      }
    }
    doc.addEventListener("visibilitychange", () => {
      if (doc.hidden) {
        clearTimeout(timer);
        controller?.abort();
        revision++;
        root.setAttribute("aria-busy", "false");
      } else void load();
    });
    void load();
  }
  return { groupItems, mount };
});
