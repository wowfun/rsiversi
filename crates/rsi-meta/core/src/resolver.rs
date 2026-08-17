use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use crate::model::{
    BindingSnapshot, Diagnostic, Generation, GraphRevision, GraphSnapshot, InactiveReason,
    InstanceId, InstanceSnapshot, InstanceSpec, InstanceStatus, MAX_COMPOSITION_REQUIREMENTS,
    PackageSource, RouteKey, RouteTarget, RoutingSnapshot, ScopeId, ServiceKey, ServiceRequirement,
    ValidationReport,
};
use crate::{HostError, Result};

/// Loader-derived composition input. It is deliberately crate-private: callers
/// declare only mounts, while `plugin.toml` remains the sole contract source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedInstanceSpec {
    pub mount: InstanceSpec,
    pub package: PackageSource,
    pub provides: Vec<ServiceKey>,
    pub requires: Vec<ServiceRequirement>,
    pub capabilities: Vec<String>,
}

pub(crate) fn dependency_waves(graph: &GraphSnapshot) -> Result<Vec<Vec<InstanceId>>> {
    let active: BTreeSet<_> = graph
        .instances
        .iter()
        .filter(|(_, instance)| instance.status.is_active())
        .map(|(instance_id, _)| instance_id.clone())
        .collect();
    let mut indegree: BTreeMap<_, usize> = active
        .iter()
        .cloned()
        .map(|instance| (instance, 0))
        .collect();
    let mut consumers = BTreeMap::<InstanceId, BTreeSet<InstanceId>>::new();
    for binding in &graph.bindings {
        if active.contains(&binding.provider) && active.contains(&binding.consumer) {
            let inserted = consumers
                .entry(binding.provider.clone())
                .or_default()
                .insert(binding.consumer.clone());
            if inserted {
                *indegree
                    .get_mut(&binding.consumer)
                    .expect("active consumer has indegree") += 1;
            }
        }
    }
    let mut ready: BTreeSet<_> = indegree
        .iter()
        .filter(|(_, degree)| **degree == 0)
        .map(|(instance, _)| instance.clone())
        .collect();
    let mut waves = Vec::new();
    let mut ordered = 0;
    while !ready.is_empty() {
        let wave = std::mem::take(&mut ready).into_iter().collect::<Vec<_>>();
        ordered += wave.len();
        let mut next = BTreeSet::new();
        for instance in &wave {
            for consumer in consumers.get(instance).into_iter().flatten() {
                let degree = indegree
                    .get_mut(consumer)
                    .expect("dependency consumer has indegree");
                *degree -= 1;
                if *degree == 0 {
                    next.insert(consumer.clone());
                }
            }
        }
        waves.push(wave);
        ready = next;
    }
    if ordered != active.len() {
        return Err(HostError::InvalidManifest(
            "active service dependency graph contains a cycle".to_owned(),
        ));
    }
    Ok(waves)
}

#[allow(clippy::too_many_lines)]
pub(crate) fn resolve(
    composition_id: &str,
    instances: &[ResolvedInstanceSpec],
    scope_parents: &BTreeMap<ScopeId, Option<ScopeId>>,
    revision: GraphRevision,
    reusable_generations: Option<&BTreeMap<InstanceId, Arc<Generation>>>,
    next_generation: &mut u64,
) -> std::result::Result<RoutingSnapshot, ValidationReport> {
    let requirement_count = instances.iter().fold(0_usize, |total, instance| {
        total.saturating_add(instance.requires.len())
    });
    if requirement_count > MAX_COMPOSITION_REQUIREMENTS {
        return Err(ValidationReport {
            diagnostics: vec![Diagnostic::error(
                "requirement_limit",
                format!(
                    "composition contains {requirement_count} service requirements; maximum is {MAX_COMPOSITION_REQUIREMENTS}"
                ),
                Some("instances".to_owned()),
            )],
        });
    }
    let instances_by_id: BTreeMap<_, _> = instances
        .iter()
        .map(|instance| (instance.mount.id.clone(), instance))
        .collect();
    let mut providers = BTreeMap::<ScopeId, BTreeMap<ServiceKey, Vec<InstanceId>>>::new();
    let mut consumers = BTreeMap::<ServiceKey, BTreeSet<InstanceId>>::new();
    for instance in instances {
        for service in &instance.provides {
            providers
                .entry(instance.mount.scope.clone())
                .or_default()
                .entry(service.clone())
                .or_default()
                .push(instance.mount.id.clone());
        }
        for requirement in &instance.requires {
            consumers
                .entry(requirement.service.clone())
                .or_default()
                .insert(instance.mount.id.clone());
        }
    }
    let explicit: BTreeMap<_, _> = instances
        .iter()
        .flat_map(|instance| {
            instance.mount.bindings.iter().map(|(service, provider)| {
                (
                    (instance.mount.id.clone(), service.clone()),
                    provider.clone(),
                )
            })
        })
        .collect();

    let mut diagnostics = validate_contract_bindings(instances, &instances_by_id);
    if !diagnostics.is_empty() {
        return Err(ValidationReport { diagnostics });
    }

    let mut active: BTreeSet<_> = instances
        .iter()
        .filter(|instance| instance.mount.enabled)
        .map(|instance| instance.mount.id.clone())
        .collect();
    let mut inactive = BTreeMap::<InstanceId, Vec<InactiveReason>>::new();
    for instance in instances.iter().filter(|instance| !instance.mount.enabled) {
        inactive.insert(instance.mount.id.clone(), vec![InactiveReason::Disabled]);
    }

    // Removing a provider can only affect consumers of its services. A work
    // queue avoids rescanning unrelated instances to reach the monotonic fixpoint.
    let mut queued = active.clone();
    let mut work: VecDeque<_> = active.iter().cloned().collect();
    while let Some(instance_id) = work.pop_front() {
        queued.remove(&instance_id);
        if !active.contains(&instance_id) {
            continue;
        }
        let instance = instances_by_id
            .get(&instance_id)
            .expect("validated instance id is present");
        let mut reasons = Vec::new();
        for requirement in &instance.requires {
            match resolve_provider(
                &instance_id,
                &instance.mount.scope,
                &requirement.service,
                &providers,
                &active,
                &explicit,
                scope_parents,
            ) {
                ProviderResolution::Bound { .. }
                | ProviderResolution::HostOwned
                | ProviderResolution::Ambiguous(_) => {}
                ProviderResolution::Missing if requirement.optional => {}
                ProviderResolution::Missing => reasons.push(InactiveReason::MissingService {
                    service: requirement.service.clone(),
                }),
                ProviderResolution::ExplicitInactive(provider) => {
                    reasons.push(InactiveReason::ExplicitProviderInactive {
                        service: requirement.service.clone(),
                        provider,
                    });
                }
            }
        }
        if reasons.is_empty() {
            continue;
        }
        active.remove(&instance_id);
        inactive.entry(instance_id.clone()).or_insert(reasons);
        for service in &instance.provides {
            for consumer in consumers.get(service).into_iter().flatten() {
                if active.contains(consumer) && queued.insert(consumer.clone()) {
                    work.push_back(consumer.clone());
                }
            }
        }
    }

    // Ambiguity is never interpreted as absence, including optional injects.
    // It rejects the whole graph before any routing snapshot can be published.
    let mut resolutions = BTreeMap::new();
    for instance_id in &active {
        let instance = instances_by_id
            .get(instance_id)
            .expect("validated instance id is present");
        for requirement in &instance.requires {
            let resolution = resolve_provider(
                instance_id,
                &instance.mount.scope,
                &requirement.service,
                &providers,
                &active,
                &explicit,
                scope_parents,
            );
            if let ProviderResolution::Ambiguous(candidates) = &resolution {
                diagnostics.push(Diagnostic::error(
                    "ambiguous_service",
                    format!(
                        "instance {} sees multiple nearest providers for {}: {}",
                        instance_id,
                        requirement.service,
                        candidates
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    Some(format!("instances[{instance_id}].injects")),
                ));
            }
            resolutions.insert(
                (instance_id.clone(), requirement.service.clone()),
                resolution,
            );
        }
    }
    if !diagnostics.is_empty() {
        return Err(ValidationReport { diagnostics });
    }

    let mut generations = BTreeMap::new();
    for instance_id in &active {
        let generation = reusable_generations
            .and_then(|generations| generations.get(instance_id).cloned())
            .unwrap_or_else(|| {
                let id = *next_generation;
                *next_generation = next_generation.saturating_add(1);
                Arc::new(Generation::new(id, instance_id.clone()))
            });
        *next_generation = (*next_generation).max(generation.id.saturating_add(1));
        generations.insert(instance_id.clone(), generation);
    }

    let mut routes = BTreeMap::new();
    for instance_id in &active {
        let instance = instances_by_id
            .get(instance_id)
            .expect("validated instance id is present");
        for requirement in &instance.requires {
            if let Some(ProviderResolution::Bound { provider, explicit }) =
                resolutions.get(&(instance_id.clone(), requirement.service.clone()))
            {
                let generation = Arc::clone(
                    generations
                        .get(provider)
                        .expect("active provider has a generation"),
                );
                routes.insert(
                    RouteKey {
                        consumer: instance_id.clone(),
                        service: requirement.service.clone(),
                    },
                    RouteTarget {
                        provider: provider.clone(),
                        explicit: *explicit,
                        generation,
                    },
                );
            }
        }
    }

    let instance_snapshots = instances
        .iter()
        .map(|instance| {
            let status = generations.get(&instance.mount.id).map_or_else(
                || InstanceStatus::Inactive {
                    reasons: inactive
                        .get(&instance.mount.id)
                        .cloned()
                        .unwrap_or_else(|| vec![InactiveReason::Disabled]),
                },
                |_generation| InstanceStatus::Active,
            );
            (
                instance.mount.id.clone(),
                InstanceSnapshot {
                    id: instance.mount.id.clone(),
                    package: instance.package.clone(),
                    scope: instance.mount.scope.clone(),
                    status,
                    provides: instance.provides.clone(),
                    requires: instance.requires.clone(),
                },
            )
        })
        .collect();

    let bindings = routes
        .iter()
        .map(|(key, target)| BindingSnapshot {
            consumer: key.consumer.clone(),
            service: key.service.clone(),
            provider: target.provider.clone(),
            explicit: target.explicit,
        })
        .chain(active.iter().flat_map(|instance_id| {
            let instance = instances_by_id
                .get(instance_id)
                .expect("active instance is present");
            instance
                .requires
                .iter()
                .filter(|requirement| is_host_service(&requirement.service))
                .map(|requirement| BindingSnapshot {
                    consumer: instance_id.clone(),
                    service: requirement.service.clone(),
                    provider: InstanceId::new(format!("@host/{}", requirement.service.as_str())),
                    explicit: false,
                })
        }))
        .collect();
    Ok(RoutingSnapshot::new(
        GraphSnapshot {
            revision,
            composition_id: composition_id.to_owned(),
            instances: instance_snapshots,
            bindings,
            retiring_instances: Vec::new(),
        },
        routes,
        generations,
    ))
}

fn validate_contract_bindings(
    instances: &[ResolvedInstanceSpec],
    instances_by_id: &BTreeMap<InstanceId, &ResolvedInstanceSpec>,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for instance in instances {
        for service in &instance.provides {
            if is_host_service(service) {
                diagnostics.push(Diagnostic::error(
                    "reserved_service_provided",
                    format!(
                        "instance {} cannot provide host-owned service {}",
                        instance.mount.id, service
                    ),
                    Some(format!("instances[{}].provides", instance.mount.id)),
                ));
            }
        }
        for (service, provider_id) in &instance.mount.bindings {
            if is_host_service(service) {
                diagnostics.push(Diagnostic::error(
                    "reserved_service_binding",
                    format!("host-owned service {service} cannot be explicitly rebound"),
                    Some(format!("instances[{}].bindings", instance.mount.id)),
                ));
                continue;
            }
            if !instance
                .requires
                .iter()
                .any(|requirement| requirement.service == *service)
            {
                diagnostics.push(Diagnostic::error(
                    "undeclared_bound_inject",
                    format!(
                        "instance {} does not inject {} in its package manifest",
                        instance.mount.id, service
                    ),
                    Some(format!("instances[{}].bindings", instance.mount.id)),
                ));
            }
            if let Some(provider) = instances_by_id.get(provider_id)
                && !provider.provides.contains(service)
            {
                diagnostics.push(Diagnostic::error(
                    "bound_provider_missing_contract",
                    format!("bound instance {provider_id} does not provide {service}"),
                    Some(format!("instances[{}].bindings", instance.mount.id)),
                ));
            }
        }
        if instance
            .requires
            .iter()
            .any(|requirement| requirement.service.as_str() == crate::runtime::STATE_SERVICE)
            && !instance
                .capabilities
                .iter()
                .any(|capability| capability == crate::runtime::STATE_SERVICE)
        {
            diagnostics.push(Diagnostic::error(
                "state_capability_required",
                format!(
                    "instance {} injects state.cas without declaring the state.cas capability",
                    instance.mount.id
                ),
                Some(format!("instances[{}]", instance.mount.id)),
            ));
        }
    }
    diagnostics
}

enum ProviderResolution {
    Bound {
        provider: InstanceId,
        explicit: bool,
    },
    Missing,
    Ambiguous(Vec<InstanceId>),
    ExplicitInactive(InstanceId),
    HostOwned,
}

fn resolve_provider(
    consumer: &InstanceId,
    consumer_scope: &ScopeId,
    service: &ServiceKey,
    providers: &BTreeMap<ScopeId, BTreeMap<ServiceKey, Vec<InstanceId>>>,
    active: &BTreeSet<InstanceId>,
    explicit: &BTreeMap<(InstanceId, ServiceKey), InstanceId>,
    scope_parents: &BTreeMap<ScopeId, Option<ScopeId>>,
) -> ProviderResolution {
    if is_host_service(service) {
        return ProviderResolution::HostOwned;
    }
    if let Some(provider) = explicit.get(&(consumer.clone(), service.clone())) {
        return if active.contains(provider) {
            ProviderResolution::Bound {
                provider: provider.clone(),
                explicit: true,
            }
        } else {
            ProviderResolution::ExplicitInactive(provider.clone())
        };
    }

    let mut scope = Some(consumer_scope);
    while let Some(current_scope) = scope {
        let candidates: Vec<_> = providers
            .get(current_scope)
            .and_then(|services| services.get(service))
            .into_iter()
            .flatten()
            .filter(|instance| active.contains(*instance))
            .cloned()
            .collect();
        match candidates.as_slice() {
            [] => {}
            [provider] => {
                return ProviderResolution::Bound {
                    provider: provider.clone(),
                    explicit: false,
                };
            }
            _ => return ProviderResolution::Ambiguous(candidates),
        }
        scope = scope_parents.get(current_scope).and_then(Option::as_ref);
    }
    ProviderResolution::Missing
}

fn is_host_service(service: &ServiceKey) -> bool {
    matches!(
        service.as_str(),
        crate::runtime::STATE_SERVICE | crate::runtime::TICK_SERVICE
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::model::PackageId;

    fn instance(id: &str, scope: &str) -> ResolvedInstanceSpec {
        ResolvedInstanceSpec {
            mount: InstanceSpec {
                id: InstanceId::new(id),
                package: PathBuf::from(format!("{id}/plugin.toml")),
                scope: ScopeId::new(scope),
                enabled: true,
                config: serde_json::Value::Null,
                bindings: BTreeMap::new(),
            },
            package: PackageSource {
                package_id: PackageId::new(format!("example/{id}")),
                version: "1.0.0".to_owned(),
                manifest_path: PathBuf::from(format!("{id}/plugin.toml")),
                target: "test-target".to_owned(),
                manifest_sha256: rsi_meta_loader::ContentHash::digest([]),
                artifact_sha256: rsi_meta_loader::ContentHash::digest([]),
                config_schema_sha256: None,
            },
            provides: Vec::new(),
            requires: Vec::new(),
            capabilities: Vec::new(),
        }
    }

    #[test]
    fn nearest_lexical_provider_shadows_root() {
        let service = ServiceKey::new("example/cache");
        let mut root = instance("root-cache", "root");
        root.provides.push(service.clone());
        let mut local = instance("local-cache", "team");
        local.provides.push(service.clone());
        let mut consumer = instance("consumer", "app");
        consumer.requires.push(ServiceRequirement {
            service: service.clone(),
            optional: false,
        });
        let mut next = 1;
        let scopes = scopes();
        let snapshot = resolve(
            "test",
            &[root, local, consumer],
            &scopes,
            GraphRevision(1),
            None,
            &mut next,
        )
        .expect("unambiguous graph");
        let route = snapshot
            .route(&RouteKey {
                consumer: InstanceId::new("consumer"),
                service,
            })
            .expect("route");
        assert_eq!(route.provider, InstanceId::new("local-cache"));
        assert!(!route.explicit);
    }

    #[test]
    fn ambiguity_rejects_even_an_optional_inject() {
        let service = ServiceKey::new("example/cache");
        let mut first = instance("first", "root");
        first.provides.push(service.clone());
        let mut second = instance("second", "root");
        second.provides.push(service.clone());
        let mut consumer = instance("consumer", "root");
        consumer.requires.push(ServiceRequirement {
            service,
            optional: true,
        });
        let mut next = 1;
        let scopes = scopes();
        let report = resolve(
            "test",
            &[first, second, consumer],
            &scopes,
            GraphRevision(1),
            None,
            &mut next,
        )
        .expect_err("ambiguity must reject the transaction");
        assert_eq!(report.diagnostics[0].code, "ambiguous_service");
    }

    #[test]
    fn explicit_binding_selects_one_provider() {
        let service = ServiceKey::new("example/cache");
        let mut first = instance("first", "root");
        first.provides.push(service.clone());
        let mut second = instance("second", "root");
        second.provides.push(service.clone());
        let mut consumer = instance("consumer", "root");
        consumer.requires.push(ServiceRequirement {
            service: service.clone(),
            optional: false,
        });
        consumer
            .mount
            .bindings
            .insert(service.clone(), InstanceId::new("second"));
        let mut next = 1;
        let scopes = scopes();
        let snapshot = resolve(
            "test",
            &[first, second, consumer],
            &scopes,
            GraphRevision(1),
            None,
            &mut next,
        )
        .expect("explicit binding resolves ambiguity");
        let route = snapshot
            .route(&RouteKey {
                consumer: InstanceId::new("consumer"),
                service,
            })
            .expect("route");
        assert_eq!(route.provider, InstanceId::new("second"));
        assert!(route.explicit);
    }

    #[test]
    fn host_owned_services_cannot_be_provided_or_rebound() {
        let state = ServiceKey::new(crate::runtime::STATE_SERVICE);
        let mut impostor = instance("impostor", "root");
        impostor.provides.push(state.clone());
        let mut consumer = instance("consumer", "root");
        consumer.requires.push(ServiceRequirement {
            service: state.clone(),
            optional: false,
        });
        consumer
            .capabilities
            .push(crate::runtime::STATE_SERVICE.to_owned());
        consumer
            .mount
            .bindings
            .insert(state, InstanceId::new("impostor"));
        let mut next = 1;
        let report = resolve(
            "test",
            &[impostor, consumer],
            &scopes(),
            GraphRevision(1),
            None,
            &mut next,
        )
        .expect_err("reserved service impersonation must reject the graph");
        let codes: BTreeSet<_> = report
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect();
        assert!(codes.contains("reserved_service_provided"));
        assert!(codes.contains("reserved_service_binding"));
    }

    #[test]
    fn missing_required_inject_marks_consumer_inactive() {
        let service = ServiceKey::new("example/missing");
        let mut consumer = instance("consumer", "root");
        consumer.requires.push(ServiceRequirement {
            service: service.clone(),
            optional: false,
        });
        let mut next = 1;
        let snapshot = resolve(
            "test",
            &[consumer],
            &scopes(),
            GraphRevision(1),
            None,
            &mut next,
        )
        .expect("missing required service produces an inactive graph");
        assert!(matches!(
            snapshot.graph().instances[&InstanceId::new("consumer")].status,
            InstanceStatus::Inactive { ref reasons }
                if reasons == &vec![InactiveReason::MissingService { service }]
        ));
    }

    #[test]
    fn missing_optional_inject_keeps_consumer_active_without_a_route() {
        let service = ServiceKey::new("example/optional");
        let mut consumer = instance("consumer", "root");
        consumer.requires.push(ServiceRequirement {
            service: service.clone(),
            optional: true,
        });
        let mut next = 1;
        let snapshot = resolve(
            "test",
            &[consumer],
            &scopes(),
            GraphRevision(1),
            None,
            &mut next,
        )
        .expect("optional absence is valid");
        assert!(
            snapshot.graph().instances[&InstanceId::new("consumer")]
                .status
                .is_active()
        );
        assert!(
            snapshot
                .route(&RouteKey {
                    consumer: InstanceId::new("consumer"),
                    service,
                })
                .is_none()
        );
    }

    #[test]
    fn required_inactivity_propagates_transitively_to_dependents() {
        let upstream = ServiceKey::new("example/upstream");
        let downstream = ServiceKey::new("example/downstream");
        let mut middle = instance("middle", "root");
        middle.provides.push(downstream.clone());
        middle.requires.push(ServiceRequirement {
            service: upstream,
            optional: false,
        });
        let mut leaf = instance("leaf", "root");
        leaf.requires.push(ServiceRequirement {
            service: downstream,
            optional: false,
        });
        let mut next = 1;
        let snapshot = resolve(
            "test",
            &[middle, leaf],
            &scopes(),
            GraphRevision(1),
            None,
            &mut next,
        )
        .expect("inactive closure is a valid graph state");
        for id in ["middle", "leaf"] {
            assert!(matches!(
                snapshot.graph().instances[&InstanceId::new(id)].status,
                InstanceStatus::Inactive { .. }
            ));
        }
    }

    #[test]
    fn cascading_inactivity_stays_bounded_at_supported_graph_scales() {
        for size in [100, 1_000, crate::model::MAX_COMPOSITION_INSTANCES] {
            let mut instances = Vec::with_capacity(size);
            for index in 0..size {
                let mut current = instance(&format!("instance-{index}"), "root");
                current
                    .provides
                    .push(ServiceKey::new(format!("service-{index}")));
                current.requires.push(ServiceRequirement {
                    service: if index == 0 {
                        ServiceKey::new("missing-root")
                    } else {
                        ServiceKey::new(format!("service-{}", index - 1))
                    },
                    optional: false,
                });
                instances.push(current);
            }
            let started = Instant::now();
            let mut next = 1;
            let snapshot = resolve(
                "scale",
                &instances,
                &scopes(),
                GraphRevision(1),
                None,
                &mut next,
            )
            .expect("bounded cascading graph resolves");
            assert!(
                snapshot
                    .graph()
                    .instances
                    .values()
                    .all(|instance| matches!(instance.status, InstanceStatus::Inactive { .. }))
            );
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "{size}-instance cascading graph exceeded the debug-build budget"
            );
        }
    }

    #[test]
    fn explicit_binding_can_cross_sibling_scope_branches() {
        let service = ServiceKey::new("example/cross-branch");
        let mut provider = instance("provider", "right");
        provider.provides.push(service.clone());
        let mut consumer = instance("consumer", "left");
        consumer.requires.push(ServiceRequirement {
            service: service.clone(),
            optional: false,
        });
        consumer
            .mount
            .bindings
            .insert(service.clone(), InstanceId::new("provider"));
        let branches = BTreeMap::from([
            (ScopeId::new("root"), None),
            (ScopeId::new("left"), Some(ScopeId::new("root"))),
            (ScopeId::new("right"), Some(ScopeId::new("root"))),
        ]);
        let mut next = 1;
        let snapshot = resolve(
            "test",
            &[provider, consumer],
            &branches,
            GraphRevision(1),
            None,
            &mut next,
        )
        .expect("explicit binding bypasses lexical ancestry");
        let route = snapshot
            .route(&RouteKey {
                consumer: InstanceId::new("consumer"),
                service,
            })
            .expect("explicit cross-branch route");
        assert_eq!(route.provider, InstanceId::new("provider"));
        assert!(route.explicit);
    }

    #[test]
    fn recomputing_same_desired_input_activates_when_provider_appears() {
        let service = ServiceKey::new("example/dynamic");
        let mut consumer = instance("consumer", "root");
        consumer.requires.push(ServiceRequirement {
            service: service.clone(),
            optional: false,
        });
        let mut first_next = 1;
        let absent = resolve(
            "test",
            &[consumer.clone()],
            &scopes(),
            GraphRevision(1),
            None,
            &mut first_next,
        )
        .expect("missing provider is inactive");
        assert!(
            !absent.graph().instances[&consumer.mount.id]
                .status
                .is_active()
        );

        let mut provider = instance("provider", "root");
        provider.provides.push(service.clone());
        let mut second_next = first_next;
        let present = resolve(
            "test",
            &[consumer.clone(), provider],
            &scopes(),
            GraphRevision(2),
            None,
            &mut second_next,
        )
        .expect("same desired consumer activates when provider appears");
        assert!(
            present.graph().instances[&consumer.mount.id]
                .status
                .is_active()
        );
        assert!(
            present
                .route(&RouteKey {
                    consumer: consumer.mount.id,
                    service,
                })
                .is_some()
        );
    }

    fn scopes() -> BTreeMap<ScopeId, Option<ScopeId>> {
        BTreeMap::from([
            (ScopeId::new("root"), None),
            (ScopeId::new("team"), Some(ScopeId::new("root"))),
            (ScopeId::new("app"), Some(ScopeId::new("team"))),
        ])
    }
}
