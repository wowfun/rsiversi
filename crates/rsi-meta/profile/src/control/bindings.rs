use super::{ProfileError, ProfileResolver, Result};
use crate::{IsolationLane, IsolationSpec};
use rsi_meta::{
    ChildPosition, Context, FiberGeneration, FiberId, InstanceId, IsolationId, LocalIsolationId,
    RuntimeIdentity,
};
use std::any::TypeId;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};

/// Activation-local identity, never a configuration input or a Runtime owner.
#[derive(Debug)]
pub(super) struct Namespace {
    runtime: RuntimeIdentity,
    owner: (FiberId, FiberGeneration),
    allocations: Mutex<BTreeMap<AllocationKey, Weak<Allocation>>>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum AllocationKey {
    Fresh(String, ContractKey),
    Named(String, ContractKey),
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum ContractKey {
    Local(String, TypeId),
    Event(String, TypeId),
    Portable(String),
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum Allocation {
    Local(LocalIsolationId),
    Portable(IsolationId),
}

pub(super) type EffectiveBindings = BTreeMap<ContractKey, Arc<Allocation>>;

#[derive(Clone, Debug, Default)]
pub(super) struct BindingSnapshot {
    pub(super) namespace: Option<Arc<Namespace>>,
    pub(super) groups: BTreeMap<String, EffectiveBindings>,
    pub(super) effective: BTreeMap<InstanceId, EffectiveBindings>,
    pub(super) positions: BTreeMap<InstanceId, ChildPosition>,
}

impl Namespace {
    pub(super) fn select(base: &Context, previous: &BindingSnapshot) -> Result<Arc<Self>> {
        let owner = base.owner().ok_or_else(|| {
            ProfileError::InvalidProgram("Profile bindings require an owning wrapper".into())
        })?;
        if let Some(namespace) = &previous.namespace {
            if namespace.runtime != base.runtime_identity() || namespace.owner != owner {
                return Err(ProfileError::InvalidProgram(
                    "Profile binding namespace belongs to another activation".into(),
                ));
            }
            namespace
                .allocations
                .lock()
                .expect("Profile allocations poisoned")
                .retain(|_, value| value.strong_count() != 0);
            return Ok(namespace.clone());
        }
        Ok(Arc::new(Self {
            runtime: base.runtime_identity(),
            owner,
            allocations: Mutex::new(BTreeMap::new()),
        }))
    }

    pub(super) fn delta(
        &self,
        base: &Context,
        resolver: &dyn ProfileResolver,
        group: &str,
        spec: &IsolationSpec,
    ) -> Result<EffectiveBindings> {
        let mut delta = EffectiveBindings::new();
        for (lane, keys) in [
            (IsolationLane::Local, spec.local()),
            (IsolationLane::Event, spec.events()),
            (IsolationLane::Portable, spec.portable()),
        ] {
            for key in keys {
                let contract = resolve_contract(resolver, lane, key)?;
                let allocation = self.allocate(
                    base,
                    AllocationKey::Fresh(group.to_owned(), contract.clone()),
                    &contract,
                )?;
                delta.insert(contract, allocation);
            }
        }
        for named in spec.named() {
            let contract = resolve_contract(resolver, named.lane(), named.key())?;
            let allocation = self.allocate(
                base,
                AllocationKey::Named(named.label().to_owned(), contract.clone()),
                &contract,
            )?;
            delta.insert(contract, allocation);
        }
        Ok(delta)
    }

    fn allocate(
        &self,
        base: &Context,
        key: AllocationKey,
        contract: &ContractKey,
    ) -> Result<Arc<Allocation>> {
        let mut allocations = self
            .allocations
            .lock()
            .expect("Profile allocations poisoned");
        if let Some(existing) = allocations.get(&key).and_then(Weak::upgrade) {
            return Ok(existing);
        }
        // Mint through the existing Context boundary; only Profile owns the
        // allocation lookup. The frozen resolver supplies nominal metadata only.
        let allocation = Arc::new(match contract {
            ContractKey::Local(key, nominal) => {
                Allocation::Local(base.clone().isolate_local_type_fresh(*nominal, key)?.1)
            }
            ContractKey::Event(key, nominal) => {
                Allocation::Local(base.clone().isolate_event_type_fresh(*nominal, key)?.1)
            }
            ContractKey::Portable(key) => Allocation::Portable(base.clone().isolate_fresh(key)?.1),
        });
        allocations.insert(key, Arc::downgrade(&allocation));
        Ok(allocation)
    }
}

fn resolve_contract(
    resolver: &dyn ProfileResolver,
    lane: IsolationLane,
    key: &str,
) -> Result<ContractKey> {
    Ok(match lane {
        IsolationLane::Local => {
            ContractKey::Local(key.to_owned(), resolver.local_contract_type(key)?)
        }
        IsolationLane::Event => ContractKey::Event(key.to_owned(), resolver.local_event_type(key)?),
        IsolationLane::Portable => ContractKey::Portable(key.to_owned()),
    })
}

pub(super) fn apply(mut base: Context, effective: &EffectiveBindings) -> Result<Context> {
    for (contract, allocation) in effective {
        base = match (contract, allocation.as_ref()) {
            (ContractKey::Local(key, nominal), Allocation::Local(id)) => {
                base.isolate_local_type(*nominal, key, *id)?
            }
            (ContractKey::Event(key, nominal), Allocation::Local(id)) => {
                base.isolate_event_type(*nominal, key, *id)?
            }
            (ContractKey::Portable(key), Allocation::Portable(id)) => base.isolate(key, *id)?,
            _ => unreachable!("Profile allocates identities for their exact contract lane"),
        };
    }
    Ok(base)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use rsi_meta::{
        ActivationPlan, ConfigValue, PluginFactory, PreparedActivation, ResolvedFactory, Runtime,
        UpdateMode,
    };

    #[derive(Debug)]
    struct Capture(Arc<Mutex<Option<Context>>>);
    #[async_trait]
    impl PluginFactory for Capture {
        fn prepare(&self, value: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
            Ok(PreparedActivation::new(value.clone()))
        }
        async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
            *self.0.lock().unwrap() = Some(plan.context().clone());
            Ok(())
        }
    }
    #[derive(Debug)]
    struct Resolver;
    impl ProfileResolver for Resolver {
        fn resolve(&self, _: &rsi_meta::PluginId) -> Result<ResolvedFactory> {
            unreachable!()
        }
    }

    #[tokio::test]
    async fn allocation_lookup_reclaims_history_and_rejects_other_wrapper_generations() {
        let runtime = Runtime::default();
        let captured = Arc::new(Mutex::new(None));
        let factory = ResolvedFactory::linked(
            "capture",
            "one",
            UpdateMode::Replayable,
            Arc::new(Capture(captured.clone())),
        );
        let wrapper = runtime
            .root()
            .apply(factory, ConfigValue::Null)
            .await
            .unwrap();
        let base = captured.lock().unwrap().take().unwrap();
        assert!(Namespace::select(&runtime.root(), &BindingSnapshot::default()).is_err());
        let namespace = Namespace::select(&base, &BindingSnapshot::default()).unwrap();
        let snapshot = BindingSnapshot {
            namespace: Some(namespace.clone()),
            ..BindingSnapshot::default()
        };
        let kept = namespace
            .delta(
                &base,
                &Resolver,
                "g",
                &IsolationSpec::default().with_named(IsolationLane::Portable, "key", "kept"),
            )
            .unwrap();
        for index in 0..1000 {
            let current = Namespace::select(&base, &snapshot).unwrap();
            let temporary = current
                .delta(
                    &base,
                    &Resolver,
                    "g",
                    &IsolationSpec::default().with_named(
                        IsolationLane::Portable,
                        "key",
                        index.to_string(),
                    ),
                )
                .unwrap();
            assert_eq!(current.allocations.lock().unwrap().len(), 2);
            assert_ne!(temporary, kept);
        }
        drop(kept);
        Namespace::select(&base, &snapshot).unwrap();
        assert!(namespace.allocations.lock().unwrap().is_empty());
        wrapper
            .reconfigure(serde_json::json!({"next": true}))
            .await
            .unwrap();
        let next = captured.lock().unwrap().take().unwrap();
        assert_ne!(next.owner(), base.owner());
        assert!(Namespace::select(&next, &snapshot).is_err());
        drop((base, next));
        assert!(wrapper.dispose().await.is_clean());
        assert!(runtime.shutdown().await.is_clean());
    }
}
