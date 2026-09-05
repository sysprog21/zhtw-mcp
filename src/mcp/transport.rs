// JSON-RPC framing for the RMCP stdio server.
//
// RMCP's own stdio transport reads unbounded lines, drops input its parser
// rejects without a reply, and treats anything before initialize as a fatal
// handshake error that ends the process. This transport keeps the framing
// contract this server has always had:
//
//   - one line is bounded to MAX_LINE_BYTES, oversize lines are drained and
//     answered -32600 rather than buffered,
//   - malformed UTF-8 and malformed JSON are answered -32700,
//   - valid JSON that is not a valid JSON-RPC request is answered -32600 with
//     the id echoed, so the client can correlate it,
//   - response-shaped messages are discarded in silence, per JSON-RPC 2.0,
//   - a request before initialize is answered -32002 and the connection
//     stays up.
//
// Everything past the envelope is RMCP's job.

use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, PoisonError};
use std::time::Duration;

// Logging is deprecated by SEP-2577 but stays in the spec for now, and this
// server's clients use it; the level is typed rather than stringly so the
// filter, the wire, and logging/setLevel cannot disagree.
#[allow(deprecated)]
use rmcp::model::LoggingLevel;
use rmcp::model::RequestId as PeerRequestId;
use rmcp::service::{RxJsonRpcMessage, TxJsonRpcMessage};
use rmcp::transport::Transport;
use rmcp::RoleServer;
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, Stdin,
};
use tokio::sync::{mpsc, oneshot, Notify};

use super::types::{
    parse_jsonrpc_line, JsonRpcResponse, RequestId, TransportError, INVALID_REQUEST,
    SERVER_NOT_INITIALIZED,
};

/// Maximum line length accepted from stdin (4 MiB payload).
/// Prevents memory exhaustion from a stream that never sends a newline.
const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;

/// State shared between the framing layer, the request handler, and the
/// writer task.
///
/// The framing layer has to answer some messages before RMCP would see them
/// (a pre-init request, a call after `shutdown`), and the handler is what
/// learns that `initialize` and `shutdown` happened. One small shared cell
/// beats routing either concern through the other. The writer task is here
/// for the same reason: it is spawned before the transport exists, so the
/// queue it reads and the accounting it settles have to live somewhere both
/// can reach.
#[derive(Default)]
pub struct Lifecycle {
    initialized: AtomicBool,
    shutdown: AtomicBool,
    /// Receiver for tracing output bound for the client, present once the
    /// client has asked for log notifications.
    logs: std::sync::Mutex<Option<std::sync::mpsc::Receiver<crate::trace::McpLogMessage>>>,
    /// Lowest severity the client asked to receive, as a rank. Everything
    /// below it is dropped rather than sent, which is what `logging/setLevel`
    /// asks for; a client that wants errors should not be reading info.
    log_level: AtomicU8,
    /// The outbound queue, for waiting on it before the process goes.
    ///
    /// Held here rather than on the transport because the termination reads it
    /// from a spawned task, which outlives the read future it was spawned
    /// from. See the `Gate::Exit` arm in `receive`.
    outbound: std::sync::Mutex<Option<mpsc::UnboundedSender<Outbound>>>,
    /// Set once a write to stdout has failed, so nothing waits on a reply
    /// that can no longer be sent.
    ///
    /// Per-frame `io::Result`s do not end the session on their own. RMCP
    /// routes every response and error through `Event::ToSink`, which spawns
    /// the send and reduces its result to a log line, so a caller returning
    /// an error there changes nothing. Without this, a dead stdout leaves the
    /// server reading stdin and accepting requests it can no longer answer.
    /// `receive` returning `None` is the one signal RMCP always acts on.
    ///
    /// Paired with `write_failed_signal` because the two readers need
    /// different things: `drain_in_flight` polls this between attempts, while
    /// the read side has to be woken. A stored permit cannot serve both, since
    /// taking it to answer a poll would swallow the other's wakeup.
    write_failed: AtomicBool,
    /// Wakes a read already parked on stdin once `write_failed` is set.
    ///
    /// The flag alone is only consulted between lines, and the read it has to
    /// interrupt can be parked indefinitely: stdout dying does not close
    /// stdin, so a redirected stdout hitting ENOSPC would otherwise leave the
    /// process waiting on input it could never answer. `notify_one` keeps the
    /// wakeup until it is taken, so a failure landing before the read parks is
    /// not lost, and a wait dropped in favor of the read puts the permit back.
    write_failed_signal: Notify,
    /// What the layer above owns and this one does not, run once the queue has
    /// drained and immediately before the process goes. Today that is the
    /// judgment cache the SDK holds.
    ///
    /// Stored rather than passed in at the call site, and that is the whole
    /// reason `exit` has one owner. When the SDK passed its own closure, it
    /// also had to be the caller, so `exit` was forwarded to a handler on a
    /// spawned task while the read side went on to end the session, and
    /// whichever arrived first decided whether the pending output was written
    /// or discarded. With the closure here the framing layer terminates for
    /// both, and there is no second path to race.
    ///
    /// A `OnceLock` because it is installed once at startup and read once at
    /// the exit; a later install is a wiring bug, not a state change.
    on_exit: std::sync::OnceLock<Box<dyn Fn() + Send + Sync>>,
    /// Request ids still owed a response.
    ///
    /// A set rather than a count, and deliberately so on both sides.
    ///
    /// Not a per-id count: RMCP keeps one cancellation-token entry per id
    /// (`local_ct_pool.insert` in its `service.rs`), so a client that reuses
    /// an id while the first request is still running gets exactly one
    /// response, and RMCP drops the other with "dropping response for
    /// cancelled request". Counting two owed responses would then hold end of
    /// input open for the full drain deadline waiting for a second response
    /// that RMCP has already discarded. Measured: 30s versus 0s to exit.
    ///
    /// Not one global counter either: a request can stop being owed a
    /// response without one being sent, when the client cancels it, and a
    /// bare total cannot express "this particular one is settled" without
    /// risking a double retire that would underflow it.
    in_flight: std::sync::Mutex<std::collections::HashSet<PeerRequestId>>,
}

impl Lifecycle {
    fn mark_shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
    }

    /// Record that stdout is gone. Set by the writer before it stops.
    fn mark_write_failed(&self) {
        self.write_failed.store(true, Ordering::Release);
        self.write_failed_signal.notify_one();
    }

    /// Resolve once stdout has failed, for the read that has to be woken.
    async fn write_failed_wait(&self) {
        self.write_failed_signal.notified().await;
    }

    /// Whether stdout has failed, for the waits that poll rather than park.
    fn write_failed(&self) -> bool {
        self.write_failed.load(Ordering::Acquire)
    }

    /// Whether anything is wired to run before the process goes.
    ///
    /// The framing layer terminates on `exit` and cannot reach what the layer
    /// above owns, so a server that forgets to install its flush loses it on
    /// every clean exit with nothing else failing. This is what lets that
    /// wiring be asserted where it is done, in the SDK's own tests, which is
    /// the only thing that asks: nothing in the running server needs to know.
    #[cfg(test)]
    pub(crate) fn exit_hook_installed(&self) -> bool {
        self.on_exit.get().is_some()
    }

    /// Install what runs immediately before the process goes.
    ///
    /// Called once, while the server is being wired up and before any envelope
    /// has been read. A second call is ignored: the first owner keeps the
    /// slot, rather than an install racing an exit already in progress.
    pub(crate) fn set_exit_hook(&self, before_exit: impl Fn() + Send + Sync + 'static) {
        if self.on_exit.set(Box::new(before_exit)).is_err() {
            tracing::warn!("exit hook already installed, keeping the first");
        }
    }

    /// The status `exit` terminates with: 0 when `shutdown` came first, 1
    /// otherwise.
    ///
    /// This reads the same flag the gate reserves, so a client that pipelines
    /// `shutdown` and `exit` without waiting cannot race the two apart.
    pub(crate) fn exit_code(&self) -> i32 {
        if self.shutdown.load(Ordering::Acquire) {
            0
        } else {
            1
        }
    }

    /// Rank a severity so levels can be compared, in the order the spec
    /// defines. Exhaustive on purpose: a level added to the enum should not
    /// quietly fall into a catch-all and be filtered out at every setting.
    #[allow(deprecated)]
    pub(crate) fn log_rank(level: LoggingLevel) -> u8 {
        match level {
            LoggingLevel::Debug => 0,
            LoggingLevel::Info => 1,
            LoggingLevel::Notice => 2,
            LoggingLevel::Warning => 3,
            LoggingLevel::Error => 4,
            LoggingLevel::Critical => 5,
            LoggingLevel::Alert => 6,
            LoggingLevel::Emergency => 7,
        }
    }

    /// Record the level the client asked for. Logs below it stop being sent.
    #[allow(deprecated)]
    pub(crate) fn set_log_level(&self, level: LoggingLevel) {
        self.log_level
            .store(Self::log_rank(level), Ordering::Relaxed);
    }

    /// Start forwarding tracing output to the client.
    ///
    /// Called from three places, all of them a client asking for the same
    /// thing in the place its revision has for it: a `logging` key in the
    /// `initialize` capabilities, which is what this server has always honored
    /// and which RMCP's typed `ClientCapabilities` discards; the same key in a
    /// request's own `_meta`, which is where the handshake-free revision puts
    /// it; and `logging/setLevel`, which is the spec's way to ask. Repeat calls
    /// are no-ops.
    pub(crate) fn enable_logs(&self) {
        let mut slot = self.logs.lock().unwrap_or_else(PoisonError::into_inner);
        if slot.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        crate::trace::set_mcp_log_sender(Some(tx));
        *slot = Some(rx);
    }

    /// Queue whatever tracing output has accrued, as `notifications/message`.
    ///
    /// On `Lifecycle` rather than on the transport because the log receiver is
    /// here and because terminating needs it; the transport's own flushing
    /// goes through the same function.
    pub(crate) fn queue_logs(&self) {
        let Some(outbound) = self
            .outbound
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        else {
            return;
        };
        for message in self.drain_logs() {
            let frame = encode(&serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/message",
                "params": message,
            }));
            if let Ok(frame) = frame {
                let _ = outbound.send(Outbound {
                    frame,
                    answers: None,
                    done: None,
                });
            }
        }
    }

    /// The one way this process terminates on `exit`, for both sides of the
    /// handshake. The `Gate::Exit` arm in `receive` says why there is only one.
    ///
    /// The exit hook runs after the queue has drained, so that a scan
    /// finishing during the drain still counts.
    pub(crate) async fn terminate(&self) -> ! {
        self.queue_logs();
        self.drain_outbound().await;
        if let Some(before_exit) = self.on_exit.get() {
            // The read side is parked on this task and nothing else will
            // terminate, so a hook that panics would wedge the process rather
            // than cost a cache write. Unwinding here is not hypothetical: the
            // flush steps over a poisoned lock precisely because handlers do
            // panic.
            let run = std::panic::AssertUnwindSafe(before_exit);
            if std::panic::catch_unwind(run).is_err() {
                tracing::error!("the exit hook panicked, exiting anyway");
            }
        }
        std::process::exit(self.exit_code());
    }

    /// Wait for everything already queued to reach the client.
    ///
    /// `process::exit` does not run destructors and does not drain anything,
    /// so a reply queued just before it, a `shutdown` acknowledgement being
    /// the case that matters, would never be written. An empty frame writes
    /// no bytes; it is here to be acknowledged after the ones ahead of it.
    ///
    /// This covers what is already queued, not what is still being computed:
    /// a request in flight when `exit` arrives still loses its response,
    /// because `exit` is unconditional. End of input is the path that waits
    /// for those, through `drain_in_flight`.
    pub(crate) async fn drain_outbound(&self) {
        // Cloned into this future rather than borrowed, and it has to stay that
        // way: holding a sender across the await is what stops a concurrent
        // close() from finishing its writer join before this resumes. Hoisting
        // the clone out reintroduces the deadlock family this has already been
        // bitten by twice.
        let Some(outbound) = self
            .outbound
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        else {
            return;
        };
        let (done, written) = oneshot::channel();
        let queued = outbound.send(Outbound {
            frame: Vec::new(),
            answers: None,
            done: Some(done),
        });
        if queued.is_ok() {
            match tokio::time::timeout(FLUSH_TIMEOUT, written).await {
                Err(_) => {
                    tracing::warn!("exiting with output still queued: the client is not reading");
                }

                // Worth saying out loud rather than exiting quietly: the queue
                // emptied because stdout broke, not because it was written.
                Ok(Ok(Err(e))) => tracing::warn!("exiting with output unwritten: {e}"),

                // Either it was written, or the writer went away without
                // answering, which leaves nothing to report but also nothing
                // still queued.
                Ok(_) => {}
            }
        }
    }

    /// Install the queue this lifecycle hands frames to.
    fn set_outbound(&self, tx: mpsc::UnboundedSender<Outbound>) {
        *self.outbound.lock().unwrap_or_else(PoisonError::into_inner) = Some(tx);
    }

    fn close_outbound(&self) {
        self.outbound
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
    }

    /// Mark one request as accepted, until its response has been written.
    ///
    /// The framing layer consults this at end of input: RMCP ends its service
    /// loop the moment the transport reports EOF, so without this a request
    /// still being served loses its response.
    ///
    /// Both ends of this live in the transport, which is the only layer that
    /// sees a request arrive and its response leave. Counting in the handlers
    /// instead retired a request when the handler returned, which is before
    /// RMCP has serialized and written the response, so the drain could see
    /// nothing outstanding while a reply was still unwritten. It also left
    /// coverage up to each handler remembering to opt in, and three did not.
    pub(crate) fn accept_request(&self, id: PeerRequestId) {
        self.in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id);
    }

    /// Mark one request as answered, once its response is on the wire.
    ///
    /// A request RMCP never answers stays counted until the drain deadline.
    /// That costs a slower exit rather than a lost response, which is the
    /// direction to err in, and this server answers every request it accepts.
    ///
    /// Idempotent on purpose. A request stops being owed a response twice
    /// when the client cancels it and the response was already past RMCP's
    /// cancellation check; removing an absent id is a no-op, where a counter
    /// would underflow.
    pub(crate) fn retire_request(&self, id: &PeerRequestId) {
        self.in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id);
    }

    /// How many requests are still owed a response.
    fn outstanding(&self) -> usize {
        self.in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    pub(crate) fn initialized(&self) -> bool {
        self.initialized.load(Ordering::Acquire)
    }

    fn may_route_peer_response(&self) -> bool {
        self.initialized() || self.outstanding() != 0
    }

    /// Open the post-handshake request gate after a handshake succeeds.
    pub(crate) fn mark_initialized(&self) {
        self.initialized.store(true, Ordering::Release);
    }

    /// Take whatever tracing output has accrued since the last drain.
    ///
    /// Logs accrue synchronously on the thread serving the request, so a
    /// single drain afterward catches everything for that request.
    pub(crate) fn drain_logs(&self) -> Vec<crate::trace::McpLogMessage> {
        let slot = self.logs.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(rx) = slot.as_ref() else {
            return Vec::new();
        };

        // Drained either way, so a level the client is not reading cannot
        // accumulate in the channel.
        let floor = self.log_level.load(Ordering::Relaxed);
        rx.try_iter()
            .filter(|message| Self::log_rank(message.level) >= floor)
            .collect()
    }
}

/// How much of an unterminated line to discard before giving up on the
/// client. Generous next to the 4 MiB line bound: this is reached only by a
/// stream with no newline in it at all.
const MAX_DRAIN_BYTES: usize = 64 * 1024 * 1024;

/// How long either shutdown path waits for queued output to reach the client.
///
/// Delivery is best effort and termination is not: a client that stops reading
/// its end of the pipe leaves the write unable to complete, and without a
/// bound here that client can keep this process alive indefinitely.
const FLUSH_TIMEOUT: Duration = Duration::from_secs(2);

/// How long the runtime is given to finish blocking work once the service
/// loop has returned. A separate deadline from `FLUSH_TIMEOUT` even at the
/// same value: that one bounds a write, this one bounds a scan.
pub const BLOCKING_SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// How long end of input waits for handlers to finish before giving up on
/// them. Past the longest a handler should take: a sampling round trip is
/// capped at five seconds and a scan is bounded by the text limit.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

/// How often the drain rechecks. Short enough not to add visible latency to a
/// normal exit, long enough not to spin.
const DRAIN_POLL: Duration = Duration::from_millis(5);

/// One frame on its way out, and whether writing it answers a request.
struct Outbound {
    frame: Vec<u8>,
    answers: Option<PeerRequestId>,
    /// Signalled once the frame is written, for callers that must not run
    /// ahead of it. RMCP awaits `send`, and on a refused handshake it tears
    /// the session down as soon as that returns: without this the process
    /// could exit with the refusal still queued.
    done: Option<oneshot::Sender<std::io::Result<()>>>,
}

/// Write queued frames in order. Keeping this independent of stdout makes
/// partial writes and failures observable to the sender that queued a frame.
///
/// Every frame is flushed, not just written. `tokio::io::stdout` buffers, so
/// `write_all` reports success before the blocking write has run and the real
/// error surfaces on the following flush. Skipping it would hide the failures
/// this reports.
///
/// A request is retired after its write, not when the handler returned: the
/// response does not exist until it is on the wire, and end of input consults
/// this to decide whether anything is still owed.
async fn write_outbound<W: AsyncWrite + Unpin>(
    out: &mut W,
    rx: &mut mpsc::UnboundedReceiver<Outbound>,
    lifecycle: &Lifecycle,
) {
    // The error that stopped the writer, once one has. Everything still queued
    // is then settled from it rather than written, which is what lets the
    // frames behind a failure report the errno that actually happened instead
    // of a synthesized BrokenPipe standing in for all of them.
    let mut failed: Option<std::io::ErrorKind> = None;
    while let Some(item) = rx.recv().await {
        let written = match failed {
            Some(kind) => Err(std::io::Error::from(kind)),
            None => match out.write_all(&item.frame).await {
                Ok(()) => out.flush().await,
                Err(error) => Err(error),
            },
        };
        if let (None, Err(error)) = (failed, written.as_ref()) {
            failed = Some(error.kind());

            // Raised before the sender is woken. That wake can resume RMCP,
            // which re-enters receive, and the read has to see the failure
            // rather than park on a stdin that may never say anything again.
            lifecycle.mark_write_failed();

            // Closing is what makes the rest of this loop terminate: senders
            // are still held by the lifecycle and the transport, so recv would
            // otherwise wait forever for a frame that can no longer be written.
            rx.close();
        }
        if let Some(done) = item.done {
            let _ = done.send(written);
        }
        if let Some(id) = &item.answers {
            lifecycle.retire_request(id);
        }
    }
}

/// Build the stdio transport for `lifecycle`.
pub fn stdio(lifecycle: Arc<Lifecycle>) -> StdioTransport {
    let (outbound, mut rx) = mpsc::unbounded_channel::<Outbound>();

    // Writing happens here and nowhere else. RMCP polls receive inside a
    // select! and drops that future whenever another arm wins, which under a
    // stream of responses is most of the time. A write awaited inside it is
    // dropped with it, losing the reply outright, and a write dropped partway
    // through leaves half a frame on the wire for the next one to run into.
    // Handing frames to a task instead puts them outside anything that gets
    // cancelled, and a queue keeps them in the order they were produced.
    let writer = tokio::spawn({
        let lifecycle = lifecycle.clone();
        async move {
            let mut out = tokio::io::stdout();
            write_outbound(&mut out, &mut rx, &lifecycle).await;
        }
    });
    lifecycle.set_outbound(outbound.clone());
    StdioTransport {
        reader: BufReader::new(tokio::io::stdin()),
        outbound,
        writer: Some(writer),
        lifecycle,
        raw: Vec::new(),
        drain_deadline: None,
    }
}

pub struct StdioTransport {
    reader: BufReader<Stdin>,
    outbound: mpsc::UnboundedSender<Outbound>,
    writer: Option<tokio::task::JoinHandle<()>>,
    lifecycle: Arc<Lifecycle>,
    raw: Vec<u8>,
    /// Set on the first end of input, so a drain restarted after cancellation
    /// keeps counting from when it actually began.
    drain_deadline: Option<tokio::time::Instant>,
}

impl StdioTransport {
    /// Queue one frame and forget it. Never awaits, so a caller that is
    /// cancelled mid-call cannot lose it. For the framing layer's own
    /// replies, which are produced inside `receive`.
    fn enqueue(&self, frame: Vec<u8>) {
        let _ = self.outbound.send(Outbound {
            frame,
            answers: None,
            done: None,
        });
    }

    /// Queue one frame and hand back something that completes when it has
    /// been written. For `send`, which RMCP awaits outside `receive`.
    fn enqueue_tracked(
        &self,
        frame: Vec<u8>,
        answers: Option<PeerRequestId>,
    ) -> oneshot::Receiver<std::io::Result<()>> {
        let (done, written) = oneshot::channel();

        // A refused send hands the frame back, so the id it answers can be
        // retired from that rather than from a clone kept for the failure.
        // Reporting through the same channel the caller already awaits keeps
        // one path out of here rather than a second one beside it.
        if let Err(mpsc::error::SendError(mut item)) = self.outbound.send(Outbound {
            frame,
            answers,
            done: Some(done),
        }) {
            if let Some(id) = &item.answers {
                self.lifecycle.retire_request(id);
            }
            if let Some(done) = item.done.take() {
                let _ = done.send(Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe)));
            }
        }
        written
    }
}

/// Outcome of one bounded line read.
enum ReadLine {
    Line(String),
    Eof,
    TooLong,
    MalformedUtf8,
    /// An unterminated line so long that the discard gave up on finding where
    /// it ends. Nothing further can be framed from this stream.
    Unrecoverable,
}

/// Read one line, bounded to `MAX_LINE_BYTES`.
///
/// UTF-8 is validated exactly once, here, so no caller re-validates or risks a
/// panic on an invalid boundary.
///
/// `raw` is the caller's buffer and it is deliberately not cleared on entry.
/// This future is polled inside RMCP's `select!`, so an in-progress read is
/// dropped whenever another branch becomes ready, which under a stream of
/// outgoing responses happens often. `read_until` appends as it goes and only
/// returns at a delimiter or end of input, so a cancelled read leaves its
/// bytes here for the next call to resume from. Clearing on entry instead
/// discards them, splitting the client's line in two: the request is never
/// answered and the client waits forever.
async fn read_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    raw: &mut Vec<u8>,
) -> std::io::Result<ReadLine> {
    // The bound is on the line, not the call, so a resumed read gets what is
    // left of the budget. Saturating because a cancelled read may already hold
    // all of it, in which case there is nothing left to take.
    let budget = (MAX_LINE_BYTES + 1).saturating_sub(raw.len()) as u64;
    reader.take(budget).read_until(b'\n', raw).await?;

    // read_until stops at a delimiter, at end of input, or when the budget runs
    // out, and its byte count does not say which. What distinguishes them is
    // the buffer: a delimiter at the end, or the whole budget spent without
    // one.
    if !raw.ends_with(b"\n") && raw.len() > MAX_LINE_BYTES {
        let recovered = drain_until_newline(reader).await?;
        raw.clear();

        // If the discard gave up, the stream has no line boundary left to
        // resynchronize on, so there is nothing to go back to reading.
        return Ok(if recovered {
            ReadLine::TooLong
        } else {
            ReadLine::Unrecoverable
        });
    }
    if raw.is_empty() {
        return Ok(ReadLine::Eof);
    }

    // Either a whole line, or a final one the client left unterminated before
    // closing. The second still parses: a batch caller that writes its last
    // request without a newline and closes gets it answered, and end of input
    // is reported on the next call, once the buffer is empty.
    let line = match std::str::from_utf8(raw) {
        Ok(line) => ReadLine::Line(line.trim().to_owned()),
        Err(_) => ReadLine::MalformedUtf8,
    };
    raw.clear();
    Ok(line)
}

/// Read one line, unless stdout fails first. `None` means the session is over.
///
/// Selected rather than awaited bare: stdout dying does not close stdin, so
/// the read can sit on input that is never coming, and a failure noticed only
/// between lines would never be noticed at all.
///
/// Dropping the read here is safe for the same reason a cancelled read is:
/// `raw` belongs to the caller, so nothing already read is lost.
async fn read_line_unless_write_failed<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    raw: &mut Vec<u8>,
    lifecycle: &Lifecycle,
) -> Option<std::io::Result<ReadLine>> {
    tokio::select! {
        biased;
        () = lifecycle.write_failed_wait() => None,
        result = read_line(reader, raw) => Some(result),
    }
}

/// Consume and discard bytes until a newline or EOF, so the line after an
/// oversize one still parses.
async fn drain_until_newline<R: AsyncBufRead + Unpin>(reader: &mut R) -> std::io::Result<bool> {
    let mut discarded = 0usize;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(true);
        }
        match available.iter().position(|&b| b == b'\n') {
            Some(pos) => {
                reader.consume(pos + 1);
                return Ok(true);
            }
            None => {
                let len = available.len();
                reader.consume(len);
                discarded = discarded.saturating_add(len);

                // A line that never ends is not a line. Without a bound this
                // reads forever, and since a client has one stream, it would
                // never get to send anything else either.
                if discarded > MAX_DRAIN_BYTES {
                    return Ok(false);
                }
            }
        }
    }
}

/// Serialize one message as a newline-terminated JSON-RPC frame.
fn encode(message: &impl serde::Serialize) -> Result<Vec<u8>, serde_json::Error> {
    let mut line = serde_json::to_vec(message)?;
    line.push(b'\n');
    Ok(line)
}

/// The request a `notifications/cancelled` refers to, if it is one.
fn cancelled_request_id(
    notification: &rmcp::model::JsonRpcNotification<rmcp::model::ClientNotification>,
) -> Option<PeerRequestId> {
    match &notification.notification {
        rmcp::model::ClientNotification::CancelledNotification(cancelled) => {
            cancelled.params.request_id.clone()
        }
        _ => None,
    }
}

/// Wait for accepted requests to produce their responses.
///
/// Every request is counted by `receive` before it is handed to RMCP, so by
/// the time end of input is seen here, everything dispatched before it is
/// already counted. Nothing has to be yielded to first: there is no window
/// between a request being dispatched and its being registered.
///
/// Free-standing and taking only the lifecycle, so the waiting can be tested
/// without a transport, a subprocess, or a lint slow enough to still be
/// running when end of input lands.
async fn drain_in_flight(lifecycle: &Lifecycle, deadline: &mut Option<tokio::time::Instant>) {
    // Kept by the caller for the same reason the read buffer is: this runs
    // inside a future RMCP cancels, and a deadline restarted on every
    // cancellation is not a deadline. A handler that keeps producing peer
    // traffic would otherwise hold the process open indefinitely.
    let deadline = *deadline.get_or_insert_with(|| tokio::time::Instant::now() + DRAIN_TIMEOUT);
    loop {
        if lifecycle.write_failed() {
            return;
        }
        let outstanding = lifecycle.outstanding();
        if outstanding == 0 {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            tracing::warn!("end of input with {outstanding} request(s) still running");
            return;
        }
        tokio::select! {
            () = lifecycle.write_failed_wait() => return,
            () = tokio::time::sleep(DRAIN_POLL) => {},
        }
    }
}

impl StdioTransport {
    /// Emit any pending tracing output as `notifications/message`, or discard
    /// it when nobody is owed it.
    ///
    /// Only a handshake buys delivery here. Before one, a line has no request
    /// that asked for it, so it is dropped rather than held for whichever
    /// later request happens to ask. Dropped only with nothing in flight,
    /// because a request still running may yet be owed it.
    fn flush_logs(&self) {
        if self.lifecycle.initialized() {
            self.lifecycle.queue_logs();
        } else if self.lifecycle.outstanding() == 0 {
            // Called for the drain, not the lines: this is the discard.
            self.lifecycle.drain_logs();
        }
    }

    /// Write a framing-level response directly, bypassing RMCP.
    ///
    /// Whatever this request logged goes out first: clients read causality
    /// from that order.
    fn reply(&self, response: JsonRpcResponse) {
        self.flush_logs();
        if let Ok(frame) = encode(&response) {
            self.enqueue(frame);
        }
    }
}

/// What the framing layer does with an envelope before RMCP sees it.
enum Gate {
    /// Hand it to RMCP.
    Forward,
    /// Answer here.
    Reply(Box<JsonRpcResponse>),
    /// Drop in silence, which is what a notification gets when there is
    /// nothing to say back.
    Drop,
    /// Terminate. The status is read at the exit, from the `shutdown` flag,
    /// rather than carried from here.
    Exit,
}

/// One-shot helper: a request is answered, a notification is dropped.
fn answer(id: Option<RequestId>, response: impl FnOnce(RequestId) -> JsonRpcResponse) -> Gate {
    match id {
        Some(id) => Gate::Reply(Box::new(response(id))),
        None => Gate::Drop,
    }
}

/// Decide the fate of one envelope.
fn gate(lifecycle: &Lifecycle, request: &super::types::JsonRpcRequest) -> Gate {
    let method = request.method.as_str();
    let id = request.id.clone();

    // exit is honored regardless of lifecycle state, and honored here rather
    // than forwarded. Forwarding it after the handshake put termination on a
    // task RMCP spawns while the read side went on to see end of input and end
    // the session, and ending the session takes the outbound queue away from
    // the drain the exit was about to run. The judgment cache was the only
    // reason to forward, and the lifecycle holds that as an exit hook now.
    if method == "exit" {
        return Gate::Exit;
    }
    if lifecycle.shutdown.load(Ordering::Acquire) {
        tracing::warn!("rejecting {method} after shutdown");
        return answer(id, |id| {
            JsonRpcResponse::error(Some(id), INVALID_REQUEST, "server is shutting down".into())
        });
    }

    // A notification carrying an id is a client bug that used to be reported
    // rather than silently reinterpreted.
    if method.starts_with("notifications/") && id.is_some() {
        return answer(id, |id| {
            JsonRpcResponse::error(
                Some(id),
                INVALID_REQUEST,
                format!("{method} must be sent as a notification (no id)"),
            )
        });
    }

    // Answered here rather than forwarded, for two reasons: the handler runs
    // asynchronously, so a pipelined request could otherwise pass this gate
    // while it waits to run, and before the handshake RMCP would treat it as a
    // failed initialize and end the session.
    if method == "shutdown" {
        tracing::info!("shutdown requested");
        lifecycle.mark_shutdown();
        return answer(id, |id| {
            JsonRpcResponse::success(Some(id), serde_json::json!({}))
        });
    }
    if method == "initialize" {
        // A logging key in the client capabilities is this server's own
        // extension, and RMCP's typed capabilities drop it before the handler
        // ever sees it, so it is read here off the raw envelope.
        if request.params.pointer("/capabilities/logging").is_some() {
            lifecycle.enable_logs();
        }
        return Gate::Forward;
    }

    // Discovery is by definition pre-handshake, and it is the one question a
    // client on a revision this server does not serve can still ask. Refusing
    // it here for a malformed or unknown declaration answers "server not
    // initialized", which is both untrue and useless: the gate does not know
    // the version list, and that list is the entire point of the request. RMCP
    // owns the _meta contract, so let it answer with the key it is missing or
    // with the revisions on offer. Forwarding does not open the gate; nothing
    // on this path marks the session initialized.
    if method == "server/discover" {
        return Gate::Forward;
    }

    // 2026-07-28 deleted the handshake: every request carries its own protocol
    // declaration in _meta, so a client may open a connection and send a call
    // on it without a preceding initialize or server/discover. The declaration
    // is request-scoped: do not turn it into connection state.
    //
    // What counts as a declaration is the revision table's business, not the
    // framing layer's, so both questions are asked of revisions.
    if let Some(meta) = super::revisions::declaration(&request.params) {
        if super::revisions::is_self_declaring(meta) {
            // Same extension the handshake path reads, in the place this
            // revision has for it. Wired here rather than left to initialize,
            // which a client on this path never sends.
            if super::revisions::logging_opt_in(meta) {
                lifecycle.enable_logs();
            }
            return Gate::Forward;
        }
    }
    if lifecycle.initialized() {
        return Gate::Forward;
    }

    // Pre-init. RMCP ends the session on anything but initialize, so these are
    // answered here instead.
    match method {
        "ping" => answer(id, |id| {
            JsonRpcResponse::success(Some(id), serde_json::json!({}))
        }),
        _ if method.starts_with("notifications/") => Gate::Drop,
        _ => {
            tracing::warn!("rejecting {method} before initialization");
            answer(id, |id| {
                JsonRpcResponse::error(
                    Some(id),
                    SERVER_NOT_INITIALIZED,
                    "server not initialized".into(),
                )
            })
        }
    }
}

impl Transport<RoleServer> for StdioTransport {
    type Error = std::io::Error;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleServer>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        // Only a reply retires a request. A notification answers nothing, and a
        // server-initiated request (sampling) is this server asking, not
        // answering.
        let answers = match &item {
            TxJsonRpcMessage::<RoleServer>::Response(response) => Some(response.id.clone()),

            // An error response may carry no id, when nothing could be
            // correlated; there is then no request for it to retire.
            TxJsonRpcMessage::<RoleServer>::Error(error) => error.id.clone(),
            _ => None,
        };
        let queued = match encode(&item) {
            Ok(frame) => self.enqueue_tracked(frame, answers),
            Err(e) => {
                // Nothing will be written, so nothing will retire this one
                // later. Leaving it outstanding would hold end of input open
                // for a response that is never coming.
                if let Some(id) = &answers {
                    self.lifecycle.retire_request(id);
                }

                // Reported the same way a refused send is, so both ways of
                // never reaching the wire come back through one channel.
                let (done, written) = oneshot::channel();
                let _ = done.send(Err(std::io::Error::from(e)));
                written
            }
        };
        async move {
            // Resolves when the frame is on the wire, or immediately if it
            // never got queued, which means nothing more will be written
            // anyway. A dropped sender is the writer already gone.
            queued
                .await
                .map_err(|_| std::io::Error::from(std::io::ErrorKind::BrokenPipe))?
        }
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleServer>> {
        loop {
            // Framing-level logs (a parse error, an oversize line) have no
            // response to ride along with, so they go out before the next read
            // blocks. Request-scoped logs are flushed by the handler instead,
            // which is what keeps them ahead of their own response.
            self.flush_logs();

            // Nothing can be answered once stdout is gone, and RMCP only logs
            // the send errors that say so. Reading on would accept requests
            // whose replies go nowhere until stdin happened to close too.
            let Some(read) =
                read_line_unless_write_failed(&mut self.reader, &mut self.raw, &self.lifecycle)
                    .await
            else {
                tracing::error!("stdout write failed, closing the session");
                return None;
            };
            let line = match read {
                Ok(ReadLine::Line(line)) => line,
                Ok(ReadLine::Eof) => {
                    // Stdin closing means the client stopped sending, not that
                    // it stopped listening: a batch caller writes its requests,
                    // closes the write half, and waits for the answers.
                    // Reporting EOF now would end RMCP's service loop with
                    // handlers still running and their responses unwritten.
                    drain_in_flight(&self.lifecycle, &mut self.drain_deadline).await;
                    self.flush_logs();
                    return None;
                }
                Ok(ReadLine::Unrecoverable) => {
                    tracing::error!(
                        "no line boundary within {MAX_DRAIN_BYTES} bytes, closing the session"
                    );
                    self.reply(JsonRpcResponse::error(
                        None,
                        INVALID_REQUEST,
                        "request too large".into(),
                    ));
                    return None;
                }
                Ok(ReadLine::TooLong) => {
                    tracing::warn!("request exceeds {MAX_LINE_BYTES} bytes");
                    self.reply(JsonRpcResponse::error(
                        None,
                        INVALID_REQUEST,
                        "request too large".into(),
                    ));
                    continue;
                }
                Ok(ReadLine::MalformedUtf8) => {
                    self.reply(JsonRpcResponse::error(
                        None,
                        super::types::PARSE_ERROR,
                        "invalid UTF-8 in request".into(),
                    ));
                    continue;
                }
                Err(e) => {
                    tracing::error!("stdin read failed: {e}");
                    return None;
                }
            };
            if line.is_empty() {
                continue;
            }

            // Validate the envelope before RMCP sees it. The line is parsed
            // twice, once here and once by RMCP's own deserializer; that costs
            // one JSON pass per message and buys a single definition of what a
            // well-formed request is.
            let request = match parse_jsonrpc_line(&line) {
                Ok(request) => request,
                Err(TransportError::PeerResponse) => {
                    // A reply to a request this server sent: sampling today,
                    // and whatever server-to-client request comes next. RMCP's
                    // peer owns the ids it has outstanding, so the reply has to
                    // reach it; answering nothing here leaves the request
                    // waiting for its timeout and the sampled answer lost. A
                    // reply carrying no id, or one whose envelope RMCP cannot
                    // model, has nothing to correlate against and is dropped,
                    // which is what the peer would do with it anyway. Only once
                    // a session exists. Before the handshake this server has
                    // asked the client nothing, so there is no id for a reply
                    // to match, and RMCP reads any non-request arriving there
                    // as a failed handshake and ends the session. Dropping it
                    // costs nothing and keeps a stray line from taking the
                    // connection down.
                    if !self.lifecycle.may_route_peer_response() {
                        continue;
                    }
                    match serde_json::from_str(&line) {
                        Ok(message) => return Some(message),
                        Err(_) => continue,
                    }
                }
                Err(e) => {
                    tracing::warn!("{e}");
                    if let Some(response) = e.into_response(None) {
                        self.reply(response);
                    }
                    continue;
                }
            };

            match gate(&self.lifecycle, &request) {
                Gate::Forward => {}
                Gate::Reply(response) => {
                    self.reply(*response);
                    continue;
                }
                Gate::Drop => continue,
                Gate::Exit => {
                    tracing::info!("exit notification, terminating");

                    // Spawned rather than awaited here, and the two halves are
                    // one fix. RMCP polls receive inside a select and drops
                    // this future the moment a response is ready to send, so a
                    // termination awaited here is cancelled halfway with the
                    // exit line already off the wire: nothing terminates, and
                    // the next read parks on a client that has said everything
                    // it means to say. A spawned task is not cancelled.
                    //
                    // Parking then keeps the session from ending underneath it.
                    // Returning None here instead would run close, close takes
                    // the outbound queue, and the drain the termination is on
                    // its way to would find nothing to wait on and exit with
                    // the queue unwritten.
                    let lifecycle = self.lifecycle.clone();
                    tokio::spawn(async move { lifecycle.terminate().await });

                    // Never resolves. The spawned task is what ends this
                    // process, and it is bounded by FLUSH_TIMEOUT, so a
                    // deadline here would only second-guess that one.
                    std::future::pending().await
                }
            }

            match serde_json::from_str::<RxJsonRpcMessage<RoleServer>>(&line) {
                Ok(message) => {
                    // Counted here rather than in the handlers, so every
                    // request is covered whether or not its handler knows about
                    // the drain. A notification has nothing to answer.
                    match &message {
                        RxJsonRpcMessage::<RoleServer>::Request(request) => {
                            self.lifecycle.accept_request(request.id.clone());
                        }

                        // A cancelled request will never be answered, so it
                        // stops being owed one here. Without this, end of input
                        // waits out its whole deadline for a response that by
                        // definition is not coming.
                        RxJsonRpcMessage::<RoleServer>::Notification(notification) => {
                            if let Some(id) = cancelled_request_id(notification) {
                                self.lifecycle.retire_request(&id);
                            }
                        }
                        _ => {}
                    }
                    return Some(message);
                }
                Err(e) => {
                    // A valid JSON-RPC envelope RMCP cannot model: report it
                    // rather than dropping the client's request on the floor.
                    tracing::warn!("unsupported message shape: {e}");
                    if request.id.is_some() {
                        self.reply(JsonRpcResponse::error(
                            request.id,
                            INVALID_REQUEST,
                            e.to_string(),
                        ));
                    }
                }
            }
        }
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        // Dropping the queue ends the writer task once it has drained, so
        // whatever was queued still reaches the client before the process goes.
        // Waiting on the task is what makes that ordering hold.
        self.lifecycle.close_outbound();
        self.outbound = mpsc::unbounded_channel().0;
        if let Some(writer) = self.writer.take() {
            // Bounded for the same reason the exit drain is: with both senders
            // dropped the writer ends once it has drained, but a client that
            // has stopped reading leaves it blocked in a write it cannot
            // finish, and this await is what end of input returns through.
            if tokio::time::timeout(FLUSH_TIMEOUT, writer).await.is_err() {
                tracing::warn!("closing with output still queued: the client is not reading");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    /// Fails at one of the two points a write can fail, with a caller-chosen
    /// kind so the tests can tell a carried error from a synthesized one.
    struct FailingWriter {
        on_flush: bool,
        kind: io::ErrorKind,
    }

    impl FailingWriter {
        fn on_write(kind: io::ErrorKind) -> Self {
            Self {
                on_flush: false,
                kind,
            }
        }

        /// The shape `tokio::io::stdout` actually fails in: it buffers, so the
        /// write reports success and the error arrives on the flush behind it.
        fn on_flush(kind: io::ErrorKind) -> Self {
            Self {
                on_flush: true,
                kind,
            }
        }
    }

    impl AsyncWrite for FailingWriter {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            if self.on_flush {
                Poll::Ready(Ok(buf.len()))
            } else {
                Poll::Ready(Err(io::Error::from(self.kind)))
            }
        }

        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            if self.on_flush {
                Poll::Ready(Err(io::Error::from(self.kind)))
            } else {
                Poll::Ready(Ok(()))
            }
        }

        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    fn frame(answers: Option<PeerRequestId>) -> (Outbound, oneshot::Receiver<io::Result<()>>) {
        let (done, written) = oneshot::channel();
        (
            Outbound {
                frame: b"frame\n".to_vec(),
                answers,
                done: Some(done),
            },
            written,
        )
    }

    fn lifecycle() -> Lifecycle {
        Lifecycle::default()
    }

    #[tokio::test]
    async fn a_failure_at_either_point_reaches_the_sender_and_stops_the_read() {
        // Both points matter. A real stdout buffers, so the write reports
        // success and the error only shows up when the buffer is pushed out:
        // losing the flush would leave that case undetected entirely.
        for mut out in [
            FailingWriter::on_write(io::ErrorKind::ConnectionReset),
            FailingWriter::on_flush(io::ErrorKind::PermissionDenied),
        ] {
            let kind = out.kind;
            let lifecycle = lifecycle();
            let (tx, mut rx) = mpsc::unbounded_channel();
            let (item, written) = frame(None);
            tx.send(item).unwrap();
            drop(tx);

            write_outbound(&mut out, &mut rx, &lifecycle).await;
            assert_eq!(written.await.unwrap().unwrap_err().kind(), kind);

            // RMCP only logs the send errors, so raising this is what stops the
            // server accepting requests it can no longer answer.
            tokio::time::timeout(Duration::from_secs(1), lifecycle.write_failed_wait())
                .await
                .expect("a write failure has to stop the read side");
        }
    }

    #[tokio::test]
    async fn writer_failure_retires_queued_responses() {
        let lifecycle = lifecycle();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut queued = Vec::new();
        for id in [1, 2] {
            let id = PeerRequestId::Number(id);
            lifecycle.accept_request(id.clone());
            let (item, written) = frame(Some(id));
            tx.send(item).unwrap();
            queued.push(written);
        }

        // tx stays alive on purpose. Both the lifecycle and the transport hold
        // sender clones in the real thing, so the drain has to close the
        // receiver itself; without that this waits forever for a frame that can
        // no longer be written.
        let mut out = FailingWriter::on_write(io::ErrorKind::ConnectionReset);
        let drained = tokio::time::timeout(
            Duration::from_secs(1),
            write_outbound(&mut out, &mut rx, &lifecycle),
        )
        .await;
        assert!(drained.is_ok(), "the drain must not wait on a live sender");

        assert_eq!(lifecycle.outstanding(), 0);
        // Every frame behind the failure learns why, not a stand-in for it.
        for written in queued {
            assert_eq!(
                written.await.unwrap().unwrap_err().kind(),
                io::ErrorKind::ConnectionReset
            );
        }
        assert!(
            tx.send(frame(None).0).is_err(),
            "a closed queue must reject anything sent after the failure"
        );
    }

    #[tokio::test]
    async fn a_parked_read_gives_up_when_stdout_fails() {
        // The whole point of the signal. Stdout dying does not close stdin, so
        // a failure noticed only between lines would never be noticed at all:
        // this read is parked on a client that has stopped talking.
        let lifecycle = Arc::new(lifecycle());
        // Held open, so the read parks rather than seeing end of input.
        let (client, _server) = tokio::io::duplex(64);
        let read = tokio::spawn({
            let lifecycle = lifecycle.clone();
            async move {
                let mut reader = BufReader::new(client);
                let mut raw = Vec::new();
                read_line_unless_write_failed(&mut reader, &mut raw, &lifecycle)
                    .await
                    .is_none()
            }
        });

        // Let the read park before the failure lands, which is the ordering
        // that has no other way out.
        tokio::task::yield_now().await;

        lifecycle.mark_write_failed();

        let gave_up = tokio::time::timeout(Duration::from_secs(1), read)
            .await
            .expect("a parked read must give up, not wait on input that is not coming")
            .unwrap();
        assert!(gave_up, "the failure has to win the select, not the read");
    }

    fn req(method: &str, id: Option<i64>) -> super::super::types::JsonRpcRequest {
        super::super::types::JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: id.map(RequestId::Int),
            method: method.into(),
            params: serde_json::json!({}),
        }
    }

    /// A request declaring the handshake-free revision the way its clients do.
    fn declared(method: &str, id: i64) -> super::super::types::JsonRpcRequest {
        let mut request = req(method, Some(id));
        request.params = serde_json::json!({
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {}
            }
        });
        request
    }

    #[test]
    fn pre_init_request_is_rejected_not_fatal() {
        let lc = lifecycle();
        let Gate::Reply(response) = gate(&lc, &req("tools/list", Some(1))) else {
            panic!("a pre-init request must be answered, not dropped");
        };
        assert_eq!(response.error.unwrap().code, SERVER_NOT_INITIALIZED);
    }

    #[test]
    fn self_declaring_request_is_served_without_opening_the_gate() {
        // 2026-07-28 clients open a connection per call and declare the
        // revision in _meta, so this must be served, not refused. What it must
        // not do is turn that declaration into connection state: the undeclared
        // follow-up is how the gate proves it stayed shut.
        let lc = lifecycle();
        assert!(matches!(
            gate(&lc, &declared("tools/list", 1)),
            Gate::Forward
        ));

        let Gate::Reply(response) = gate(&lc, &req("tools/call", Some(2))) else {
            panic!("an undeclared follow-up request must be refused");
        };
        assert_eq!(response.error.unwrap().code, SERVER_NOT_INITIALIZED);
    }

    #[test]
    fn an_active_stateless_request_allows_its_peer_reply() {
        let lc = lifecycle();
        assert!(!lc.may_route_peer_response());
        lc.accept_request(PeerRequestId::Number(1));
        assert!(lc.may_route_peer_response());
        lc.retire_request(&PeerRequestId::Number(1));
        assert!(!lc.may_route_peer_response());
    }

    #[test]
    fn a_handshake_revision_in_meta_does_not_skip_the_handshake() {
        // _meta is not where the older revisions carry the protocol version, so
        // naming one there buys no exemption from their initialize.
        let lc = lifecycle();
        let mut request = req("tools/list", Some(1));
        request.params = serde_json::json!({
            "_meta": { "io.modelcontextprotocol/protocolVersion": "2025-06-18" }
        });
        let Gate::Reply(response) = gate(&lc, &request) else {
            panic!("a request that declares a handshake revision must be refused");
        };
        assert_eq!(response.error.unwrap().code, SERVER_NOT_INITIALIZED);
        assert!(!lc.initialized.load(Ordering::Relaxed));
    }

    #[test]
    fn pre_init_notification_is_dropped() {
        let lc = lifecycle();
        assert!(matches!(
            gate(&lc, &req("notifications/initialized", None)),
            Gate::Drop
        ));
    }

    #[test]
    fn pre_init_ping_is_answered() {
        let lc = lifecycle();
        let Gate::Reply(response) = gate(&lc, &req("ping", Some(1))) else {
            panic!("pre-init ping must be answered");
        };
        assert!(response.result.is_some());
    }

    #[test]
    fn successful_initialize_opens_the_gate() {
        let lc = lifecycle();
        assert!(matches!(
            gate(&lc, &req("initialize", Some(1))),
            Gate::Forward
        ));
        lc.mark_initialized();
        assert!(matches!(
            gate(&lc, &req("tools/list", Some(2))),
            Gate::Forward
        ));
    }

    #[test]
    fn notification_with_id_is_rejected() {
        let lc = lifecycle();
        lc.initialized.store(true, Ordering::Relaxed);
        let Gate::Reply(response) = gate(&lc, &req("notifications/cancelled", Some(3))) else {
            panic!("an id-bearing notification must be answered");
        };
        assert_eq!(response.error.unwrap().code, INVALID_REQUEST);
    }

    #[test]
    fn after_shutdown_everything_but_exit_is_rejected() {
        let lc = lifecycle();
        lc.initialized.store(true, Ordering::Relaxed);
        lc.mark_shutdown();
        assert!(matches!(gate(&lc, &req("exit", None)), Gate::Exit));
        let Gate::Reply(response) = gate(&lc, &req("tools/list", Some(4))) else {
            panic!("a post-shutdown request must be answered");
        };
        assert_eq!(response.error.unwrap().code, INVALID_REQUEST);
    }

    #[test]
    fn discover_forwards_without_opening_the_gate() {
        // Forwarded whatever it declares: the branch above the declaration
        // check takes it, because the version list is the one answer a client
        // on an unknown revision still needs. Declaring is therefore not what
        // is under test, and passing a declaration would hide which branch
        // answered.
        let lc = lifecycle();
        assert!(matches!(
            gate(&lc, &req("server/discover", Some(1))),
            Gate::Forward
        ));
        assert!(!lc.initialized(), "discovery must not mark the session up");
    }

    #[test]
    fn pre_init_exit_terminates_rather_than_forwarding() {
        // Forwarding it would reach RMCP as a failed handshake, which ends the
        // session with the wrong status and an error the client did not cause.
        // The gate decides that this terminates; what status it terminates with
        // is read at the exit, from the same flag either path consults.
        let lc = lifecycle();
        assert!(matches!(gate(&lc, &req("exit", None)), Gate::Exit));
        assert_eq!(lc.exit_code(), 1);
        lc.mark_shutdown();
        assert!(matches!(gate(&lc, &req("exit", None)), Gate::Exit));
        assert_eq!(lc.exit_code(), 0);
    }

    #[test]
    fn pre_init_shutdown_is_answered_not_forwarded() {
        // Forwarding it would reach RMCP as a failed handshake and end the
        // session, which is the one thing the pre-init gate exists to prevent.
        let lc = lifecycle();
        let Gate::Reply(response) = gate(&lc, &req("shutdown", Some(1))) else {
            panic!("a pre-init shutdown must be answered here");
        };
        assert!(response.result.is_some());
        assert_eq!(lc.exit_code(), 0);
    }

    #[test]
    fn shutdown_as_a_notification_sets_the_flag_without_replying() {
        let lc = lifecycle();
        lc.initialized.store(true, Ordering::Relaxed);
        assert!(matches!(gate(&lc, &req("shutdown", None)), Gate::Drop));
        assert_eq!(lc.exit_code(), 0);
    }

    #[test]
    fn a_handshake_does_not_move_the_exit_off_the_gate() {
        // The case the other two gate tests do not reach: initialized and not
        // shutting down, which is where exit used to be forwarded. That put
        // termination on a task RMCP spawns and raced it against the read side
        // ending the session. Anything but Gate::Exit here brings that back.
        // Pre-init is pinned by pre_init_exit_terminates_rather_than_forwarding
        // and post-shutdown by after_shutdown_everything_but_exit_is_rejected.
        let lc = lifecycle();
        lc.initialized.store(true, Ordering::Relaxed);
        assert!(matches!(gate(&lc, &req("exit", None)), Gate::Exit));
    }

    #[test]
    fn the_exit_hook_keeps_its_first_owner() {
        // A second install would let late wiring displace the flush that the
        // exit is about to run.
        let lc = lifecycle();
        let ran = Arc::new(AtomicBool::new(false));
        let first = ran.clone();
        lc.set_exit_hook(move || first.store(true, Ordering::Relaxed));
        lc.set_exit_hook(|| unreachable!("the second install must not take"));

        lc.on_exit.get().expect("a hook is installed")();
        assert!(
            ran.load(Ordering::Relaxed),
            "the first hook is the one kept"
        );
    }

    #[test]
    fn exit_code_is_one_without_shutdown() {
        assert_eq!(lifecycle().exit_code(), 1);
    }

    #[test]
    fn shutdown_closes_the_gate_before_its_handler_runs() {
        let lc = lifecycle();
        lc.initialized.store(true, Ordering::Relaxed);
        assert!(matches!(
            gate(&lc, &req("shutdown", Some(1))),
            Gate::Reply(_)
        ));
        let Gate::Reply(response) = gate(&lc, &req("tools/call", Some(2))) else {
            panic!("a pipelined request after shutdown must be answered here");
        };
        assert_eq!(response.error.unwrap().code, INVALID_REQUEST);
    }

    #[tokio::test]
    async fn oversize_line_is_drained_and_the_next_line_parses() {
        let big = "x".repeat(MAX_LINE_BYTES + 10);
        let input = format!("{big}\n{{\"jsonrpc\":\"2.0\"}}\n");
        let mut reader = BufReader::new(input.as_bytes());
        let mut raw = Vec::new();

        assert!(matches!(
            read_line(&mut reader, &mut raw).await.unwrap(),
            ReadLine::TooLong
        ));
        let ReadLine::Line(next) = read_line(&mut reader, &mut raw).await.unwrap() else {
            panic!("the line after an oversize one must still parse");
        };
        assert_eq!(next, "{\"jsonrpc\":\"2.0\"}");
    }

    #[tokio::test(start_paused = true)]
    async fn end_of_input_waits_for_an_accepted_request() {
        let lifecycle = Lifecycle::default();
        lifecycle.accept_request(PeerRequestId::Number(1));

        let mut deadline = None;
        let drain = std::pin::pin!(drain_in_flight(&lifecycle, &mut deadline));
        // Well past DRAIN_POLL, and with time paused it costs no wall clock.
        let waited = tokio::time::timeout(DRAIN_TIMEOUT / 2, drain).await;
        assert!(waited.is_err(), "a request still running holds the drain");

        lifecycle.retire_request(&PeerRequestId::Number(1));
        tokio::time::timeout(DRAIN_TIMEOUT, drain_in_flight(&lifecycle, &mut None))
            .await
            .expect("the drain returns once the response exists");
    }

    #[tokio::test]
    async fn a_reused_id_does_not_hold_the_drain_open() {
        // RMCP keeps one cancellation-token entry per request id, so a client
        // that reuses an id while the first request is still running is
        // answered once and RMCP discards the other response. Counting two owed
        // responses here would hold end of input open for the full drain
        // deadline waiting for a reply that no longer exists: measured at 30s
        // to exit instead of 0s. One entry per id is what RMCP will deliver.
        let lifecycle = Lifecycle::default();
        let id = PeerRequestId::Number(7);
        lifecycle.accept_request(id.clone());
        lifecycle.accept_request(id.clone());
        assert_eq!(lifecycle.outstanding(), 1, "one id, one response owed");

        lifecycle.retire_request(&id);
        assert_eq!(lifecycle.outstanding(), 0);
        tokio::time::timeout(DRAIN_TIMEOUT, drain_in_flight(&lifecycle, &mut None))
            .await
            .expect("the one response RMCP sends must release the drain");
    }

    #[test]
    fn retiring_a_request_twice_is_harmless() {
        // The double retire the set exists to absorb: a cancellation settles
        // the request, and a response already past RMCP's cancellation check
        // arrives afterwards and settles it again.
        let lifecycle = Lifecycle::default();
        let id = PeerRequestId::Number(1);
        lifecycle.accept_request(id.clone());
        lifecycle.retire_request(&id);
        lifecycle.retire_request(&id);
        lifecycle.retire_request(&PeerRequestId::Number(99));
        assert_eq!(lifecycle.outstanding(), 0);

        // Still usable afterwards rather than stuck at a wrapped count.
        lifecycle.accept_request(id);
        assert_eq!(lifecycle.outstanding(), 1);
    }

    #[test]
    fn distinct_ids_are_tracked_separately() {
        let lifecycle = Lifecycle::default();
        lifecycle.accept_request(PeerRequestId::Number(1));
        lifecycle.accept_request(PeerRequestId::Number(2));
        assert_eq!(lifecycle.outstanding(), 2);

        lifecycle.retire_request(&PeerRequestId::Number(2));
        assert_eq!(
            lifecycle.outstanding(),
            1,
            "answering one request must not settle the other"
        );
        lifecycle.retire_request(&PeerRequestId::Number(1));
        assert_eq!(lifecycle.outstanding(), 0);
    }

    #[tokio::test]
    async fn end_of_input_stops_when_stdout_fails() {
        let lifecycle = Lifecycle::default();
        lifecycle.accept_request(PeerRequestId::Number(1));

        let mut deadline = None;
        let mut drain = std::pin::pin!(drain_in_flight(&lifecycle, &mut deadline));
        let waited = tokio::time::timeout(Duration::from_millis(20), &mut drain).await;
        assert!(waited.is_err(), "an in-flight request starts the EOF drain");

        lifecycle.mark_write_failed();
        tokio::time::timeout(Duration::from_secs(1), drain)
            .await
            .expect("a failed stdout stops the EOF drain immediately");
    }

    #[tokio::test(start_paused = true)]
    async fn end_of_input_gives_up_on_a_request_that_never_finishes() {
        let lifecycle = Lifecycle::default();
        lifecycle.accept_request(PeerRequestId::Number(1));

        // Bounded, so a wedged handler cannot keep the process alive.
        tokio::time::timeout(DRAIN_TIMEOUT * 2, drain_in_flight(&lifecycle, &mut None))
            .await
            .expect("the drain gives up at DRAIN_TIMEOUT");
    }

    #[tokio::test]
    async fn a_final_frame_without_its_newline_is_still_answered() {
        // A caller that writes its last request and closes without a trailing
        // newline gets it answered. Nothing distinguishes that from a line
        // still being written except end of input, so it is only delivered once
        // the client has stopped sending.
        let (client, mut server) = tokio::io::duplex(64);
        let mut reader = BufReader::new(client);
        let mut raw = Vec::new();

        server.write_all(b"{\"jsonrpc\":\"2.0\"}").await.unwrap();
        drop(server);

        let ReadLine::Line(line) = read_line(&mut reader, &mut raw).await.unwrap() else {
            panic!("an unterminated final frame is a request, not a broken one");
        };
        assert_eq!(line, "{\"jsonrpc\":\"2.0\"}");
        // And end of input follows, so the loop still terminates.
        assert!(matches!(
            read_line(&mut reader, &mut raw).await.unwrap(),
            ReadLine::Eof
        ));
    }

    #[tokio::test]
    async fn a_line_at_exactly_the_limit_is_not_too_long() {
        // The limit is on the content, so the newline that terminates a
        // maximum-length line does not push it over.
        let (client, mut server) = tokio::io::duplex(MAX_LINE_BYTES + 64);
        let mut reader = BufReader::new(client);
        let mut raw = Vec::new();

        let body = "x".repeat(MAX_LINE_BYTES);
        tokio::spawn(async move {
            server.write_all(body.as_bytes()).await.unwrap();
            server.write_all(b"\n").await.unwrap();
        });

        let ReadLine::Line(line) = read_line(&mut reader, &mut raw).await.unwrap() else {
            panic!("a line of exactly MAX_LINE_BYTES fits");
        };
        assert_eq!(line.len(), MAX_LINE_BYTES);
    }

    #[tokio::test]
    async fn a_read_resumed_mid_character_still_decodes() {
        // raw outlives one call on purpose: RMCP polls receive inside a select!
        // and drops the read whenever another arm wins, which can land between
        // the bytes of one character. The decode runs once on the reassembled
        // buffer, so the halves have to be carried as bytes rather than decoded
        // apart and rejected as malformed.
        let mut raw = vec![0xE4, 0xBD]; // the first two bytes of 你
        let rest = [0xA0, b'\n'];
        let mut reader = BufReader::new(&rest[..]);

        let ReadLine::Line(line) = read_line(&mut reader, &mut raw).await.unwrap() else {
            panic!("the halves of one character must reassemble");
        };
        assert_eq!(line, "你");
        assert!(raw.is_empty(), "a delivered line must not stay buffered");
    }

    #[tokio::test]
    async fn a_cancelled_read_of_an_oversize_line_is_still_too_long() {
        // The resumed read has no budget left. That must report the line as
        // oversize, not mistake an empty read for end of input and hang up.
        let (client, mut server) = tokio::io::duplex(MAX_LINE_BYTES + 64);
        let mut reader = BufReader::new(client);
        let mut raw = vec![b'x'; MAX_LINE_BYTES + 1];

        server
            .write_all(b"tail-of-the-oversize-line\n{\"jsonrpc\":\"2.0\"}\n")
            .await
            .unwrap();
        assert!(matches!(
            read_line(&mut reader, &mut raw).await.unwrap(),
            ReadLine::TooLong
        ));

        let ReadLine::Line(next) = read_line(&mut reader, &mut raw).await.unwrap() else {
            panic!("the line after an oversize one still parses");
        };
        assert_eq!(next, "{\"jsonrpc\":\"2.0\"}");
    }

    #[tokio::test]
    async fn a_cancelled_read_resumes_instead_of_losing_the_line() {
        // RMCP polls receive inside a select!, so a read in progress is dropped
        // whenever a response becomes ready. The bytes already taken have to
        // survive that, or the client's request is split in two and never
        // answered. This is the failure that hung CI: it needs an outgoing
        // response to land mid-read, so it is timing-dependent in the server
        // and deterministic only here.
        let (client, mut server) = tokio::io::duplex(64);
        let mut reader = BufReader::new(client);
        let mut raw = Vec::new();

        server.write_all(b"{\"jsonrpc\":").await.unwrap();
        // Drop the read future mid-line, exactly as the select! would.
        let cancelled =
            tokio::time::timeout(Duration::from_millis(20), read_line(&mut reader, &mut raw)).await;
        assert!(cancelled.is_err(), "the read must still be waiting");
        assert!(!raw.is_empty(), "the bytes taken so far are kept");

        server.write_all(b"\"2.0\"}\n").await.unwrap();
        let ReadLine::Line(line) = read_line(&mut reader, &mut raw).await.unwrap() else {
            panic!("the resumed read must produce the whole line");
        };
        assert_eq!(line, "{\"jsonrpc\":\"2.0\"}");
        assert!(raw.is_empty(), "a consumed line leaves the buffer empty");
    }

    #[tokio::test]
    async fn malformed_utf8_is_reported_not_dropped() {
        let mut reader = BufReader::new(&b"\xff\xfe\n"[..]);
        let mut raw = Vec::new();
        assert!(matches!(
            read_line(&mut reader, &mut raw).await.unwrap(),
            ReadLine::MalformedUtf8
        ));
    }

    #[tokio::test]
    async fn empty_input_is_eof() {
        let mut reader = BufReader::new(&b""[..]);
        let mut raw = Vec::new();
        assert!(matches!(
            read_line(&mut reader, &mut raw).await.unwrap(),
            ReadLine::Eof
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn draining_after_the_queue_is_closed_returns_rather_than_waiting() {
        // The shape of the bug this has been bitten by: a drain that waits on a
        // writer which can no longer answer. With the sender taken there is
        // nothing to enqueue against, so it has to return, not block.
        let lifecycle = Lifecycle::default();
        let (outbound, _rx) = mpsc::unbounded_channel();
        lifecycle.set_outbound(outbound);
        lifecycle.close_outbound();

        tokio::time::timeout(Duration::from_secs(1), lifecycle.drain_outbound())
            .await
            .expect("a closed queue has nothing to wait for");
    }

    #[tokio::test(start_paused = true)]
    async fn draining_gives_up_rather_than_waiting_on_a_writer_that_cannot_write() {
        // Nothing consumes the queue here, which is what a client that has
        // stopped reading its end of the pipe looks like from in here.
        let lifecycle = Lifecycle::default();
        let (outbound, _rx) = mpsc::unbounded_channel();
        lifecycle.set_outbound(outbound);

        tokio::time::timeout(FLUSH_TIMEOUT * 2, lifecycle.drain_outbound())
            .await
            .expect("the drain is bounded, so termination does not depend on the client");
    }
}
