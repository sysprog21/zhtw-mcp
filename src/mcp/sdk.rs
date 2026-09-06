//! RMCP SDK adapter.
//!
//! The `ServerHandler` RMCP dispatches to. Handlers here do three things and
//! no more: run the matching `tools.rs` method on the blocking pool, carry its
//! error across to RMCP's shape, and flush whatever it logged. Everything the
//! wire sees is built in `tools.rs` as RMCP model types, so this module holds
//! no wire format of its own.
//!
//! Six `#[allow(deprecated)]` sit in here, and they are one decision rather
//! than six. MCP SEP-2577 deprecates sampling, logging notifications, and
//! roots; rmcp marked them deprecated in 1.8.0 and its notes name no successor,
//! because the capabilities are being removed rather than renamed. The clients
//! this server serves still use sampling and logging, so dropping them would be
//! a silent feature removal, and the allows are what keeps a warning-free build
//! while they stay wired.
//!
//! The exit condition is not a date. rmcp is pinned to major 3 in `Cargo.toml`,
//! and the release that removes these will not be a 3.x, so the next major bump
//! is where this gets revisited: either the clients have moved by then, or the
//! bump waits until they have.

use std::sync::{Arc, Mutex, PoisonError};

#[allow(deprecated)]
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CompleteRequestParams, CompleteResult,
    CustomNotification, CustomRequest, CustomResult, ErrorCode, GetPromptRequestParams,
    GetPromptResponse, Implementation, InitializeRequestParams, InitializeResult,
    ListPromptsResult, ListResourceTemplatesResult, ListResourcesResult, ListToolsResult,
    LoggingMessageNotificationParam, PaginatedRequestParams, ProtocolVersion,
    ReadResourceRequestParams, ReadResourceResponse, ServerCapabilities, ServerInfo,
    SetLevelRequestParams,
};
use rmcp::service::{NotificationContext, Peer, RequestContext, RoleServer};
use rmcp::{ErrorData, ServerHandler};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use super::revisions::{
    is_handshake_free, negotiable_protocol_versions, supported_protocol_versions,
};
use super::sampling::{
    PeerSampler, SamplingBridge, DEFAULT_SAMPLING_BUDGET, DEFAULT_SAMPLING_TIMEOUT,
};
use super::tools::{ParamResult, Server};
use super::transport::Lifecycle;

/// Whether the judgment cache was written on the way out.
#[derive(Debug, PartialEq, Eq)]
enum Flushed {
    Yes,
    /// A scan is still running and holds the lock. Exit is unconditional, so
    /// a contended lock costs the flush, not the exit.
    SkippedForScanInFlight,
}

/// Flush the judgment cache before the process goes.
///
/// `process::exit` skips `Drop`, so this is the only chance to write it.
///
/// A poisoned lock is not a contended one. A handler that panicked left the
/// flag set, and reading any lock error as "a scan is running" meant every
/// later exit skipped the flush for a scan that was not there, losing the
/// session's judgments. Poisoning is stepped over here the way it is at
/// every other lock site in this module.
fn flush_before_exit(inner: &Mutex<Server>) -> Flushed {
    match inner.try_lock() {
        Ok(mut server) => {
            server.flush_judgment_cache();
            Flushed::Yes
        }
        Err(std::sync::TryLockError::Poisoned(poisoned)) => {
            poisoned.into_inner().flush_judgment_cache();
            Flushed::Yes
        }
        Err(std::sync::TryLockError::WouldBlock) => {
            tracing::warn!("exiting without flushing the judgment cache: scan in flight");
            Flushed::SkippedForScanInFlight
        }
    }
}

/// The methods this server implements, for telling a request it cannot serve
/// from one it can serve but whose parameters did not parse. Kept next to the
/// handlers rather than derived from the SDK, which has no list of what a
/// given server actually implements.
///
/// A method is here only if it is actually served. `completion/complete` is
/// not: the `completions` capability is unadvertised and the handler refuses
/// it, so listing it here would have made the same method answer
/// method-not-found with good parameters and invalid-params with bad ones.
const IMPLEMENTED_METHODS: &[&str] = &[
    "tools/call",
    "tools/list",
    "resources/list",
    "resources/read",
    "resources/templates/list",
    "prompts/list",
    "prompts/get",
    "logging/setLevel",
    "initialize",
    "server/discover",
    "ping",
];

/// The -32603 returned when a request handler panics.
///
/// `spawn_blocking` catches the unwind, the lock guard is released on the way
/// out, and the poison flag is ignored on the next lock, so one bad request
/// costs a response rather than the server.
fn handler_panicked() -> ErrorData {
    tracing::error!("request handler panicked");
    ErrorData::internal_error("internal error: request handler panicked", None)
}

pub struct SdkServer {
    inner: Arc<Mutex<Server>>,
    /// The immutable half of the server, so the handlers that only read it
    /// answer while a lint holds the lock instead of queueing behind it.
    catalog: Arc<super::tools::Catalog>,
    lifecycle: Arc<Lifecycle>,
}

impl SdkServer {
    pub fn new(inner: Server) -> Self {
        let catalog = inner.catalog();
        let inner = Arc::new(Mutex::new(inner));
        let lifecycle = Arc::new(Lifecycle::default());

        // The judgment cache is this layer's, and flushing it is the only thing
        // the framing layer cannot do for itself. Handing it over here is what
        // lets exit have a single owner: the transport terminates, and this
        // still gets written.
        let flushing = inner.clone();
        lifecycle.set_exit_hook(move || {
            flush_before_exit(&flushing);
        });

        Self {
            catalog,
            inner,
            lifecycle,
        }
    }

    /// The state the transport shares with this handler.
    pub fn lifecycle(&self) -> Arc<Lifecycle> {
        self.lifecycle.clone()
    }

    /// Send pending tracing output as `notifications/message`, before the
    /// response that produced it, which is the order clients read causality
    /// from.
    #[allow(deprecated)]
    async fn flush_logs(&self, ctx: &RequestContext<RoleServer>) {
        // A handshake declared the opt-in once for the connection, so the peer
        // is a session RMCP knows about and can carry a notification.
        if self.lifecycle.initialized() {
            for message in self.lifecycle.drain_logs() {
                // Built directly rather than round-tripped through a Value,
                // which rebuilt the whole data tree twice per line and dropped
                // the message outright if it ever failed to fit. The level
                // arrives already typed, so nothing has to decide what it means
                // here.
                let param = LoggingMessageNotificationParam::new(message.level, message.data)
                    .with_logger(message.logger);
                let _ = ctx.peer.notify_logging_message(param).await;
            }
            return;
        }
        if super::revisions::logging_opt_in(&ctx.meta) {
            // Framed onto the transport's own queue rather than sent through
            // the peer. RMCP never saw an initialize on this connection, and a
            // notification handed to that peer waits on a session that will
            // never open: the send is accepted and the future that resolves
            // when it reaches the wire never does, which parks the request
            // forever with no reply and no exit. The queue is the same wire
            // without the handshake state machine in front of it.
            self.lifecycle.queue_logs();
            return;
        }

        // Declared nothing, so it is owed nothing. Drained either way, because
        // the opt-in is request-scoped and lines held over would be delivered
        // to whichever later request happened to ask.
        //
        // Capture is process-wide and carries no request identity, so this
        // drops whatever a concurrent opted-in request logged and has not
        // flushed yet. Holding the lines back instead only moves the damage:
        // two overlapping requests that declared nothing would each see the
        // other still running and neither would ever drain, and the pile would
        // go out under the next request that did ask, as its own lines. Both
        // ends of that trade need the one thing the channel does not carry,
        // which is who logged each line. Tagging them means threading a request
        // id through the blocking pool and rayon into the trace layer; the loss
        // here is log lines on a connection that pipelines mixed opt-ins, and
        // it has not been worth that.
        self.lifecycle.drain_logs();
    }

    /// Run one read-only handler, flushing whatever it logged first.
    ///
    /// This goes to the blocking pool for the same reason `call_tool` does,
    /// and it is not an optimization: a lint holds the server lock across its
    /// sampling round trip, so taking that lock on the runtime thread would
    /// stall the very loop that has to answer the sampling request, and the
    /// deadline with it. Parking a blocking-pool thread instead leaves the
    /// runtime free.
    async fn on_server<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Server) -> T + Send + 'static,
    ) -> Result<T, ErrorData> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            f(&mut inner.lock().unwrap_or_else(PoisonError::into_inner))
        })
        .await
        .map_err(|_| handler_panicked())
    }

    /// Record the handshake client's identity.
    ///
    /// Both entry points feed this: `initialize`, and `discover` for a client
    /// that skips the handshake and declares itself in per-request `_meta`
    /// instead. Capabilities are read from each request context so inline
    /// sessions cannot leak them between requests.
    ///
    /// Off the runtime thread for the same reason every other handler is: a
    /// re-declaration while a lint waits on sampling must not stall the loop
    /// that has to answer it.
    async fn record_client(&self, name: String) -> Result<(), ErrorData> {
        self.on_server(move |server| server.set_client(name)).await
    }

    /// Answer from data that needs no lock, flushing whatever it logged.
    async fn answer<T>(
        &self,
        ctx: &RequestContext<RoleServer>,
        result: ParamResult<T>,
    ) -> Result<T, ErrorData> {
        self.flush_logs(ctx).await;
        result
    }
}

/// One `sampling/createMessage` in flight: the params to send, and where the
/// reply text goes.
type SamplingCall = (Value, oneshot::Sender<Option<String>>);

/// Sampling as the blocking pipeline sees it: hand the params to the runtime
/// and block until the peer answers.
struct ChannelSampler {
    tx: mpsc::Sender<SamplingCall>,
}

impl PeerSampler for ChannelSampler {
    fn create_message(&mut self, params: Value) -> Option<String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx.blocking_send((params, reply_tx)).ok()?;
        reply_rx.blocking_recv().ok().flatten()
    }
}

/// Ask the client to sample, and pull the reply text out of the result.
///
/// SEP-2577 deprecates sampling in the SDK; the clients this server serves
/// still use it, and dropping it would be a silent feature removal.
#[allow(deprecated)]
async fn create_message(peer: &Peer<RoleServer>, params: Value) -> Option<String> {
    let params = serde_json::from_value(params)
        .inspect_err(|e| tracing::warn!("sampling params rejected: {e}"))
        .ok()?;
    let result = tokio::time::timeout(DEFAULT_SAMPLING_TIMEOUT, peer.create_message(params))
        .await
        .map_err(|_| tracing::warn!("sampling request timed out"))
        .ok()?
        .inspect_err(|e| tracing::warn!("sampling request failed: {e}"))
        .ok()?;
    reply_text(result)
}

/// The usable text in a client's sampling reply.
///
/// A client may answer with one content block or several, and only text is
/// useful here; a blank answer is no answer. `pub(crate)` so the sampling
/// tests script replies through the same rule the server applies rather than
/// a copy of it.
#[allow(deprecated)]
pub(crate) fn reply_text(result: rmcp::model::CreateMessageResult) -> Option<String> {
    let text = result
        .message
        .content
        .into_vec()
        .iter()
        .filter_map(|block| block.as_text())
        .map(|text| text.text.trim())
        .find(|text| !text.is_empty())?
        .to_owned();
    Some(text)
}

// SEP-2577 deprecates the logging and sampling APIs in the SDK. This server's
// clients still use both, so they stay wired until the clients move.
#[allow(deprecated)]
impl ServerHandler for SdkServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_prompts()
                .enable_logging()
                .build(),
        )
        .with_protocol_version(ProtocolVersion::V_2024_11_05)
        .with_server_info(Implementation::new("zhtw-mcp", env!("CARGO_PKG_VERSION")))
    }

    /// The revisions this server serves, newest first.
    ///
    /// Listed rather than taken from the SDK's `KNOWN_VERSIONS` so that an
    /// upgrade adding a revision is a decision rather than a silent claim.
    ///
    /// 2026-07-28 earns its place on the list rather than inheriting it. That
    /// revision has no `initialize` at all: `server/discover` is the entry
    /// point and the client declares itself in per-request `_meta`, which is
    /// the lifecycle this server implements and drives in its tests. It
    /// requires `ttlMs` and `cacheScope` on every list and read result, which
    /// this server sets. It deprecates sampling but keeps it in the
    /// specification for at least twelve months, so the Tier 3 path stays
    /// valid under it. The extensions it adds beyond that (tasks,
    /// `subscriptions/listen`) are capability-gated and unadvertised here.
    ///
    /// The older revisions negotiate through `initialize`, which RMCP handles,
    /// and share the same tool, resource, and prompt surface.
    fn supported_protocol_versions(&self) -> std::borrow::Cow<'static, [ProtocolVersion]> {
        std::borrow::Cow::Borrowed(supported_protocol_versions())
    }

    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, ErrorData> {
        // A revision served but without a handshake is not an unsupported
        // version, it is the wrong entry point, and saying so beats the generic
        // refusal: that one's list of alternatives cannot include the version
        // actually wanted.
        if is_handshake_free(request.protocol_version.as_str()) {
            tracing::warn!(
                requested = %request.protocol_version,
                "initialize named a revision that has no handshake"
            );
            return Err(ErrorData::new(
                ErrorCode::UNSUPPORTED_PROTOCOL_VERSION,
                format!(
                    "Protocol version {} has no initialize; \
                     it is served through server/discover",
                    request.protocol_version
                ),
                Some(serde_json::json!({
                    "requested": request.protocol_version,
                    "supported": negotiable_protocol_versions(),
                    "entryPoint": "server/discover",
                })),
            ));
        }

        // A revision this server does not serve is refused, not quietly
        // downgraded: since 2026-07-28 the spec makes an unsupported version a
        // server error rather than a client's judgment call, and answering a
        // different version than the one asked for leaves the client guessing
        // which of the two is in force. The reply names what was requested and
        // what is on offer so the client can pick.
        if !negotiable_protocol_versions().contains(&request.protocol_version) {
            tracing::warn!(
                requested = %request.protocol_version,
                "rejecting unsupported protocol version"
            );
            return Err(ErrorData::unsupported_protocol_version(
                request.protocol_version,
                negotiable_protocol_versions(),
            ));
        }
        self.record_client(request.client_info.name.to_string())
            .await?;
        let negotiated = request.protocol_version.clone();
        context.peer.set_peer_info(request);
        self.lifecycle.mark_initialized();

        // Answered with the revision asked for, set here rather than left to
        // the service: RMCP patches the negotiated version onto the result only
        // when the session began with this handshake. A session that opened
        // with server/discover and then sent initialize takes a different path,
        // and the reply went out naming get_info's default instead of the
        // version requested. That is the same "which of the two is in force?"
        // the refusal above exists to avoid. The version is already checked
        // against the negotiable list, so there is nothing further to validate.
        Ok(self.get_info().with_protocol_version(negotiated))
    }

    /// Discovery works before a handshake. 2026-07-28 has no `initialize`, so
    /// its declaration rides in per-request `_meta`.
    ///
    /// Answering does not open the post-handshake gate. A client that asks
    /// what this server speaks has not yet said what it will speak, and on
    /// this revision it never will: the declaration arrives with each request
    /// instead.
    ///
    /// Nothing about the caller is recorded here either. Everything this
    /// revision declares, identity included, is request-scoped and read where
    /// it is used: `call_tool` takes the name off its own `_meta`, and a
    /// discovery probe naming a client is not consent for a later request that
    /// named nobody to be answered as that client. A handshake still records
    /// one, because there the declaration really is per connection.
    async fn discover(
        &self,
        _context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::DiscoverResult, ErrorData> {
        Ok(rmcp::model::DiscoverResult::from_server_info(
            self.supported_protocol_versions().into_owned(),
            self.get_info(),
        ))
    }

    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        self.answer(&ctx, Ok(super::tools::list_tools())).await
    }

    async fn call_tool(
        &self,
        params: CallToolRequestParams,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let name = params.name.to_string();
        let arguments = Value::Object(params.arguments.unwrap_or_default());
        let sampling = ctx
            .client_capabilities()
            .is_some_and(|capabilities| capabilities.sampling.is_some());

        // The lint pipeline is synchronous and CPU-bound, so it runs on the
        // blocking pool: keeping it off the runtime thread is what lets pings,
        // cancellations, and this call's own sampling round trips be serviced
        // while a scan is in flight.
        let inner = self.inner.clone();

        // 2026-07-28 has no handshake to record the client at, so the identity
        // rides in this request's _meta instead: it picks the default output
        // mode, and a call that never learns who is asking answers an agent
        // client in full where it had been answering compact. Handed to the
        // call rather than stored on the server, because the declaration is
        // scoped to this request and must not decide the mode for a later one
        // that declared nothing.
        let declared_client = ctx.meta.client_info().map(|info| info.name);
        let (sampling_tx, mut sampling_rx) = mpsc::channel::<SamplingCall>(1);
        let scan = tokio::task::spawn_blocking(move || {
            let mut server = inner.lock().unwrap_or_else(PoisonError::into_inner);
            let mut sampler = ChannelSampler { tx: sampling_tx };
            let mut bridge =
                sampling.then(|| SamplingBridge::new(&mut sampler, DEFAULT_SAMPLING_BUDGET));
            server.call_tool(
                &name,
                &arguments,
                bridge.as_mut(),
                declared_client.as_deref(),
            )
        });

        // The scan owns the sending half, so the channel closing is how this
        // learns the scan is done, whether it returned or unwound.
        while let Some((params, reply)) = sampling_rx.recv().await {
            // A cancelled request is not owed an answer, and the client that
            // cancelled it is not going to send one. Waiting out the sampling
            // deadline anyway costs five seconds per question, up to the whole
            // budget, with the server lock held the entire time. Answering the
            // bridge with nothing lets the scan finish and let go of it.
            let answer = tokio::select! {
                biased;
                () = ctx.ct.cancelled() => None,
                answer = create_message(&ctx.peer, params) => answer,
            };
            let _ = reply.send(answer);
        }
        let response = scan.await;

        self.flush_logs(&ctx).await;
        // A panic in the pipeline costs this request, not the connection.
        let response = response.map_err(|_| handler_panicked())?;
        response.map(Into::into)
    }

    /// `logging/setLevel` is the spec's way to ask for what this server's
    /// clients have historically asked for with a `logging` capability.
    async fn set_level(
        &self,
        params: SetLevelRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        self.lifecycle.enable_logs();

        // The level is the point of the request. Turning forwarding on and
        // ignoring which level was asked for sends a client that wants errors
        // every info notification this server produces.
        self.lifecycle.set_log_level(params.level);
        Ok(())
    }

    async fn list_resources(
        &self,
        _: Option<PaginatedRequestParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        self.answer(&ctx, Ok(super::tools::list_resources())).await
    }

    async fn read_resource(
        &self,
        params: ReadResourceRequestParams,
        ctx: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        self.answer(&ctx, self.catalog.read_resource(&params.uri))
            .await
            .map(Into::into)
    }

    async fn list_prompts(
        &self,
        _: Option<PaginatedRequestParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        self.answer(&ctx, Ok(super::tools::list_prompts())).await
    }

    async fn get_prompt(
        &self,
        params: GetPromptRequestParams,
        ctx: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, ErrorData> {
        // Only string arguments reach the prompt templates; anything else the
        // client sends is not something a template can substitute.
        let arguments = params
            .arguments
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(key, value)| value.as_str().map(|value| (key, value.to_owned())))
            .collect();
        self.answer(&ctx, super::tools::get_prompt(&params.name, &arguments))
            .await
            .map(Into::into)
    }

    /// No templates, which is not the same as no such method.
    ///
    /// `resources/templates/list` is a standard request under the `resources`
    /// capability this server advertises, and 2026-07-28 lists it among the
    /// ten a client may send. Answering METHOD_NOT_FOUND would deny a method
    /// the advertised capability promises; the accurate answer is that the
    /// list is empty. `completion/complete` is different and stays refused:
    /// that one is gated on a `completions` capability this server does not
    /// advertise.
    async fn list_resource_templates(
        &self,
        _: Option<PaginatedRequestParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        self.answer(&ctx, Ok(super::tools::list_resource_templates()))
            .await
    }

    /// Not implemented. An empty completion result reads as "supported, no
    /// matches", which is worse than saying so.
    async fn complete(
        &self,
        _: CompleteRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<CompleteResult, ErrorData> {
        Err(ErrorData::new(
            ErrorCode::METHOD_NOT_FOUND,
            "completion/complete",
            None,
        ))
    }

    /// No custom request reaches this server: `shutdown`, the only one its
    /// clients send, is answered by the framing layer.
    async fn on_custom_request(
        &self,
        request: CustomRequest,
        _: RequestContext<RoleServer>,
    ) -> Result<CustomResult, ErrorData> {
        // shutdown never reaches here: the framing layer answers it, so the
        // flag it sets is in place before the next envelope is read.
        //
        // Anything else landing here is either a method this server does not
        // have, or one it does have whose params failed to deserialize:
        // ClientRequest is untagged with CustomRequest last, so a tools/call
        // whose params are the wrong shape falls through to here rather than
        // being rejected as a typed request. Answering METHOD_NOT_FOUND for the
        // second kind tells a client its tool does not exist when the tool
        // exists and the arguments are wrong.
        if IMPLEMENTED_METHODS.contains(&request.method.as_ref()) {
            return Err(ErrorData::invalid_params(
                format!("invalid parameters for {}", request.method),
                None,
            ));
        }
        Err(ErrorData::new(
            ErrorCode::METHOD_NOT_FOUND,
            request.method,
            None,
        ))
    }

    /// No custom notification reaches this server either.
    ///
    /// `exit` is the only one its clients send, and the framing layer honors
    /// it before RMCP is handed the envelope; the `Gate::Exit` arm in
    /// `transport::StdioTransport::receive` says why. Kept rather than left to
    /// RMCP's no-op default so an unknown notification is at least logged.
    async fn on_custom_notification(
        &self,
        notification: CustomNotification,
        _: NotificationContext<RoleServer>,
    ) {
        tracing::debug!("unhandled notification: {}", notification.method);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_server() -> (Mutex<Server>, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("temp dir");
        let server = Server::new(
            crate::rules::store::OverrideStore::open(&dir.path().join("overrides.json")).unwrap(),
            crate::rules::store::SuppressionStore::open(&dir.path().join("suppressions.json"))
                .unwrap(),
            crate::rules::store::PackStore::new(dir.path().join("packs")),
            vec![],
            None,
        )
        .expect("build server");
        (Mutex::new(server), dir)
    }

    #[test]
    fn the_server_wires_its_cache_flush_into_the_exit() {
        // exit terminates in the framing layer now, and that layer cannot reach
        // the judgment cache. Losing this wiring costs the cache on every clean
        // exit and fails nothing else, which is why it is asserted here rather
        // than left to an end-to-end test that would still pass.
        let (server, _dir) = test_server();
        let sdk = SdkServer::new(server.into_inner().expect("unpoisoned"));
        assert!(
            sdk.lifecycle().exit_hook_installed(),
            "new must hand the flush to the lifecycle"
        );
    }

    /// Poison a lock the way a panicking handler does.
    fn poison(inner: &Mutex<Server>) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = inner.lock().unwrap();
            panic!("handler panicked");
        }));
        assert!(inner.is_poisoned(), "the lock should now be poisoned");
    }

    #[test]
    fn a_panicked_handler_does_not_cost_the_judgment_cache() {
        // try_lock reports a poisoned lock as an error even when nothing holds
        // it, so reading every error as "a scan is in flight" threw the flush
        // away for the rest of the process once any handler panicked.
        let (inner, _dir) = test_server();
        poison(&inner);
        assert_eq!(flush_before_exit(&inner), Flushed::Yes);
    }

    #[test]
    fn a_scan_in_flight_is_left_to_finish_without_the_flush() {
        // The other half, and it is about contention alone: a lock genuinely
        // held is what the warning is for. Poisoning it as well would pass only
        // because try_lock reports contention ahead of poison.
        let (inner, _dir) = test_server();
        let _held = inner.lock().expect("a fresh lock is not poisoned");
        assert_eq!(flush_before_exit(&inner), Flushed::SkippedForScanInFlight);
    }

    #[test]
    fn an_unadvertised_method_is_not_listed_as_implemented() {
        // The list is maintained by hand, and this is the entry that costs
        // something when it drifts. completion/complete is refused because the
        // completions capability is unadvertised; listing it here would make
        // the same method answer method-not-found on good parameters and
        // invalid-params on bad ones, which is the bug the list exists to
        // prevent rather than one it should introduce.
        assert!(
            !IMPLEMENTED_METHODS.contains(&"completion/complete"),
            "completion/complete is refused, so it must not count as implemented"
        );
    }

    #[test]
    fn implemented_methods_has_no_duplicates() {
        // A duplicate is invisible at the call site (contains() still says
        // true) but means someone edited the list twice for one method, which
        // is the state where the next edit removes only one of them.
        let unique: std::collections::BTreeSet<_> = IMPLEMENTED_METHODS.iter().collect();
        assert_eq!(
            unique.len(),
            IMPLEMENTED_METHODS.len(),
            "duplicate entry in IMPLEMENTED_METHODS"
        );
    }

    /// A client reply carrying the given text blocks, in order.
    ///
    /// Written as the wire payload and parsed back, the same way the sampling
    /// tests script their canned replies: a shape a real client could send is
    /// then a shape this test can build.
    // Deprecated by SEP-2577 along with the sampling API this exercises; the
    // allow matches the one on reply_text itself.
    #[allow(deprecated)]
    fn sampling_reply(blocks: &[&str]) -> rmcp::model::CreateMessageResult {
        let content: Vec<_> = blocks
            .iter()
            .map(|t| serde_json::json!({"type": "text", "text": t}))
            .collect();
        serde_json::from_value(serde_json::json!({
            "role": "assistant",
            "model": "test-model",
            "content": content,
        }))
        .expect("a CreateMessageResult payload")
    }

    #[test]
    fn a_sampling_reply_yields_its_first_non_blank_text() {
        // The model is free to lead with an empty or whitespace-only block, and
        // taking it would hand the caller "" as though that were the judgment.
        // First block with something in it wins, trimmed.
        assert_eq!(
            reply_text(sampling_reply(&["  ", "", "  軟體  ", "檔案"])),
            Some("軟體".to_string())
        );
    }

    #[test]
    fn a_sampling_reply_with_nothing_to_say_is_none() {
        assert_eq!(reply_text(sampling_reply(&[])), None);
        assert_eq!(reply_text(sampling_reply(&["", "   ", "\n\t"])), None);
    }
}
