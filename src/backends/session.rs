use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Instant,
};

use crate::{
    model::{
        capability::{CapabilityOperation, CapabilityScope, Support},
        connection::UnavailableReason,
        identity::{BackendSessionId, ComponentId, EndpointId},
        observation::{DeviceObservation, Observation},
        topology::Topology,
    },
    transport::LeaseSet,
};

use super::{
    Backend, BackendCandidate, BackendDescriptor, BackendFuture, BatteryRead, CapabilityDelivery,
    ConnectContext, FirmwareRead, ObservationDelivery, PreparedRefresh, ProtocolDriver,
    ProtocolMessage, ReplyValue, RequestId, SessionOptions, UpdateStrategy,
    error::{BackendError, FailureKind, Phase, Result},
    events::{self, EventReceiver, Publisher},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionState {
    Connecting,
    Active,
    Disconnected,
    Failed,
    Shutdown,
}

pub(crate) struct Session {
    backend: String,
    updates: UpdateStrategy,
    context: ConnectContext,
    driver: Box<dyn ProtocolDriver>,
    leases: Option<LeaseSet>,
    state: SessionState,
    options: SessionOptions,
    publisher: Publisher,
    receiver: Option<EventReceiver>,
    next_request: u64,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("backend", &self.backend)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl Session {
    /// Leases and context are captured before the future is polled. Cancellation
    /// of even an unpolled negotiation therefore drops all transferred resources.
    pub(crate) fn connect(
        descriptor: &BackendDescriptor,
        candidate: BackendCandidate,
        context: ConnectContext,
        leases: LeaseSet,
        driver: Box<dyn ProtocolDriver>,
        options: SessionOptions,
    ) -> BackendFuture<'static, Self> {
        let backend = descriptor.id().to_owned();
        let updates = descriptor.updates();
        Box::pin(async move {
            let admission = (|| {
                if options.work_per_poll == 0
                    || options.max_message_bytes == 0
                    || options.max_endpoints == 0
                    || leases.iter().count() > options.max_endpoints
                {
                    return Err(BackendError::new(
                        FailureKind::InvalidInput,
                        "work/message/endpoint bounds must be nonzero and the lease count must fit",
                    ));
                }
                if candidate.backend() != backend
                    || candidate.endpoints().len() != leases.iter().count()
                    || !candidate
                        .endpoints()
                        .iter()
                        .all(|id| leases.iter().any(|lease| lease.endpoint() == *id))
                {
                    return Err(BackendError::new(
                        FailureKind::InvalidInput,
                        "candidate does not describe this backend and lease set",
                    ));
                }
                context.admit(&leases)?;
                if driver.requires_exclusive() {
                    leases.require_exclusive()?;
                }
                Ok(())
            })();
            admission.map_err(|error: BackendError| {
                error.context(&backend, Phase::Connect, None, None)
            })?;
            let (publisher, receiver) = events::channel(options.event_capacity, backend.clone())
                .map_err(|error| error.context(&backend, Phase::Connect, None, None))?;
            let session = Self {
                backend,
                updates,
                context,
                driver,
                leases: Some(leases),
                state: SessionState::Connecting,
                options,
                publisher,
                receiver: Some(receiver),
                next_request: 1,
            };
            ConnectFuture {
                session: Some(session),
            }
            .await
        })
    }

    pub(crate) fn id(&self) -> BackendSessionId {
        self.context.id()
    }
    pub(crate) fn state(&self) -> SessionState {
        self.state
    }
    pub(crate) fn updates(&self) -> UpdateStrategy {
        self.updates
    }
    pub(crate) fn context(&self) -> &ConnectContext {
        &self.context
    }

    fn active(&self) -> Result<()> {
        if self.state != SessionState::Active {
            return Err(self.error(FailureKind::Closed, "session is not active", Phase::Read));
        }
        Ok(())
    }
    fn error(&self, kind: FailureKind, detail: &str, phase: Phase) -> BackendError {
        BackendError::new(kind, detail).context(
            &self.backend,
            phase,
            None,
            Some(self.context.primary()),
        )
    }
    fn terminate(&mut self, state: SessionState, error: Option<BackendError>) {
        if matches!(self.state, SessionState::Active | SessionState::Connecting) {
            self.state = state;
            self.leases.take();
            self.publisher.finish(error);
        }
    }
    fn fail(&mut self, error: BackendError) -> BackendError {
        self.terminate(SessionState::Failed, Some(error.clone()));
        error
    }

    pub(crate) fn disconnect(&mut self) {
        self.terminate(SessionState::Disconnected, None);
    }
    pub(crate) fn shutdown(&mut self) {
        self.terminate(SessionState::Shutdown, None);
    }
    pub(crate) fn cancel(&mut self) {
        let error = self.error(
            FailureKind::Cancelled,
            "operation cancelled by the host",
            Phase::Shutdown,
        );
        self.terminate(SessionState::Failed, Some(error));
    }

    /// Host-side propagation only: losing a session is not proof of physical
    /// removal. Already-retired endpoints need no further availability update.
    pub(crate) fn close_with_topology(
        &mut self,
        topology: &mut Topology,
        reason: UnavailableReason,
    ) -> Result<()> {
        self.disconnect();
        for endpoint in self.context.endpoints() {
            if topology.endpoint(endpoint).is_ok() {
                topology
                    .invalidate_endpoint(endpoint, reason)
                    .map_err(|error| {
                        self.error(
                            FailureKind::StaleResult,
                            "failed to reconcile a closed endpoint",
                            Phase::Acceptance,
                        )
                        .caused_by(error)
                    })?;
            }
        }
        Ok(())
    }

    pub(crate) fn reconnect<'a>(
        &mut self,
        backend: &'a dyn Backend,
        candidate: BackendCandidate,
        context: ConnectContext,
        leases: LeaseSet,
        options: SessionOptions,
    ) -> BackendFuture<'a, Self> {
        if matches!(self.state, SessionState::Active | SessionState::Connecting) {
            let error = self.error(
                FailureKind::Busy,
                "disconnect and release old ownership before opening replacement leases",
                Phase::Connect,
            );
            return Box::pin(async move {
                drop(leases);
                Err(error)
            });
        }
        if !self.context.validates_replacement(&context) {
            let error = self.error(
                FailureKind::InvalidInput,
                "reconnect requires new session identity and fresh attachment generations",
                Phase::Connect,
            );
            return Box::pin(async move {
                drop(leases);
                Err(error)
            });
        }
        backend.connect(candidate, context, leases, options)
    }

    pub(crate) fn battery(&mut self) -> Option<&mut dyn BatteryRead> {
        if self.driver.battery().is_some() {
            Some(self)
        } else {
            None
        }
    }
    pub(crate) fn firmware(&mut self) -> Option<&mut dyn FirmwareRead> {
        if self.driver.firmware().is_some() {
            Some(self)
        } else {
            None
        }
    }
    pub(crate) fn take_events(&mut self) -> Result<EventReceiver> {
        self.active()?;
        self.receiver.take().ok_or_else(|| {
            self.error(
                FailureKind::Busy,
                "this session's single event receiver was already taken",
                Phase::Events,
            )
        })
    }

    fn event(
        &mut self,
        source: EndpointId,
        component: ComponentId,
        value: DeviceObservation,
    ) -> Result<()> {
        let route = self
            .context
            .component_source(component)
            .map_err(|error| error.context(&self.backend, Phase::Events, None, Some(source)))?;
        if route != source {
            return Err(BackendError::new(
                FailureKind::InvalidTarget,
                "unsolicited report used an unauthorized route",
            )
            .context(&self.backend, Phase::Events, None, Some(source)));
        }
        if observation_size(&value) > self.options.max_message_bytes {
            return Err(BackendError::new(
                FailureKind::ResourceLimit,
                "decoded event exceeds the message bound",
            )
            .context(&self.backend, Phase::Events, None, Some(source)));
        }
        self.publisher.publish(ObservationDelivery {
            session: self.id(),
            source,
            component,
            value,
        });
        Ok(())
    }

    /// The host drives idle input explicitly. No executor or worker is hidden in
    /// this method; a polling backend additionally needs host-scheduled reads.
    pub(crate) fn poll_drive(&mut self, cx: &mut Context<'_>) -> Poll<Result<()>> {
        self.active()?;
        for _ in 0..self.options.work_per_poll {
            match self.driver.poll_message(
                cx,
                self.leases.as_mut().expect("active leases"),
                self.options.max_message_bytes,
            ) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Ok(ProtocolMessage::Event {
                    source,
                    component,
                    value,
                })) => {
                    if let Err(error) = self.event(source, component, value) {
                        return Poll::Ready(Err(self.fail(error)));
                    }
                }
                Poll::Ready(Ok(ProtocolMessage::Reply { .. })) => {
                    let error = self.error(
                        FailureKind::Protocol,
                        "reply arrived without a pending request",
                        Phase::Events,
                    );
                    return Poll::Ready(Err(self.fail(error)));
                }
                Poll::Ready(Err(error)) => {
                    let error = error.context(&self.backend, Phase::Events, None, None);
                    return Poll::Ready(Err(self.fail(error)));
                }
            }
        }
        cx.waker().wake_by_ref();
        Poll::Pending
    }

    /// The caller must serialize this check with committing its own observed
    /// state. Receiving an old event must never mint a new topology token.
    pub(crate) fn accept_observation<'a>(
        &self,
        topology: &Topology,
        delivery: &'a ObservationDelivery,
    ) -> Result<&'a DeviceObservation> {
        self.active()?;
        if delivery.session != self.id()
            || self.context.component_source(delivery.component)? != delivery.source
        {
            return Err(self.error(
                FailureKind::StaleResult,
                "delivery belongs to another session or route",
                Phase::Acceptance,
            ));
        }
        self.context.validate_live(topology).map_err(|error| {
            error.context(
                &self.backend,
                Phase::Acceptance,
                None,
                Some(delivery.source),
            )
        })?;
        Ok(&delivery.value)
    }

    pub(crate) fn prepare_refresh(
        &self,
        topology: &mut Topology,
        scope: CapabilityScope,
    ) -> Result<PreparedRefresh> {
        self.active()?;
        self.context
            .validate_live(topology)
            .map_err(|error| error.context(&self.backend, Phase::Acceptance, None, None))?;
        let source = self.context.source_for(scope)?;
        let token = topology
            .begin_capability_refresh(scope, source)
            .map_err(|e| {
                self.error(
                    FailureKind::StaleResult,
                    "cannot start capability publication",
                    Phase::Acceptance,
                )
                .caused_by(e)
            })?;
        Ok(PreparedRefresh {
            session: self.id(),
            token,
        })
    }

    pub(crate) fn accept_capabilities(
        &self,
        topology: &mut Topology,
        prepared: PreparedRefresh,
        delivery: CapabilityDelivery,
    ) -> Result<()> {
        self.active()?;
        self.context.validate_live(topology).map_err(|error| {
            error.context(
                &self.backend,
                Phase::Acceptance,
                None,
                Some(delivery.source),
            )
        })?;
        if delivery.session != self.id()
            || prepared.session != self.id()
            || prepared.token.scope() != delivery.scope
            || prepared.token.source() != delivery.source
        {
            return Err(self.error(
                FailureKind::StaleResult,
                "capability delivery does not belong to the prepared refresh",
                Phase::Acceptance,
            ));
        }
        topology
            .complete_capability_refresh(prepared.token, delivery.report)
            .map_err(|e| {
                self.error(
                    FailureKind::StaleResult,
                    "capability refresh was superseded or invalidated",
                    Phase::Acceptance,
                )
                .caused_by(e)
            })
    }

    pub(crate) fn query_capabilities(
        &mut self,
        scope: CapabilityScope,
        deadline: Instant,
    ) -> BackendFuture<'_, CapabilityDelivery> {
        let session = self.id();
        Box::pin(async move {
            let (source, value) =
                RequestFuture::new(self, Request::Capabilities(scope), deadline).await?;
            let ReplyValue::Capabilities(report) = value else {
                unreachable!("validated reply kind");
            };
            Ok(CapabilityDelivery {
                session,
                source,
                scope,
                report,
            })
        })
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl BatteryRead for Session {
    fn read_battery(
        &mut self,
        component: ComponentId,
        deadline: Instant,
    ) -> BackendFuture<'_, ObservationDelivery> {
        let session = self.id();
        Box::pin(async move {
            let (source, value) =
                RequestFuture::new(self, Request::Battery(component), deadline).await?;
            let ReplyValue::Observation(value) = value else {
                unreachable!("validated reply kind");
            };
            Ok(ObservationDelivery {
                session,
                source,
                component,
                value,
            })
        })
    }
}
impl FirmwareRead for Session {
    fn read_firmware(
        &mut self,
        component: ComponentId,
        deadline: Instant,
    ) -> BackendFuture<'_, ObservationDelivery> {
        let session = self.id();
        Box::pin(async move {
            let (source, value) =
                RequestFuture::new(self, Request::Firmware(component), deadline).await?;
            let ReplyValue::Observation(value) = value else {
                unreachable!("validated reply kind");
            };
            Ok(ObservationDelivery {
                session,
                source,
                component,
                value,
            })
        })
    }
}

struct ConnectFuture {
    session: Option<Session>,
}
impl Future for ConnectFuture {
    type Output = Result<Session>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let session = this
            .session
            .as_mut()
            .expect("connect future polled after completion");
        if session.options.clock.now() >= session.options.connect_deadline {
            let error = session.error(
                FailureKind::DeadlineExceeded,
                "connection deadline elapsed",
                Phase::Connect,
            );
            return Poll::Ready(Err(session.fail(error)));
        }
        match session.driver.poll_connect(
            cx,
            session.leases.as_mut().expect("connecting leases"),
            session.options.max_message_bytes,
        ) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(error)) => {
                let error = error.context(&session.backend, Phase::Connect, None, None);
                Poll::Ready(Err(session.fail(error)))
            }
            Poll::Ready(Ok(())) => {
                if session.options.clock.now() >= session.options.connect_deadline {
                    let error = session.error(
                        FailureKind::DeadlineExceeded,
                        "connection completed after its deadline",
                        Phase::Connect,
                    );
                    return Poll::Ready(Err(session.fail(error)));
                }
                session.state = SessionState::Active;
                Poll::Ready(Ok(this.session.take().expect("connecting session")))
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Request {
    Battery(ComponentId),
    Firmware(ComponentId),
    Capabilities(CapabilityScope),
}
impl Request {
    fn scope(self) -> CapabilityScope {
        match self {
            Self::Battery(id) | Self::Firmware(id) => CapabilityScope::Component(id),
            Self::Capabilities(scope) => scope,
        }
    }
    fn operation(self) -> Option<CapabilityOperation> {
        match self {
            Self::Battery(_) => Some(CapabilityOperation::ReadBattery),
            Self::Firmware(_) => Some(CapabilityOperation::ReadFirmware),
            Self::Capabilities(_) => None,
        }
    }
    fn accepts(self, value: &ReplyValue) -> bool {
        matches!(
            (self, value),
            (
                Self::Battery(_),
                ReplyValue::Observation(DeviceObservation::Battery(_))
            ) | (
                Self::Firmware(_),
                ReplyValue::Observation(DeviceObservation::Firmware(_))
            ) | (Self::Capabilities(_), ReplyValue::Capabilities(_))
        )
    }
}

struct RequestFuture<'a> {
    session: &'a mut Session,
    request: Request,
    deadline: Instant,
    admitted: Option<RequestId>,
    done: bool,
}
impl<'a> RequestFuture<'a> {
    fn new(session: &'a mut Session, request: Request, deadline: Instant) -> Self {
        Self {
            session,
            request,
            deadline,
            admitted: None,
            done: false,
        }
    }
    fn admit(&mut self) -> Result<()> {
        let session = &mut self.session;
        session.active()?;
        session.context.source_for(self.request.scope())?;
        if let Some(operation) = self.request.operation() {
            let report = session.driver.capability_report(self.request.scope())?;
            match report.support(operation) {
                Support::Unknown => {
                    return Err(session.error(
                        FailureKind::SupportUnknown,
                        "operation support is not established",
                        Phase::Read,
                    ));
                }
                Support::Unsupported => {
                    return Err(session.error(
                        FailureKind::Unsupported,
                        "operation is unsupported for this scope",
                        Phase::Read,
                    ));
                }
                Support::Supported => {}
            }
            let implemented = match self.request {
                Request::Battery(_) => session.driver.battery().is_some(),
                Request::Firmware(_) => session.driver.firmware().is_some(),
                _ => true,
            };
            if !implemented {
                return Err(session.error(
                    FailureKind::Unsupported,
                    "protocol has no implementation of this read interface",
                    Phase::Read,
                ));
            }
        }
        let id = RequestId(session.next_request);
        session.next_request = session.next_request.checked_add(1).ok_or_else(|| {
            session.error(
                FailureKind::ResourceLimit,
                "request identities exhausted",
                Phase::Read,
            )
        })?;
        // Once queued, cancellation is terminal unless a future protocol proves
        // safe late-response draining/correlation. No resynchronization is guessed.
        self.admitted = Some(id);
        match self.request {
            Request::Battery(component) => session
                .driver
                .battery()
                .expect("checked interface")
                .queue_battery(id, component),
            Request::Firmware(component) => session
                .driver
                .firmware()
                .expect("checked interface")
                .queue_firmware(id, component),
            Request::Capabilities(scope) => session.driver.queue_capabilities(id, scope),
        }
    }
    fn contextual(&self, error: BackendError) -> BackendError {
        error.context(
            &self.session.backend,
            if self.request.operation().is_some() {
                Phase::Read
            } else {
                Phase::Capabilities
            },
            self.request.operation(),
            self.session.context.source_for(self.request.scope()).ok(),
        )
    }
    fn fail(&mut self, error: BackendError) -> Poll<Result<(EndpointId, ReplyValue)>> {
        self.done = true;
        let error = self.contextual(error);
        if self.admitted.is_some() {
            Poll::Ready(Err(self.session.fail(error)))
        } else {
            Poll::Ready(Err(error))
        }
    }
}

impl Future for RequestFuture<'_> {
    type Output = Result<(EndpointId, ReplyValue)>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        if this.session.options.clock.now() >= this.deadline {
            return this.fail(BackendError::new(
                FailureKind::DeadlineExceeded,
                "request deadline elapsed; the host must schedule deadline wakeups",
            ));
        }
        if this.admitted.is_none()
            && let Err(error) = this.admit()
        {
            return this.fail(error);
        }
        for _ in 0..this.session.options.work_per_poll {
            // Metadata validation/queueing and preceding unsolicited reports may
            // consume time within one poll; check again before touching the I/O.
            if this.session.options.clock.now() >= this.deadline {
                return this.fail(BackendError::new(
                    FailureKind::DeadlineExceeded,
                    "request deadline elapsed before I/O progress",
                ));
            }
            let session = &mut this.session;
            let message = session.driver.poll_message(
                cx,
                session.leases.as_mut().expect("active leases"),
                session.options.max_message_bytes,
            );
            if session.options.clock.now() >= this.deadline {
                return this.fail(BackendError::new(
                    FailureKind::DeadlineExceeded,
                    "request completed after its deadline",
                ));
            }
            match message {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(error)) => return this.fail(error),
                Poll::Ready(Ok(ProtocolMessage::Event {
                    source,
                    component,
                    value,
                })) => {
                    if let Err(error) = session.event(source, component, value) {
                        return this.fail(error);
                    }
                }
                Poll::Ready(Ok(ProtocolMessage::Reply {
                    request,
                    source,
                    value,
                })) => {
                    if Some(request) != this.admitted
                        || session.context.source_for(this.request.scope()).ok() != Some(source)
                        || !this.request.accepts(&value)
                    {
                        return this.fail(BackendError::new(
                            FailureKind::Protocol,
                            "reply does not match the admitted request, route or payload kind",
                        ));
                    }
                    if reply_size(&value) > session.options.max_message_bytes {
                        return this.fail(BackendError::new(
                            FailureKind::ResourceLimit,
                            "decoded reply exceeds the message bound",
                        ));
                    }
                    this.done = true;
                    return Poll::Ready(Ok((source, value)));
                }
            }
        }
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

impl Drop for RequestFuture<'_> {
    fn drop(&mut self) {
        if self.admitted.is_some() && !self.done {
            self.session.cancel();
        }
    }
}

fn observation_size(value: &DeviceObservation) -> usize {
    match value {
        DeviceObservation::Battery(_) => 2,
        DeviceObservation::Firmware(Observation::Known(info)) => {
            info.versions().iter().fold(0usize, |size, version| {
                size.saturating_add(version.namespace().len())
                    .saturating_add(version.value().len())
            })
        }
        DeviceObservation::Firmware(_) => 0,
    }
}
fn reply_size(value: &ReplyValue) -> usize {
    match value {
        ReplyValue::Observation(value) => observation_size(value),
        ReplyValue::Capabilities(report) => {
            let versions =
                report
                    .versions()
                    .iter()
                    .fold(report.reporter().len(), |size, version| {
                        size.saturating_add(version.namespace().len())
                            .saturating_add(version.value().len())
                    });
            report.operations().fold(versions, |size, (_, entry)| {
                entry
                    .constraints()
                    .iter()
                    .fold(size.saturating_add(1), |size, constraint| {
                        size.saturating_add(constraint.namespace().len())
                            .saturating_add(constraint.expression().len())
                    })
            })
        }
    }
}
