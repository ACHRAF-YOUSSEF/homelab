//! Resume EINTR only while receiving an existing response, never by replaying a request.
//! Trust boundary: delegates sockets, TLS/certificate verification and buffers to pinned
//! ureq 3.4.1's DefaultConnector. Its unversioned Transport API must be checked on upgrades.
use std::{
    io::ErrorKind,
    time::{Duration, Instant},
};
use ureq::{
    Error,
    unversioned::transport::{
        Buffers, ConnectionDetails, Connector, NextTimeout, Transport,
        time::Duration as TransportDuration,
    },
};

const MAX_INTERRUPTED_READS: usize = 1024;

#[derive(Debug)]
pub(super) struct ResumeReadsConnector;

impl<T: Transport> Connector<T> for ResumeReadsConnector {
    type Out = ResumeReads<T>;

    fn connect(
        &self,
        _: &ConnectionDetails,
        chained: Option<T>,
    ) -> Result<Option<Self::Out>, Error> {
        Ok(chained.map(ResumeReads))
    }
}

#[derive(Debug)]
pub(super) struct ResumeReads<T>(T);

impl<T: Transport> Transport for ResumeReads<T> {
    fn buffers(&mut self) -> &mut dyn Buffers {
        self.0.buffers()
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), Error> {
        // A failed write may already have sent mutation bytes. Never repeat it.
        self.0.transmit_output(amount, timeout)
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, Error> {
        let started = Instant::now();
        resume_input(&mut self.0, timeout, || started.elapsed())
    }

    fn is_open(&mut self) -> bool {
        self.0.is_open()
    }

    fn is_tls(&self) -> bool {
        self.0.is_tls()
    }
}

fn resume_input(
    inner: &mut impl Transport,
    timeout: NextTimeout,
    mut elapsed: impl FnMut() -> Duration,
) -> Result<bool, Error> {
    for attempt in 0..MAX_INTERRUPTED_READS {
        let after = match timeout.after {
            TransportDuration::Exact(budget) => {
                let remaining = budget.saturating_sub(elapsed());
                if remaining.is_zero() {
                    return Err(Error::Timeout(timeout.reason));
                }
                TransportDuration::Exact(remaining)
            }
            TransportDuration::NotHappening => TransportDuration::NotHappening,
        };
        match inner.await_input(NextTimeout { after, ..timeout }) {
            Err(Error::Io(error)) if error.kind() == ErrorKind::Interrupted => {
                // EINTR means this read transferred no bytes. Continue receiving on
                // the same connection, accounting for elapsed time and signal storms.
                if attempt + 1 == MAX_INTERRUPTED_READS {
                    return Err(Error::Io(error));
                }
                std::thread::yield_now();
            }
            result => return result,
        }
    }
    // The last interrupted attempt returns above; this loop cannot exhaust otherwise.
    Err(Error::Io(std::io::Error::from(ErrorKind::Interrupted)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use ureq::{Timeout, unversioned::transport::LazyBuffers};

    #[derive(Debug)]
    enum ReadStep {
        Interrupted,
        Payload,
        Failure(ErrorKind),
    }

    #[derive(Debug)]
    struct FixtureTransport {
        buffers: LazyBuffers,
        reads: VecDeque<ReadStep>,
        timeouts: Vec<NextTimeout>,
        writes: usize,
        write_interrupted: bool,
    }

    impl FixtureTransport {
        fn new(reads: impl IntoIterator<Item = ReadStep>) -> Self {
            Self {
                buffers: LazyBuffers::new(256, 256),
                reads: reads.into_iter().collect(),
                timeouts: Vec::new(),
                writes: 0,
                write_interrupted: false,
            }
        }
    }

    impl Transport for FixtureTransport {
        fn buffers(&mut self) -> &mut dyn Buffers {
            &mut self.buffers
        }

        fn transmit_output(&mut self, _: usize, _: NextTimeout) -> Result<(), Error> {
            self.writes += 1;
            if self.write_interrupted {
                Err(Error::Io(std::io::Error::from(ErrorKind::Interrupted)))
            } else {
                Ok(())
            }
        }

        fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, Error> {
            self.timeouts.push(timeout);
            match self
                .reads
                .pop_front()
                .expect("fixture read was not expected")
            {
                ReadStep::Interrupted => {
                    Err(Error::Io(std::io::Error::from(ErrorKind::Interrupted)))
                }
                ReadStep::Failure(kind) => Err(Error::Io(std::io::Error::from(kind))),
                ReadStep::Payload => {
                    let bytes = b"response";
                    self.buffers
                        .input_append_buf()
                        .get_mut(..bytes.len())
                        .unwrap()
                        .copy_from_slice(bytes);
                    self.buffers.input_appended(bytes.len());
                    Ok(true)
                }
            }
        }

        fn is_open(&mut self) -> bool {
            true
        }
        fn is_tls(&self) -> bool {
            true
        }
    }

    fn timeout(after: TransportDuration) -> NextTimeout {
        NextTimeout {
            after,
            reason: Timeout::RecvResponse,
        }
    }

    #[test]
    fn interrupted_receive_resumes_without_resending_and_preserves_tls_and_payload() {
        let mut transport = ResumeReads(FixtureTransport::new([
            ReadStep::Interrupted,
            ReadStep::Payload,
        ]));
        let limit = timeout(TransportDuration::Exact(Duration::from_secs(10)));
        transport.transmit_output(0, limit).unwrap();
        assert!(transport.await_input(limit).unwrap());
        assert_eq!(transport.0.writes, 1);
        assert_eq!(transport.0.timeouts.len(), 2);
        assert_eq!(transport.buffers().input(), b"response");
        assert!(transport.is_open());
        assert!(transport.is_tls());
    }

    #[test]
    fn repeated_interruptions_consume_the_original_budget_and_end_as_timeout() {
        let mut inner =
            FixtureTransport::new(std::iter::repeat_with(|| ReadStep::Interrupted).take(3));
        let limit = timeout(TransportDuration::Exact(Duration::from_secs(10)));
        inner.transmit_output(0, limit).unwrap();
        let mut elapsed = [0, 4, 9, 10].into_iter();
        let error = resume_input(&mut inner, limit, || {
            Duration::from_secs(elapsed.next().unwrap())
        })
        .unwrap_err();
        assert!(matches!(error, Error::Timeout(Timeout::RecvResponse)));
        assert_eq!(
            inner
                .timeouts
                .iter()
                .map(|timeout| timeout.after)
                .collect::<Vec<_>>(),
            [10, 6, 1].map(|seconds| TransportDuration::Exact(Duration::from_secs(seconds)))
        );
        assert!(
            inner
                .timeouts
                .iter()
                .all(|timeout| timeout.reason == limit.reason)
        );
        assert_eq!(inner.writes, 1);
    }

    #[test]
    fn absent_timeout_is_distinct_from_an_expired_budget_and_interruptions_are_bounded() {
        let mut inner = FixtureTransport::new([ReadStep::Interrupted, ReadStep::Payload]);
        assert!(
            resume_input(&mut inner, timeout(TransportDuration::NotHappening), || {
                Duration::MAX
            })
            .unwrap()
        );
        assert!(
            inner
                .timeouts
                .iter()
                .all(|timeout| timeout.after == TransportDuration::NotHappening)
        );
        let mut inner = FixtureTransport::new([]);
        assert!(matches!(
            resume_input(
                &mut inner,
                timeout(TransportDuration::Exact(Duration::ZERO)),
                || Duration::ZERO
            ),
            Err(Error::Timeout(Timeout::RecvResponse))
        ));
        assert!(inner.timeouts.is_empty());
        let mut inner = FixtureTransport::new(
            std::iter::repeat_with(|| ReadStep::Interrupted).take(MAX_INTERRUPTED_READS),
        );
        assert!(
            matches!(resume_input(&mut inner, timeout(TransportDuration::NotHappening), || Duration::ZERO), Err(Error::Io(error)) if error.kind() == ErrorKind::Interrupted)
        );
        assert_eq!(inner.timeouts.len(), MAX_INTERRUPTED_READS);
    }

    #[test]
    fn other_receive_failures_and_interrupted_writes_are_never_retried_or_relabeled() {
        for kind in [ErrorKind::UnexpectedEof, ErrorKind::ConnectionReset] {
            let mut transport = ResumeReads(FixtureTransport::new([ReadStep::Failure(kind)]));
            let limit = timeout(TransportDuration::Exact(Duration::from_secs(10)));
            assert!(
                matches!(transport.await_input(limit), Err(Error::Io(error)) if error.kind() == kind)
            );
            assert_eq!(transport.0.timeouts.len(), 1);
        }
        let mut transport = ResumeReads(FixtureTransport::new([]));
        transport.0.write_interrupted = true;
        assert!(
            matches!(transport.transmit_output(0, timeout(TransportDuration::NotHappening)), Err(Error::Io(error)) if error.kind() == ErrorKind::Interrupted)
        );
        assert_eq!(transport.0.writes, 1);
        assert!(transport.0.timeouts.is_empty());
    }
}
