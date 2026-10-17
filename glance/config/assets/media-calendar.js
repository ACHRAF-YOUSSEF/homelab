// Calendar requests stay on the Glance gateway; provider credentials stay on the server.
((factory) => {
  const calendar = factory();
  if (typeof module === "object" && module.exports) {
    module.exports = calendar;
    return;
  }
  if (typeof pageData === "undefined" || pageData.slug !== "media") return;
  const mount = () => {
    const root = document.querySelector("[data-media-calendar]");
    if (!root || root.dataset.ready) return;
    root.dataset.ready = "true";
    calendar.mount(root, pageData.baseURL);
  };
  // Glance inserts page content asynchronously after its document scripts load.
  new MutationObserver(mount).observe(document.body, { childList: true, subtree: true });
  mount();
})(() => {
  const modes = ["day", "week", "month", "year"];
  const pad = value => String(value).padStart(2, "0");
  const dateKey = date => `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
  // Calendar arithmetic uses civil dates, rather than 24-hour intervals, across DST.
  const civil = (year, month, day) => new Date(year, month, day, 12);
  const addDays = (date, days) => civil(date.getFullYear(), date.getMonth(), date.getDate() + days);

  function rangeFor(anchor, mode) {
    let start = civil(anchor.getFullYear(), anchor.getMonth(), anchor.getDate());
    let end;
    if (mode === "year") {
      start = civil(anchor.getFullYear(), 0, 1);
      end = civil(anchor.getFullYear() + 1, 0, 1);
    } else if (mode === "month") {
      start = civil(anchor.getFullYear(), anchor.getMonth(), 1);
      end = civil(anchor.getFullYear(), anchor.getMonth() + 1, 1);
    } else {
      if (mode === "week") start = addDays(start, -((start.getDay() + 6) % 7));
      end = addDays(start, mode === "week" ? 7 : 1);
    }
    return { start, end };
  }

  function move(anchor, mode, direction) {
    const { start } = rangeFor(anchor, mode);
    if (mode === "year") return civil(start.getFullYear() + direction, 0, 1);
    if (mode === "month") return civil(start.getFullYear(), start.getMonth() + direction, 1);
    return addDays(start, direction * (mode === "week" ? 7 : 1));
  }

  function eventDay(event) {
    // Movies are all-day releases; their calendar date must not shift at UTC midnight.
    if (event.kind === "movie" && /^\d{4}-\d{2}-\d{2}(?:T|$)/.test(event.date)) return event.date.slice(0, 10);
    if (/^\d{4}-\d{2}-\d{2}$/.test(event.date)) return event.date;
    const date = new Date(event.date);
    return Number.isNaN(date.getTime()) ? "" : dateKey(date);
  }

  function visibleEvents(events, start, end) {
    const first = dateKey(start), last = dateKey(end);
    return events.filter(event => {
      const day = eventDay(event);
      return day >= first && day < last;
    }).sort((a, b) => String(a.date).localeCompare(String(b.date))
      || (a.season ?? 0) - (b.season ?? 0) || (a.episode ?? 0) - (b.episode ?? 0)
      || a.title.localeCompare(b.title));
  }

  function groupEvents(events) {
    const groups = new Map();
    for (const event of events) {
      const identity = event.kind === "episode" ? `series:${event.seriesId ?? event.slug ?? event.title}` : `movie:${event.id}`;
      const key = `${eventDay(event)}:${identity}`;
      if (!groups.has(key)) groups.set(key, { key, events: [] });
      groups.get(key).events.push(event);
    }
    return [...groups.values()];
  }

  function mount(root, baseURL) {
    const doc = root.ownerDocument;
    const node = (tag, className = "", text = "") => {
      const element = doc.createElement(tag);
      element.className = className;
      element.textContent = text;
      return element;
    };
    const today = () => new Date();
    let anchor = today(), mode = "week", events = [], loadedRange = "";
    let timer, controller, revision = 0, signature = "";
    const expanded = new Set();
    const toolbar = node("div", "media-calendar-toolbar");
    toolbar.setAttribute("role", "group");
    toolbar.setAttribute("aria-label", "Calendar controls");
    const navigation = node("div", "media-calendar-navigation");
    const button = (text, action) => {
      const element = node("button", "media-calendar-control", text);
      element.type = "button";
      element.addEventListener("click", action);
      return element;
    };
    const previous = button("", () => navigate(move(anchor, mode, -1)));
    const current = button("", () => navigate(today()));
    const next = button("", () => navigate(move(anchor, mode, 1)));
    navigation.append(previous, current, next);
    const views = node("div", "media-calendar-views");
    views.setAttribute("role", "group");
    views.setAttribute("aria-label", "Calendar period");
    const viewButtons = modes.map(value => {
      const control = button(value[0].toUpperCase() + value.slice(1), () => {
        if (mode === value) return;
        mode = value;
        navigate(anchor);
      });
      views.append(control);
      return control;
    });
    toolbar.append(navigation, views);
    const heading = node("h3", "media-calendar-heading color-highlight");
    const status = node("p", "media-calendar-status color-subdue");
    status.setAttribute("role", "status");
    const retry = button("Retry", () => void load());
    retry.hidden = true;
    const content = node("div", "media-calendar-content");
    root.replaceChildren(toolbar, heading, status, retry, content);

    function navigate(date) {
      if (date.getFullYear() < 1901 || date.getFullYear() > 9998) return;
      anchor = date;
      expanded.clear();
      signature = "";
      render();
      void load();
    }

    function eventLink(event, compact = false) {
      const link = node("a", `media-calendar-event ${event.kind === "episode" ? "color-primary" : "color-highlight"}`);
      link.dataset.event = `${event.kind}:${event.id}`;
      const number = event.kind === "episode" ? `S${pad(event.season)}E${pad(event.episode)}` : "";
      const title = compact ? `${number}${event.subtitle ? ` · ${event.subtitle}` : ""}` : `${event.kind === "episode" ? "TV" : "MOVIE"} ${event.title}`;
      link.append(node("span", "", title));
      if (!compact && number) link.append(node("span", "color-subdue", ` ${number}`));
      link.title = event.subtitle || event.title;
      // Slugs are path segments, never interpreted as URLs or markup.
      link.href = `${event.kind === "episode" ? root.dataset.seriesBase : root.dataset.movieBase}${encodeURIComponent(event.slug)}`;
      return link;
    }

    function appendEvents(container, items) {
      for (const group of groupEvents(items)) {
        if (group.events[0].kind !== "episode" || group.events.length <= 3) {
          container.append(...group.events.map(event => eventLink(event)));
          continue;
        }
        const details = node("details", "media-calendar-group");
        details.dataset.group = group.key;
        details.open = expanded.has(group.key);
        const summary = node("summary", "color-primary");
        summary.append(node("span", "", `TV ${group.events[0].title}`), node("span", "media-calendar-count color-subdue", `${group.events.length} episodes`));
        details.append(summary, ...group.events.map(event => eventLink(event, true)));
        details.addEventListener("toggle", () => {
          if (details.open) expanded.add(group.key);
          else expanded.delete(group.key);
        });
        container.append(details);
      }
    }

    const format = (date, options) => date.toLocaleDateString(undefined, options);
    function dayCell(date, items) {
      const cell = node("section", `media-calendar-day${dateKey(date) === dateKey(today()) ? " media-calendar-today" : ""}`);
      const label = node("h4", "size-h6 color-subdue", format(date, { weekday: "short", day: "numeric", month: "short" }));
      cell.append(label);
      if (dateKey(date) === dateKey(today())) label.setAttribute("aria-label", `${label.textContent}, today`);
      appendEvents(cell, items);
      if (!items.length) cell.append(node("p", "media-calendar-empty color-subdue", "—"));
      return cell;
    }

    function render() {
      const { start, end } = rangeFor(anchor, mode);
      const key = `${dateKey(start)}/${dateKey(end)}`;
      previous.textContent = `‹ Previous ${mode}`;
      next.textContent = `Next ${mode} ›`;
      previous.disabled = start.getFullYear() <= 1901;
      next.disabled = end.getFullYear() > 9998;
      current.textContent = mode === "day" ? "Today" : `This ${mode}`;
      viewButtons.forEach((control, index) => control.setAttribute("aria-pressed", String(modes[index] === mode)));
      heading.textContent = mode === "year" ? String(start.getFullYear())
        : mode === "month" ? format(start, { month: "long", year: "numeric" })
          : mode === "day" ? format(start, { weekday: "long", day: "numeric", month: "long", year: "numeric" })
            : `${format(start, { day: "numeric", month: "short", year: "numeric" })} – ${format(addDays(end, -1), { day: "numeric", month: "short", year: "numeric" })}`;
      if (loadedRange !== key) {
        content.replaceChildren();
        status.textContent = "Loading calendar…";
        return;
      }
      const visible = visibleEvents(events, start, end);
      const episodes = visible.filter(event => event.kind === "episode").length;
      const movies = visible.length - episodes;
      status.textContent = `${episodes} episode${episodes === 1 ? "" : "s"} · ${movies} movie${movies === 1 ? "" : "s"}${visible.length ? "" : " · No scheduled releases"}`;
      const nextSignature = JSON.stringify([mode, key, dateKey(today()), visible]);
      // Leave disclosure state, keyboard focus and scroll position untouched on unchanged refreshes.
      if (signature === nextSignature) return;
      signature = nextSignature;
      const focusedGroup = doc.activeElement?.closest("details[data-group]")?.dataset.group;
      const focusedEvent = content.contains(doc.activeElement) ? doc.activeElement.dataset.event : null;
      const byDate = new Map();
      for (const event of visible) {
        const day = eventDay(event);
        if (!byDate.has(day)) byDate.set(day, []);
        byDate.get(day).push(event);
      }
      const grid = node("div", `media-calendar media-calendar-view-${mode}`);
      if (mode === "year") {
        for (let month = 0; month < 12; month++) {
          const panel = node("section", "media-calendar-month-panel");
          panel.append(node("h4", "color-highlight", format(civil(start.getFullYear(), month, 1), { month: "long" })));
          let populated = false;
          for (let day = civil(start.getFullYear(), month, 1); day.getMonth() === month; day = addDays(day, 1)) {
            const items = byDate.get(dateKey(day));
            if (!items) continue;
            panel.append(dayCell(day, items));
            populated = true;
          }
          if (!populated) panel.append(node("p", "media-calendar-empty color-subdue", "No scheduled releases"));
          grid.append(panel);
        }
      } else {
        // Blank cells align month dates with a Monday-first week without fetching adjacent months.
        if (mode === "month") {
          for (let index = 0; index < (start.getDay() + 6) % 7; index++) {
            const blank = node("div", "media-calendar-blank");
            blank.setAttribute("aria-hidden", "true");
            grid.append(blank);
          }
        }
        for (let day = start; day < end; day = addDays(day, 1)) grid.append(dayCell(day, byDate.get(dateKey(day)) || []));
      }
      content.replaceChildren(grid);
      const focusTarget = focusedEvent
        ? [...content.querySelectorAll("a")].find(link => link.dataset.event === focusedEvent)
        : [...content.querySelectorAll("details")].find(details => details.dataset.group === focusedGroup)?.querySelector("summary");
      focusTarget?.focus({ preventScroll: true });
    }

    async function load() {
      clearTimeout(timer);
      controller?.abort();
      const ownRevision = ++revision;
      if (!root.isConnected || doc.hidden) return;
      const requestController = new AbortController();
      controller = requestController;
      const { start, end } = rangeFor(anchor, mode);
      const key = `${dateKey(start)}/${dateKey(end)}`;
      root.setAttribute("aria-busy", "true");
      retry.hidden = true;
      // Allow Glance authorization and both bounded provider requests to complete.
      const timeout = setTimeout(() => requestController.abort(), 50000);
      try {
        const url = `${baseURL}/live/calendar?start=${dateKey(start)}&end=${dateKey(end)}`;
        const response = await fetch(url, { cache: "no-store", signal: requestController.signal });
        if (!response.ok) throw new Error("Calendar unavailable");
        const data = await response.json();
        if (ownRevision !== revision) return;
        if (data.start !== dateKey(start) || data.end !== dateKey(end) || !Array.isArray(data.events)) throw new Error("Invalid calendar response");
        events = data.events;
        loadedRange = key;
        render();
      } catch {
        if (ownRevision !== revision) return;
        status.textContent = loadedRange === key
          ? "Calendar refresh failed. Showing the last successful update."
          : "Could not load the calendar. Check Sonarr, Radarr and the Glance gateway, or try a shorter period if this calendar is too large.";
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
      } else void load();
    });
    render();
    void load();
  }

  return { dateKey, addDays, rangeFor, move, eventDay, visibleEvents, groupEvents, mount };
});
