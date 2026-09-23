// Glance renders widget content on the server. Refresh the Media widgets from
// that same endpoint so service API keys never need to reach the browser.
(() => {
  if (typeof pageData === "undefined" || pageData.slug !== "media") return;

  const widgets = [
    "media-live-services",
    "media-live-jellyfin",
    "media-live-calendar",
    "media-live-automation",
    "media-live-seerr",
  ];
  const intervalMs = 15_000;
  let timer;
  let updating = false;

  async function update() {
    if (updating || document.hidden) return;
    updating = true;

    try {
      const url = `${pageData.baseURL}/api/pages/${pageData.slug}/content/`;
      const response = await fetch(url, { cache: "no-store" });
      if (!response.ok) throw new Error(`Glance returned ${response.status}`);

      const nextPage = new DOMParser().parseFromString(await response.text(), "text/html");
      for (const name of widgets) {
        const selector = `.widget.${name} .widget-content`;
        const current = document.querySelector(selector);
        const next = nextPage.querySelector(selector);
        if (current && next && current.innerHTML !== next.innerHTML) {
          current.innerHTML = next.innerHTML;
        }
      }
    } catch (error) {
      console.warn("Could not refresh Media widgets:", error);
    } finally {
      updating = false;
      schedule();
    }
  }

  function schedule() {
    clearTimeout(timer);
    if (!document.hidden) timer = setTimeout(update, intervalMs);
  }

  document.addEventListener("visibilitychange", () => {
    if (document.hidden) {
      clearTimeout(timer);
    } else {
      void update();
    }
  });

  schedule();
})();
