// Native dashboard SSE. No periodic HTTP snapshots or credentials in stream URLs.
((root, factory) => {
  const client = factory();
  if (typeof module === 'object' && module.exports) module.exports = client;
  else root.SubforgeLive = client;
})(typeof window === 'undefined' ? globalThis : window, () => {
  const object = value => value !== null && typeof value === 'object' && !Array.isArray(value);
  const objects = value => Array.isArray(value) && value.every(object);
  function valid(type, value) {
    if (!object(value)) return false;
    if (type === 'status') return ['history', 'queue', 'logs', 'roots'].every(key => objects(value[key]))
      && (!Object.hasOwn(value,'active_jobs') || objects(value.active_jobs))
      && (!Object.hasOwn(value,'running') || value.running === null || object(value.running))
      && (Object.hasOwn(value,'active_jobs') || Object.hasOwn(value,'running'))
      && (!Object.hasOwn(value,'tasks') || object(value.tasks) && objects(value.tasks.tasks));
    if (type === 'system') return object(value.resource_usage) && typeof value.uptime_seconds === 'number' && Number.isFinite(value.uptime_seconds);
    return object(value.allocation) && objects(value.workers);
  }
  function create(handlers, environment) {
    const env = environment || { document, window, EventSource: typeof EventSource === 'undefined' ? null : EventSource,
      setTimeout: window.setTimeout.bind(window), clearTimeout: window.clearTimeout.bind(window),
      setInterval: window.setInterval.bind(window), clearInterval: window.clearInterval.bind(window),
      now: () => performance.now() };
    let stream, retry, watchdog, wanted = false, suspended = false, lastStatus = 0, delay = 1000;
    const state = value => handlers.connection?.(value);
    function close() {
      const previous = stream;
      stream = undefined;
      previous?.close();
      env.clearTimeout(retry);
      env.clearInterval(watchdog);
    }
    function reconnect() {
      close();
      state('reconnecting');
      if (!wanted || suspended || env.document.hidden) return;
      retry = env.setTimeout(connect, delay);
      delay = Math.min(30000, delay * 2);
    }
    function connect() {
      if (!wanted || suspended || env.document.hidden || stream) return;
      if (!env.EventSource) { state('unsupported'); return; }
      state('connecting');
      let current;
      try { current = new env.EventSource('/api/events'); }
      catch { reconnect(); return; }
      stream = current;
      lastStatus = env.now();
      for (const type of ['status', 'system', 'workers']) current.addEventListener(type, event => {
        if (stream !== current || env.document.hidden || suspended) return;
        try {
          const maximum = type === 'status' ? 4 * 1024 * 1024 : 256 * 1024;
          if (typeof event.data !== 'string' || event.data.length > maximum) throw new Error();
          const payload = JSON.parse(event.data);
          if (!valid(type, payload)) throw new Error();
          handlers[type]?.(payload);
          if (type === 'status') { lastStatus = env.now(); delay = 1000; state('connected'); }
        } catch {
          // Retain last known data; never log the response or private diagnostics.
          state('invalid');
        }
      });
      current.onerror = () => {
        if (stream !== current) return;
        state('reconnecting');
        // EventSource handles ordinary reconnects. HTTP rejection may leave it permanently closed.
        if (current.readyState === 2) reconnect();
      };
      watchdog = env.setInterval(() => {
        if (wanted && !env.document.hidden && env.now() - lastStatus > 40000) reconnect();
      }, 5000);
    }
    env.document.addEventListener('visibilitychange', () => {
      if (env.document.hidden) { close(); if (wanted) state('paused'); }
      else connect();
    });
    env.window.addEventListener('pagehide', () => { suspended = true; close(); if (wanted) state('paused'); });
    env.window.addEventListener('pageshow', () => { suspended = false; connect(); });
    return {
      start() { wanted = true; connect(); },
      stop() { wanted = false; close(); },
    };
  }
  return { create };
});
