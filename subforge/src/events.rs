//! Native bounded SSE transport and mutation signals. No HTTP/self polling.
use serde_json::Value;
use std::{
    fmt,
    io::{self, Write},
    net::TcpStream,
    ops::{Deref, DerefMut},
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

pub(crate) const STREAM_LIMIT: usize = 4;
const COALESCE: Duration = Duration::from_millis(250);
const HEARTBEAT: Duration = Duration::from_secs(15);
const METRICS: Duration = Duration::from_secs(3);
const LIFETIME: Duration = Duration::from_secs(900);

pub(crate) trait Lockable {
    type Value;
    type Guard<'a>: Deref<Target = Self::Value> + DerefMut
    where
        Self: 'a;
    fn acquire(&self) -> Self::Guard<'_>;
}
impl<T> Lockable for Mutex<T> {
    type Value = T;
    type Guard<'a>
        = MutexGuard<'a, T>
    where
        T: 'a;
    fn acquire(&self) -> Self::Guard<'_> {
        self.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
impl<L: Lockable> Lockable for Arc<L> {
    type Value = L::Value;
    type Guard<'a>
        = L::Guard<'a>
    where
        L: 'a;
    fn acquire(&self) -> Self::Guard<'_> {
        self.as_ref().acquire()
    }
}

/// Mutable accesses notify after unlocking. Pure snapshot reads stay silent.
pub(crate) struct Observed<T> {
    pub(crate) value: Mutex<T>,
    events: Arc<Events>,
}
impl<T> Observed<T> {
    pub(crate) fn new(value: T, events: Arc<Events>) -> Self {
        Self {
            value: Mutex::new(value),
            events,
        }
    }
}
pub(crate) struct ObservedGuard<'a, T> {
    guard: Option<MutexGuard<'a, T>>,
    events: &'a Events,
    dirty: bool,
}
impl<T> Deref for ObservedGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.guard.as_deref().expect("live observed guard")
    }
}
impl<T> DerefMut for ObservedGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.dirty = true;
        self.guard.as_deref_mut().expect("live observed guard")
    }
}
impl<T> Drop for ObservedGuard<'_, T> {
    fn drop(&mut self) {
        drop(self.guard.take());
        if self.dirty {
            self.events.notify();
        }
    }
}
impl<T> Lockable for Observed<T> {
    type Value = T;
    type Guard<'a>
        = ObservedGuard<'a, T>
    where
        T: 'a;
    fn acquire(&self) -> Self::Guard<'_> {
        ObservedGuard {
            guard: Some(self.value.acquire()),
            events: &self.events,
            dirty: false,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Kind {
    Widget,
    Status,
    System,
    Workers,
}
impl Kind {
    pub(crate) fn route(path: &str) -> Option<Self> {
        match path {
            "/api/widget/events" => Some(Self::Widget),
            "/api/events" => Some(Self::Status),
            _ => None,
        }
    }
    fn index(self) -> usize {
        match self {
            Self::Widget => 0,
            Self::Status => 1,
            Self::System => 2,
            Self::Workers => 3,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Widget => "widget",
            Self::Status => "status",
            Self::System => "system",
            Self::Workers => "workers",
        }
    }
    fn limit(self) -> usize {
        match self {
            Self::Widget => 128 * 1024,
            Self::Status => 4 * 1024 * 1024,
            _ => 256 * 1024,
        }
    }
}
struct Cached {
    revision: u64,
    built: Instant,
    frame: Arc<[u8]>,
}
pub(crate) struct Events {
    revision: Mutex<u64>,
    changed: Condvar,
    slots: AtomicUsize,
    cache: [Mutex<Option<Cached>>; 4],
}
impl Default for Events {
    fn default() -> Self {
        Self {
            revision: Mutex::new(0),
            changed: Condvar::new(),
            slots: AtomicUsize::new(0),
            cache: std::array::from_fn(|_| Mutex::new(None)),
        }
    }
}
impl Events {
    pub(crate) fn notify(&self) {
        let mut revision = self.revision.acquire();
        *revision = revision.wrapping_add(1);
        self.changed.notify_all();
    }
    pub(crate) fn revision(&self) -> u64 {
        *self.revision.acquire()
    }
    pub(crate) fn permit(self: &Arc<Self>) -> Option<Permit> {
        loop {
            let slots = self.slots.load(Ordering::Acquire);
            if slots >= STREAM_LIMIT {
                return None;
            }
            if self
                .slots
                .compare_exchange_weak(slots, slots + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Some(Permit(self.clone()));
            }
        }
    }
    fn wait(&self, revision: u64, duration: Duration) {
        let guard = self.revision.acquire();
        if *guard == revision {
            drop(
                self.changed
                    .wait_timeout(guard, duration)
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            );
        }
    }
    fn frame(
        &self,
        kind: Kind,
        revision: u64,
        age: Duration,
        snapshot: impl FnOnce() -> Value,
    ) -> io::Result<Arc<[u8]>> {
        let mut cache = self
            .cache
            .get(kind.index())
            .expect("fixed event kind")
            .acquire();
        if let Some(cached) = cache
            .as_ref()
            .filter(|cached| cached.revision == revision && cached.built.elapsed() < age)
        {
            return Ok(cached.frame.clone());
        }
        let frame: Arc<[u8]> = frame(kind, snapshot())?.into();
        *cache = Some(Cached {
            revision,
            built: Instant::now(),
            frame: frame.clone(),
        });
        Ok(frame)
    }
}
pub(crate) struct Permit(Arc<Events>);
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.slots.fetch_sub(1, Ordering::AcqRel);
        self.0.changed.notify_all();
    }
}

fn frame(kind: Kind, value: Value) -> io::Result<Vec<u8>> {
    struct Buffer {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
                return Err(io::Error::other("SSE snapshot limit exceeded"));
            }
            let needed = self.bytes.len() + bytes.len();
            if needed > self.bytes.capacity() {
                self.bytes.reserve_exact(
                    needed
                        .max(self.bytes.capacity().saturating_mul(2))
                        .min(self.limit)
                        - self.bytes.len(),
                );
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut buffer = Buffer {
        bytes: Vec::with_capacity(4096),
        limit: kind.limit(),
    };
    write!(buffer, "event: {}\ndata: ", kind.name())?;
    serde_json::to_writer(&mut buffer, &value).map_err(io::Error::other)?;
    buffer.write_all(b"\n\n")?;
    Ok(buffer.bytes)
}

fn send(socket: &mut TcpStream, mut bytes: &[u8]) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !bytes.is_empty() {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "SSE client write timed out"))?;
        socket.set_write_timeout(Some(remaining.max(Duration::from_millis(1))))?;
        let written = socket.write(bytes.get(..bytes.len().min(16 * 1024)).unwrap_or(bytes))?;
        if written == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        bytes = bytes
            .get(written..)
            .ok_or_else(|| io::Error::other("invalid socket write"))?;
    }
    socket.flush()
}

fn disconnected(socket: &TcpStream) -> bool {
    if socket.set_nonblocking(true).is_err() {
        return true;
    }
    let result = socket.peek(&mut [0u8; 1]);
    let reset = socket.set_nonblocking(false);
    // Extra request bytes are unsupported, as for regular HTTP requests.
    reset.is_err() || !matches!(result, Err(error) if error.kind() == io::ErrorKind::WouldBlock)
}

pub(crate) fn stream(
    socket: &mut TcpStream,
    events: &Events,
    shutdown: &AtomicBool,
    kind: Kind,
    snapshot: impl Fn(Kind) -> Value,
) -> io::Result<()> {
    send(socket, b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nCache-Control: no-store, no-transform\r\nConnection: close\r\nX-Accel-Buffering: no\r\nX-Content-Type-Options: nosniff\r\n\r\n")?;
    let deadline = Instant::now() + LIFETIME;
    let mut revision = events.revision();
    let mut last_status = Instant::now();
    let mut last_metrics = Instant::now();
    send(
        socket,
        &events.frame(kind, revision, HEARTBEAT, || snapshot(kind))?,
    )?;
    let dashboard = matches!(kind, Kind::Status);
    if dashboard {
        for metric in [Kind::System, Kind::Workers] {
            send(
                socket,
                &events.frame(metric, 0, METRICS, || snapshot(metric))?,
            )?;
        }
    }
    while !shutdown.load(Ordering::Relaxed) && Instant::now() < deadline {
        let seen = events.revision();
        let status_due = seen != revision && last_status.elapsed() >= COALESCE
            || last_status.elapsed() >= HEARTBEAT;
        if status_due {
            send(
                socket,
                &events.frame(kind, seen, HEARTBEAT, || snapshot(kind))?,
            )?;
            revision = seen;
            last_status = Instant::now();
        }
        if dashboard && last_metrics.elapsed() >= METRICS {
            for metric in [Kind::System, Kind::Workers] {
                send(
                    socket,
                    &events.frame(metric, 0, METRICS, || snapshot(metric))?,
                )?;
            }
            last_metrics = Instant::now();
        }
        if disconnected(socket) {
            break;
        }
        let wait = if events.revision() != revision {
            COALESCE.saturating_sub(last_status.elapsed())
        } else {
            Duration::from_secs(1)
        };
        events.wait(events.revision(), wait.min(Duration::from_secs(1)));
    }
    Ok(())
}

impl fmt::Debug for Events {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Events { bounded: true }")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_are_silent_mutations_notify_and_stream_permits_are_bounded() {
        let events = Arc::new(Events::default());
        let value = Observed::new(1, events.clone());
        assert_eq!(*value.acquire(), 1);
        assert_eq!(events.revision(), 0);
        *value.acquire() = 2;
        assert_eq!(events.revision(), 1);
        let permits: Vec<_> = (0..STREAM_LIMIT)
            .map(|_| events.permit().unwrap())
            .collect();
        assert!(events.permit().is_none());
        drop(permits);
        assert!(events.permit().is_some());
    }

    #[test]
    fn frames_have_named_direct_payloads_and_bounded_serialization() {
        assert_eq!(
            frame(Kind::Widget, json!({"count":1})).unwrap(),
            b"event: widget\ndata: {\"count\":1}\n\n"
        );
        assert!(frame(Kind::Widget, json!({"value":"x".repeat(128 * 1024)})).is_err());
    }

    #[test]
    fn shared_cache_reuses_snapshots_until_a_mutation_or_heartbeat() {
        let events = Events::default();
        let calls = AtomicUsize::new(0);
        let snapshot = || {
            calls.fetch_add(1, Ordering::Relaxed);
            json!({"count":1})
        };
        let first = events.frame(Kind::Widget, 0, HEARTBEAT, snapshot).unwrap();
        let second = events.frame(Kind::Widget, 0, HEARTBEAT, snapshot).unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        events.notify();
        events
            .frame(Kind::Widget, events.revision(), HEARTBEAT, snapshot)
            .unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        events
            .frame(Kind::Widget, events.revision(), Duration::ZERO, snapshot)
            .unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn stalled_reader_has_a_bounded_total_write_deadline() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let reader = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut writer, _) = listener.accept().unwrap();
        let bytes = vec![b'x'; 4 * 1024 * 1024];
        let started = Instant::now();
        assert!(send(&mut writer, &bytes).is_err());
        assert!(started.elapsed() < Duration::from_secs(4));
        drop(reader);
    }
}
