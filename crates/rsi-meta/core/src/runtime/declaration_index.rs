#![allow(clippy::wildcard_imports)] // This is one implementation partition of runtime.

use super::*;
use crate::Requirement;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct DeclarationSlot {
    key: ServiceKey,
    isolation: IsolationId,
    contract: ContractId,
    version: ContractVersion,
}

#[derive(Default)]
pub(super) struct DeclarationIndex {
    providers: HashMap<DeclarationSlot, BTreeSet<FiberId>>,
    by_fiber: BTreeMap<FiberId, Vec<DeclarationSlot>>,
}

impl DeclarationIndex {
    pub(super) fn insert(
        &mut self,
        id: FiberId,
        context: &ContextScope,
        descriptor: &PluginDescriptor,
    ) {
        let slots = descriptor
            .provides
            .iter()
            .map(|provision| DeclarationSlot {
                key: provision.key.clone(),
                isolation: Runtime::isolation_for(&context.isolation, &provision.key),
                contract: provision.contract.clone(),
                version: provision.version,
            })
            .collect::<Vec<_>>();
        debug_assert!(!self.by_fiber.contains_key(&id));
        for slot in &slots {
            self.providers.entry(slot.clone()).or_default().insert(id);
        }
        self.by_fiber.insert(id, slots);
    }

    pub(super) fn remove(&mut self, id: FiberId) {
        let Some(slots) = self.by_fiber.remove(&id) else {
            return;
        };
        for slot in slots {
            let remove_slot = self.providers.get_mut(&slot).is_some_and(|providers| {
                providers.remove(&id);
                providers.is_empty()
            });
            if remove_slot {
                self.providers.remove(&slot);
            }
        }
    }

    pub(super) fn providers(
        &self,
        context: &ContextScope,
        requirement: &Requirement,
    ) -> Vec<FiberId> {
        let slot = DeclarationSlot {
            key: requirement.key.clone(),
            isolation: Runtime::isolation_for(&context.isolation, &requirement.key),
            contract: requirement.contract.clone(),
            version: requirement.version,
        };
        self.providers
            .get(&slot)
            .map_or_else(Vec::new, |providers| providers.iter().copied().collect())
    }
}
