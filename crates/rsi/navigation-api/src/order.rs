use super::{
    ApiError, Deserialize, HostEpoch, NavigationClient, NavigationEntry, NavigationFilter,
    NavigationOperation, Never, Result, Serialize, SessionId, call_json, revision,
};
use rsi_agent_session_protocol::ExecutionCoordinates;
use rsi_agent_store_protocol::{MAXIMUM_ORDER_MEMBERS, MAXIMUM_ORDER_SEED_BYTES};

/// Complete ordering scope independent of search, archive and pin filtering.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OrderScope {
    /// Flat membership across all execution coordinates.
    All,
    /// One exact machine and canonical workspace path.
    Coordinates {
        /// Immutable coordinates, never execution authority.
        coordinates: ExecutionCoordinates,
    },
}
impl OrderScope {
    /// Optional exact Store index scope.
    pub fn coordinates(&self) -> Option<&ExecutionCoordinates> {
        match self {
            Self::All => None,
            Self::Coordinates { coordinates } => Some(coordinates),
        }
    }
}
/// Partition metadata accompanying one complete member identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderMember {
    /// Lossless initial activity rank, independent of later live changes.
    pub last_activity_ms: String,
    /// Index of the exact execution coordinate group.
    pub group: u16,
    /// Exact durable identity.
    pub session: SessionId,
    /// Shared pin partition; moves cannot cross it.
    pub pinned: bool,
    /// Shared archive partition.
    pub archived: bool,
}
/// Complete membership, or explicit capacity refusal.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OrderMembership {
    /// Every identity in the scope, strictly ascending.
    Available {
        /// Unique execution coordinate groups in first-member order.
        groups: Vec<ExecutionCoordinates>,
        /// Reconcile all members before reading summary pages.
        members: Vec<OrderMember>,
    },
    /// Preserve saved order and continue with updated ordering.
    TooLarge,
}
/// Exact complete seed for device-local manual ordering.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderSeed {
    /// Scope selected by the caller.
    pub scope: OrderScope,
    /// Host generation that produced this seed.
    pub host_epoch: HostEpoch,
    /// Shared partition metadata revision.
    pub metadata_revision: String,
    /// Complete bounded membership or explicit refusal.
    pub membership: OrderMembership,
}
impl OrderSeed {
    /// Checks remote identities, partitions and complete encoded size.
    pub fn validate(&self) -> Result<()> {
        self.validate_membership()?;
        if self.exceeds_bytes()? {
            return Err(ApiError::Invalid("order seed exceeds 128 KiB".into()));
        }
        Ok(())
    }
    /// Produces a complete server seed or an explicit byte-budget refusal.
    /// Malformed membership remains an error, never a capacity result.
    pub fn bounded(mut self) -> Result<Self> {
        self.validate_membership()?;
        if self.exceeds_bytes()? {
            self.membership = OrderMembership::TooLarge;
        }
        Ok(self)
    }
    fn validate_membership(&self) -> Result<()> {
        revision(&self.metadata_revision)?;
        if let OrderMembership::Available { members, .. } = &self.membership {
            for member in members {
                if revision(&member.last_activity_ms)? == 0 {
                    return Err(ApiError::Invalid("invalid order member activity".into()));
                }
            }
        }
        if let OrderMembership::Available { members, groups } = &self.membership
            && (members.len() > MAXIMUM_ORDER_MEMBERS
                || !valid_groups(groups, members, self.scope.coordinates())
                || members
                    .windows(2)
                    .any(|pair| pair[0].session >= pair[1].session)
                || members
                    .iter()
                    .any(|member| member.archived && member.pinned))
        {
            return Err(ApiError::Invalid(
                "invalid complete order membership".into(),
            ));
        }
        Ok(())
    }
    fn exceeds_bytes(&self) -> Result<bool> {
        Ok(serde_json::to_vec(self)
            .map_err(|_| ApiError::Invalid("invalid order seed".into()))?
            .len()
            > MAXIMUM_ORDER_SEED_BYTES)
    }
}
/// Exact ordered summary batch under shared metadata CAS.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SummaryRequest {
    /// Up to 64 distinct identities in device order.
    pub sessions: Vec<SessionId>,
    /// Revision of the seed being presented.
    pub metadata_revision: String,
}
impl SummaryRequest {
    /// Validates external bounds before Store access.
    pub fn validate(&self) -> Result<()> {
        revision(&self.metadata_revision)?;
        rsi_agent_store_protocol::validate_activity_summaries(&self.sessions)
            .map_err(|_| ApiError::Invalid("summary request exceeds 64 unique identities".into()))
    }
}
/// Ordered summaries; absent identities retain their requested positions.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SummaryPage {
    /// Exact shared metadata revision.
    pub metadata_revision: String,
    /// Exactly one optional row per requested identity.
    pub entries: Vec<Option<NavigationEntry>>,
}

impl NavigationClient {
    /// Reads complete membership before reconciling device order.
    pub async fn order_seed(&self, scope: OrderScope) -> Result<OrderSeed> {
        let value = match call_json::<_, OrderSeed, Never>(
            self.api.as_ref(),
            &NavigationOperation::OrderSeed.spec(),
            &scope,
        )
        .await?
        {
            Ok(value) => value,
            Err(never) => match never {},
        };
        value.validate()?;
        if value.scope != scope || value.host_epoch != self.api.description().host_epoch {
            return Err(ApiError::Invalid("order seed scope or Host changed".into()));
        }
        Ok(value)
    }
    /// Reads a device-ordered batch without history or Session attachment.
    pub async fn summaries(&self, request: SummaryRequest) -> Result<SummaryPage> {
        request.validate()?;
        let page = match call_json::<_, SummaryPage, Never>(
            self.api.as_ref(),
            &NavigationOperation::Summaries.spec(),
            &request,
        )
        .await?
        {
            Ok(value) => value,
            Err(never) => match never {},
        };
        if page.metadata_revision != request.metadata_revision
            || page.entries.len() != request.sessions.len()
        {
            return Err(ApiError::Invalid(
                "summary identity or revision changed".into(),
            ));
        }
        for (id, value) in request.sessions.iter().zip(&page.entries) {
            if let Some(value) = value {
                super::validation::entry(
                    value,
                    &NavigationFilter {
                        archived: value.metadata.archived,
                        ..Default::default()
                    },
                )?;
                if &value.session != id {
                    return Err(ApiError::Invalid("summary identity changed".into()));
                }
            }
        }
        Ok(page)
    }
}

fn valid_groups(
    groups: &[ExecutionCoordinates],
    members: &[OrderMember],
    scope: Option<&ExecutionCoordinates>,
) -> bool {
    if groups.len() > members.len()
        || groups
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != groups.len()
    {
        return false;
    }
    let mut used = std::collections::BTreeSet::new();
    for member in members {
        let index = usize::from(member.group);
        let Some(coordinates) = groups.get(index) else {
            return false;
        };
        if scope.is_some_and(|scope| coordinates != scope)
            || !used.contains(&index) && index != used.len()
        {
            return false;
        }
        used.insert(index);
    }
    used.len() == groups.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn seed() -> OrderSeed {
        OrderSeed {
            scope: OrderScope::All,
            host_epoch: HostEpoch::from_bytes([1; 16]),
            metadata_revision: "1".into(),
            membership: OrderMembership::Available {
                groups: vec![
                    ExecutionCoordinates::new(
                        rsi_agent_session_protocol::ExecutionLocation::Local,
                        "/workspace",
                    )
                    .unwrap(),
                ],
                members: vec![OrderMember {
                    last_activity_ms: "1".into(),
                    group: 0,
                    session: SessionId::new("one").unwrap(),
                    pinned: false,
                    archived: false,
                }],
            },
        }
    }
    #[test]
    fn malformed_seed_remains_an_error_instead_of_capacity() {
        let mut seed = seed();
        if let OrderMembership::Available { members, .. } = &mut seed.membership {
            members[0].group = 1;
        }
        assert!(matches!(seed.bounded(), Err(ApiError::Invalid(_))));
    }
    #[test]
    fn valid_membership_with_an_oversized_wire_envelope_is_explicitly_too_large() {
        let mut seed = seed();
        if let OrderMembership::Available { members, .. } = &mut seed.membership {
            *members = (0..1024)
                .map(|i| OrderMember {
                    last_activity_ms: "1".into(),
                    group: 0,
                    session: SessionId::new(format!("{i:04}{}", "x".repeat(200))).unwrap(),
                    pinned: false,
                    archived: false,
                })
                .collect();
        }
        assert!(matches!(
            seed.bounded().unwrap().membership,
            OrderMembership::TooLarge
        ));
    }
}
