use std::sync::{Arc, Mutex};

use super::*;
use crate::model::{
    connection::{ConnectionState, EndpointLocator},
    identity::{HardwareIdentity, IdentityAbsence},
    topology::{EndpointSpec, PhysicalDeviceSpec, Topology},
};

struct ProbeIo {
    kind: TransportKind,
    log: Arc<Mutex<Vec<String>>>,
    excess: bool,
}
impl TransportIo for ProbeIo {
    fn kind(&self) -> TransportKind {
        self.kind
    }
    fn poll_io(&mut self, _: &mut Context<'_>, request: IoRequest<'_>) -> Poll<Result<usize>> {
        self.log.lock().unwrap().push("io".into());
        let size = request.capacity();
        if let IoRequest::HidGetFeature { report, data } = request {
            self.log.lock().unwrap().push(format!("feature:{report}"));
            data.fill(7);
        }
        Poll::Ready(Ok(size + usize::from(self.excess)))
    }
    fn close(&mut self) {
        self.log.lock().unwrap().push("close".into());
    }
}
struct Reservation(Arc<Mutex<Vec<String>>>);
impl OwnershipReservation for Reservation {}
impl Drop for Reservation {
    fn drop(&mut self) {
        self.0.lock().unwrap().push("reservation".into());
    }
}

fn attachment(kind: TransportKind) -> (EndpointId, AttachmentGeneration) {
    let mut topology = Topology::new();
    let physical = topology
        .add_physical_device(
            None,
            PhysicalDeviceSpec {
                name: None,
                identity: HardwareIdentity::missing(IdentityAbsence::NotReported),
                usb_ids: None,
                serial: None,
                interfaces: vec![],
                endpoints: vec![EndpointSpec {
                    interface: None,
                    transport: kind,
                    locator: EndpointLocator::new("synthetic").unwrap(),
                    hid: Default::default(),
                }],
                components: vec![],
            },
        )
        .unwrap();
    let endpoint = topology.endpoints(physical).next().unwrap().id();
    topology.revalidate_endpoint(endpoint).unwrap();
    let ConnectionState::Ready(generation) = topology.endpoint(endpoint).unwrap().state() else {
        unreachable!();
    };
    (endpoint, generation)
}

#[test]
fn duplicate_endpoint_admission_closes_every_input_before_releasing_the_reservation() {
    let (endpoint, generation) = attachment(TransportKind::Serial);
    let log = Arc::new(Mutex::new(vec![]));
    let leases = (0..2)
        .map(|_| {
            TransportLease::new(
                endpoint,
                generation,
                AccessGrant::Cooperative,
                Box::new(ProbeIo {
                    kind: TransportKind::Serial,
                    log: log.clone(),
                    excess: false,
                }),
                4,
            )
            .unwrap()
        })
        .collect();
    assert!(
        matches!(LeaseSet::new(leases, Box::new(Reservation(log.clone()))), Err(TransportError::DuplicateEndpoint(id)) if id == endpoint)
    );
    assert_eq!(*log.lock().unwrap(), ["close", "close", "reservation"]);
}

#[test]
fn hid_feature_identity_is_preserved_and_serial_operations_cannot_use_a_hid_channel() {
    let (endpoint, generation) = attachment(TransportKind::Hid);
    let log = Arc::new(Mutex::new(vec![]));
    let mut lease = TransportLease::new(
        endpoint,
        generation,
        AccessGrant::Cooperative,
        Box::new(ProbeIo {
            kind: TransportKind::Hid,
            log: log.clone(),
            excess: false,
        }),
        4,
    )
    .unwrap();
    let mut buffer = [0; 4];
    let mut cx = Context::from_waker(std::task::Waker::noop());
    assert!(matches!(
        lease.poll_io(&mut cx, IoRequest::SerialRead(&mut buffer)),
        Poll::Ready(Err(TransportError::WrongKind(_)))
    ));
    assert!(log.lock().unwrap().is_empty());
    assert!(matches!(
        lease.poll_io(
            &mut cx,
            IoRequest::HidGetFeature {
                report: 77,
                data: &mut buffer
            }
        ),
        Poll::Ready(Ok(4))
    ));
    assert_eq!(buffer, [7; 4]);
    assert_eq!(*log.lock().unwrap(), ["io", "feature:77"]);
}

#[test]
fn transfer_bounds_reject_oversized_requests_and_impossible_adapter_reply_lengths() {
    for excess in [false, true] {
        let (endpoint, generation) = attachment(TransportKind::Serial);
        let log = Arc::new(Mutex::new(vec![]));
        let mut lease = TransportLease::new(
            endpoint,
            generation,
            AccessGrant::Cooperative,
            Box::new(ProbeIo {
                kind: TransportKind::Serial,
                log: log.clone(),
                excess,
            }),
            4,
        )
        .unwrap();
        let mut buffer = vec![0; if excess { 4 } else { 5 }];
        let mut cx = Context::from_waker(std::task::Waker::noop());
        assert!(matches!(
            lease.poll_io(&mut cx, IoRequest::SerialRead(&mut buffer)),
            Poll::Ready(Err(TransportError::ResourceLimit { .. }))
        ));
        assert_eq!(log.lock().unwrap().len(), usize::from(excess));
    }
}
