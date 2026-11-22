use super::*;
use crate::{backends::MatchHint, model::topology::Endpoint};

fn check(expected: Fact, observed: Option<Fact>) -> PredicateCheck {
    PredicateCheck { expected, observed }
}

pub(super) fn physical_checks(
    selector: &PhysicalSelector,
    metadata: PhysicalMetadata<'_>,
) -> Vec<PredicateCheck> {
    let mut checks = vec![check(Fact::Usb(selector.usb), metadata.usb.map(Fact::Usb))];
    if let Some(serial) = &selector.serial {
        checks.push(check(
            Fact::Serial(serial.clone()),
            metadata.serial.cloned().map(Fact::Serial),
        ));
    }
    checks
}

pub(super) fn identify(ids: Vec<String>) -> Identification {
    match ids.as_slice() {
        [] => Identification::Unknown,
        [one] => Identification::Identified(one.clone()),
        _ => Identification::Ambiguous(ids),
    }
}

fn endpoint_checks(
    selector: &EndpointSelector,
    endpoint: &Endpoint,
    view: &DiscoveryView<'_>,
) -> Vec<PredicateCheck> {
    let mut checks = vec![check(
        Fact::Transport(selector.transport),
        Some(Fact::Transport(endpoint.transport())),
    )];
    if let Some(number) = selector.interface_number {
        let observed = view
            .interfaces()
            .find(|interface| Some(interface.id()) == endpoint.interface())
            .and_then(|interface| interface.usb_number());
        checks.push(check(
            Fact::Interface(number),
            observed.map(Fact::Interface),
        ));
    }
    // Usage facts belong to this collection, not an existential match against
    // any sibling interface or collection that happens to share the physical unit.
    if let Some(page) = selector.hid.usage_page {
        checks.push(check(
            Fact::UsagePage(page),
            endpoint.hid().usage_page.map(Fact::UsagePage),
        ));
    }
    if let Some(usage) = selector.hid.usage {
        checks.push(check(
            Fact::Usage(usage),
            endpoint.hid().usage.map(Fact::Usage),
        ));
    }
    checks
}

fn roles(reference: &ProtocolReference, view: &DiscoveryView<'_>) -> Vec<RoleAssessment> {
    reference
        .roles
        .iter()
        .map(|role| {
            let endpoints: Vec<_> = view
                .endpoints()
                .map(|endpoint| EndpointAssessment {
                    endpoint: endpoint.id(),
                    checks: endpoint_checks(&role.selector, endpoint, view),
                })
                .collect();
            let matches = endpoints
                .iter()
                .filter(|endpoint| {
                    endpoint
                        .checks
                        .iter()
                        .all(|check| check.verdict() == Verdict::Satisfied)
                })
                .map(|endpoint| endpoint.endpoint)
                .collect();
            RoleAssessment {
                role: role.key.clone(),
                matches,
                endpoints,
            }
        })
        .collect()
}

struct Search<'a> {
    roles: &'a [RoleAssessment],
    remaining: usize,
    exhausted: bool,
    solutions: Vec<RoleAssignment>,
}
impl Search<'_> {
    fn visit(
        &mut self,
        index: usize,
        selected: &mut RoleAssignment,
        used: &mut BTreeSet<EndpointId>,
    ) {
        if self.solutions.len() == 2 || self.exhausted {
            return;
        }
        if index == self.roles.len() {
            self.solutions.push(selected.clone());
            return;
        }
        for endpoint in &self.roles[index].matches {
            if self.remaining == 0 {
                self.exhausted = true;
                return;
            }
            self.remaining -= 1;
            if !used.insert(*endpoint) {
                continue;
            }
            let key = self.roles[index].role.clone();
            selected.insert(key.clone(), *endpoint);
            self.visit(index + 1, selected, used);
            selected.remove(&key);
            used.remove(endpoint);
            if self.solutions.len() == 2 || self.exhausted {
                return;
            }
        }
    }
}

fn assignment(roles: &[RoleAssessment], no_endpoints: bool, budget: usize) -> Assignment {
    if no_endpoints {
        return Assignment::NoEndpoints;
    }
    if roles.iter().any(|role| role.matches.is_empty()) {
        return Assignment::NoMatch;
    }
    let mut search = Search {
        roles,
        remaining: budget,
        exhausted: false,
        solutions: vec![],
    };
    search.visit(0, &mut RoleAssignment::new(), &mut BTreeSet::new());
    if search.solutions.len() == 2 {
        let second = search.solutions.pop().expect("second solution");
        return Assignment::Ambiguous {
            first: search.solutions.pop().expect("first solution"),
            second,
        };
    }
    // A partial search cannot prove uniqueness even if it found one assignment.
    if search.exhausted {
        return Assignment::Indeterminate;
    }
    match search.solutions.pop() {
        Some(solution) => Assignment::Unique(solution),
        None => Assignment::NoMatch,
    }
}

fn evidence_holds(evidence: &MatchEvidence, view: &DiscoveryView<'_>) -> bool {
    match evidence {
        MatchEvidence::UsbModel(ids) => view.physical().usb_ids() == Some(*ids),
        MatchEvidence::AdvertisedSerial(serial) => view.physical().serial() == Some(serial),
        MatchEvidence::Interface(number) => view
            .interfaces()
            .any(|interface| interface.usb_number() == Some(*number)),
        MatchEvidence::Transport(kind) => view
            .endpoints()
            .any(|endpoint| endpoint.transport() == *kind),
        MatchEvidence::HidUsagePage(page) => view
            .endpoints()
            .any(|endpoint| endpoint.hid().usage_page == Some(*page)),
        MatchEvidence::HidUsage(usage) => view
            .endpoints()
            .any(|endpoint| endpoint.hid().usage == Some(*usage)),
    }
}

fn backend_candidate(
    backend: &dyn Backend,
    reference: &ProtocolReference,
    selected: &RoleAssignment,
    view: &DiscoveryView<'_>,
) -> Result<Option<BackendCandidate>> {
    let endpoints: Vec<_> = selected.values().copied().collect();
    let restricted = view.restricted(&endpoints)?;
    let MatchHint::Candidate(candidate) = backend.match_metadata(&restricted) else {
        return Ok(None);
    };
    let expected: BTreeSet<_> = endpoints.into_iter().collect();
    if candidate.backend() != reference.backend
        || candidate.configuration() != reference.configuration
        || candidate
            .endpoints()
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            != expected
    {
        return Err(RegistryError::BackendContract {
            backend: reference.backend.clone(),
            reason: "backend/configuration or endpoint set differs from the selected role assignment",
        });
    }
    if !candidate
        .evidence()
        .iter()
        .all(|evidence| evidence_holds(evidence, &restricted))
    {
        return Err(RegistryError::BackendContract {
            backend: reference.backend.clone(),
            reason: "candidate cites evidence absent from its restricted discovery view",
        });
    }
    Ok(Some(candidate))
}

type RouteKey = (String, String, EndpointId, Vec<(String, EndpointId)>);

pub(super) fn inspect(
    registry: &Registry,
    view: DiscoveryView<'_>,
    implementations: &[&dyn Backend],
) -> Result<Inspection> {
    let mut catalog = BTreeMap::new();
    for backend in implementations {
        let id = backend.descriptor().id();
        if catalog.insert(id, *backend).is_some() {
            return Err(RegistryError::DuplicateBackend(id.into()));
        }
    }
    let mut assessments = Vec::new();
    let mut matches = Vec::new();
    let mut routes: BTreeMap<RouteKey, RegistryRoute> = BTreeMap::new();
    for definition in &registry.definitions {
        let checks = physical_checks(
            &definition.selector,
            PhysicalMetadata {
                usb: view.physical().usb_ids(),
                serial: view.physical().serial(),
            },
        );
        let qualifies = checks
            .iter()
            .all(|check| check.verdict() == Verdict::Satisfied);
        let physical_evidence: Vec<_> =
            checks.iter().filter_map(PredicateCheck::evidence).collect();
        assessments.push(DefinitionAssessment {
            definition: definition.id.clone(),
            checks,
        });
        if !qualifies {
            continue;
        }
        let mut protocols = Vec::new();
        for reference in &definition.protocols {
            let roles = roles(reference, &view);
            let assignment = assignment(
                &roles,
                view.endpoints().next().is_none(),
                registry.search_budget,
            );
            let routing = if reference.status == MappingStatus::ResearchOnly {
                RouteStatus::ResearchOnly
            } else if let Some(backend) = catalog.get(reference.backend.as_str()) {
                match &assignment {
                    Assignment::Unique(selected) => {
                        if let Some(mut candidate) =
                            backend_candidate(*backend, reference, selected, &view)?
                        {
                            candidate.add_evidence(physical_evidence.clone());
                            for role in &roles {
                                let endpoint = selected[&role.role];
                                let assessment = role
                                    .endpoints
                                    .iter()
                                    .find(|assessment| assessment.endpoint == endpoint)
                                    .expect("assigned eligible endpoint");
                                candidate.add_evidence(
                                    assessment
                                        .checks
                                        .iter()
                                        .filter_map(PredicateCheck::evidence),
                                );
                            }
                            let primary = selected[&reference.primary_role];
                            let key = (
                                candidate.backend().into(),
                                candidate.configuration().into(),
                                primary,
                                selected
                                    .iter()
                                    .map(|(role, id)| (role.clone(), *id))
                                    .collect(),
                            );
                            let route = routes.entry(key).or_insert_with(|| RegistryRoute {
                                candidate: candidate.clone(),
                                primary,
                                roles: selected.clone(),
                                definitions: BTreeSet::new(),
                                sources: BTreeSet::new(),
                                references: BTreeSet::new(),
                            });
                            route
                                .candidate
                                .add_evidence(candidate.evidence().iter().cloned());
                            route.definitions.insert(definition.id.clone());
                            route.sources.extend(
                                definition.sources.iter().chain(&reference.sources).cloned(),
                            );
                            route
                                .references
                                .insert((definition.id.clone(), reference.id.clone()));
                            RouteStatus::Ready
                        } else {
                            RouteStatus::Declined
                        }
                    }
                    Assignment::Ambiguous { .. } | Assignment::Indeterminate => {
                        RouteStatus::Unresolved
                    }
                    Assignment::NoEndpoints | Assignment::NoMatch => RouteStatus::Incomplete,
                }
            } else {
                RouteStatus::BackendUnavailable
            };
            protocols.push(ProtocolAssessment {
                reference: reference.clone(),
                roles,
                assignment,
                routing,
            });
        }
        let plan = AssociationPlan {
            physical: view.physical().id(),
            keyboard: view.physical().keyboard(),
            interfaces: view.interfaces().map(|interface| interface.id()).collect(),
            endpoints: view.endpoints().map(|endpoint| endpoint.id()).collect(),
            components: definition.components.clone(),
        };
        matches.push(DefinitionMatch {
            definition: definition.clone(),
            protocols,
            plan,
        });
    }
    let identification = identify(
        matches
            .iter()
            .map(|matched| matched.definition.id.clone())
            .collect(),
    );
    Ok(Inspection {
        physical: view.physical().id(),
        identification,
        assessments,
        matches,
        routes: routes.into_values().collect(),
    })
}
