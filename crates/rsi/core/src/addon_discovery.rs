//! Read-only authoring information from an immutable product assembly.

use crate::{AddonFactoryDescription, StandardAddonSet};
use rsi_agent_composition_protocol::AgentCompositionPin;
use rsi_tools_protocol::{ToolDefinition, ToolOutputDeclaration};
use serde::Serialize;
use std::{collections::BTreeMap, sync::Arc};

/// Maximum items returned by one discovery page.
pub const MAXIMUM_ADDON_DISCOVERY_ITEMS: usize = 64;
/// Maximum compact JSON bytes in a page's entries.
pub const MAXIMUM_ADDON_DISCOVERY_BYTES: usize = 256 * 1024;

/// Descriptive entry; it conveys no factory, invocation or Local service handle.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AddonDiscoveryEntry {
    /// Existing owner-declared factory metadata and placement.
    Factory {
        /// Descriptive declaration; prepare remains authoritative.
        description: AddonFactoryDescription,
    },
    /// An explicitly forwarded Local contract, without an invented wire schema.
    LocalContract {
        /// Addon that explicitly exports the contract.
        addon: String,
        /// Nominal public contract key.
        key: String,
    },
    /// A Tool from the supplied immutable Agent pin.
    Tool {
        /// Model-facing definition from that pin.
        definition: ToolDefinition,
        /// Optional canonical output declaration from the same pin.
        output: Option<ToolOutputDeclaration>,
    },
}

#[derive(Debug)]
struct Entry {
    key: String,
    value: AddonDiscoveryEntry,
    bytes: usize,
}
#[derive(Debug)]
struct Snapshot {
    entries: Vec<Entry>,
    _pin: Option<AgentCompositionPin>,
}

/// Frozen metadata capture; all iteration and lookup share one exact identity.
#[derive(Clone, Debug)]
pub struct AddonDiscoverySnapshot(Arc<Snapshot>);

/// Process-local continuation for one exact snapshot; never execution authority.
#[derive(Clone, Debug)]
pub struct AddonDiscoveryCursor {
    snapshot: Arc<Snapshot>,
    offset: usize,
}

/// Bounded descriptive page and its optional continuation.
#[derive(Clone, Debug)]
pub struct AddonDiscoveryPage {
    /// Ordered exact lookup keys and their descriptions.
    pub entries: Vec<(String, AddonDiscoveryEntry)>,
    /// Continuation bound to this capture.
    pub next: Option<AddonDiscoveryCursor>,
}

impl StandardAddonSet {
    /// Captures metadata without activation or access to runtime configuration values.
    pub fn discovery(
        &self,
        pin: Option<&AgentCompositionPin>,
    ) -> rsi_host::Result<AddonDiscoverySnapshot> {
        let mut entries = BTreeMap::new();
        for description in self.descriptions() {
            entries.insert(
                format!("factory:{}", description.plugin),
                AddonDiscoveryEntry::Factory {
                    description: description.clone(),
                },
            );
        }
        for (addon, key) in self.exported_contracts() {
            entries.insert(
                format!("contract:{key}"),
                AddonDiscoveryEntry::LocalContract {
                    addon: addon.into(),
                    key: key.into(),
                },
            );
        }
        if let Some(pin) = pin {
            let tools = pin.tools();
            let mut outputs = tools.output_declarations();
            for definition in tools.definitions() {
                let output = outputs.remove(definition.name());
                entries.insert(
                    format!("tool:{}", definition.name()),
                    AddonDiscoveryEntry::Tool { definition, output },
                );
            }
        }
        let entries = entries
            .into_iter()
            .map(|(key, value)| {
                // Tool metadata can exceed a page's narrower limit. Reject the capture;
                // do not truncate a schema or produce an unreachable continuation.
                let bytes = encoded_len(&(&key, &value))?;
                if bytes + 2 > MAXIMUM_ADDON_DISCOVERY_BYTES {
                    return Err(invalid("discovery entry exceeds 256 KiB"));
                }
                Ok(Entry { key, value, bytes })
            })
            .collect::<rsi_host::Result<Vec<_>>>()?;
        Ok(AddonDiscoverySnapshot(Arc::new(Snapshot {
            entries,
            _pin: pin.cloned(),
        })))
    }
}

impl AddonDiscoverySnapshot {
    /// Exact key lookup in this capture; keys use factory:, contract: and tool: prefixes.
    pub fn get(&self, key: &str) -> Option<&AddonDiscoveryEntry> {
        self.0
            .entries
            .binary_search_by(|entry| entry.key.as_str().cmp(key))
            .ok()
            .map(|position| &self.0.entries[position].value)
    }

    /// Returns a bounded page; rejects cursors from any other capture.
    pub fn page(
        &self,
        cursor: Option<&AddonDiscoveryCursor>,
    ) -> rsi_host::Result<AddonDiscoveryPage> {
        if cursor.is_some_and(|cursor| !Arc::ptr_eq(&self.0, &cursor.snapshot)) {
            return Err(invalid("discovery cursor belongs to another snapshot"));
        }
        let mut offset = cursor.map_or(0, |cursor| cursor.offset);
        let mut bytes = 2;
        let mut entries = Vec::new();
        for entry in &self.0.entries[offset..] {
            let next_bytes = bytes + entry.bytes + usize::from(!entries.is_empty());
            if entries.len() == MAXIMUM_ADDON_DISCOVERY_ITEMS
                || next_bytes > MAXIMUM_ADDON_DISCOVERY_BYTES
            {
                break;
            }
            entries.push((entry.key.clone(), entry.value.clone()));
            bytes = next_bytes;
            offset += 1;
        }
        let next = (offset < self.0.entries.len()).then(|| AddonDiscoveryCursor {
            snapshot: self.0.clone(),
            offset,
        });
        Ok(AddonDiscoveryPage { entries, next })
    }
}

fn invalid(message: &str) -> rsi_host::HostError {
    rsi_host::HostError::Bootstrap(message.into())
}

fn encoded_len(value: &impl Serialize) -> rsi_host::Result<usize> {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .filter(|length| *length <= MAXIMUM_ADDON_DISCOVERY_BYTES)
                .ok_or_else(|| std::io::Error::other("discovery entry too large"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    serde_json::to_writer(&mut counter, value)
        .map_err(|_| invalid("discovery entry exceeds 256 KiB"))?;
    Ok(counter.0)
}
