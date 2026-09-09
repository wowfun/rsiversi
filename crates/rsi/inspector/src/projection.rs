use crate::PageRequest;
use rsi_meta::{
    InspectedCleanupState, InspectedCollection, InspectedFiber, InspectedFiberState,
    InspectedOwner, InspectedService, ResourceUsageSnapshot, RuntimeInspection,
    RuntimeResourceSnapshot, UpdateMode,
};
use rsi_meta_profile::{
    ProfileHealth, ProfileSnapshot, ProfileStatus, SnapshotNode, WatcherHealth,
};
use serde_json::{Value, json};

pub(super) fn runtime(value: RuntimeInspection) -> Value {
    json!({
        "revision": value.revision.to_string(), "shutting_down": value.shutting_down,
        "terminal": value.terminal, "total_fibers": value.total_fibers,
        "next_after": value.next_after.map(|id| id.0.to_string()),
        "fibers": value.fibers.into_iter().map(fiber).collect::<Vec<_>>(),
        "resources": value.resources.as_ref().map(resources),
    })
}
fn owner(value: InspectedOwner) -> Value {
    json!({"fiber": value.fiber.0.to_string(), "generation": value.generation.0.to_string()})
}
fn service(value: InspectedService) -> Value {
    match value {
        InspectedService::Local { key, isolation } => {
            json!({"kind": "local", "key": key.as_str(), "isolation": isolation.0.to_string()})
        }
        InspectedService::Portable {
            key,
            isolation,
            contract,
            version,
        } => {
            json!({"kind": "portable", "key": key.as_str(), "isolation": isolation.0.to_string(), "contract": contract, "version": version})
        }
    }
}
fn collection<T>(value: InspectedCollection<T>, project: impl Fn(T) -> Value) -> Value {
    json!({"total": value.total, "items": value.items.into_iter().map(project).collect::<Vec<_>>()})
}
fn fiber(value: InspectedFiber) -> Value {
    let state = match value.state {
        InspectedFiberState::Pending => "pending",
        InspectedFiberState::Loading => "loading",
        InspectedFiberState::Active => "active",
        InspectedFiberState::Failed => "failed",
        InspectedFiberState::Unloading => "unloading",
        InspectedFiberState::Disposed => "disposed",
    };
    let update_mode = match value.update_mode {
        UpdateMode::Replayable => "replayable",
        UpdateMode::RestartRequired => "restart_required",
    };
    let dependencies = collection(
        value.dependencies,
        |value| json!({"service": service(value.service), "provider": value.provider.map(|value| json!({"owner": owner(value.owner), "supply_token": value.supply_token.to_string()}))}),
    );
    let supplies = collection(
        value.supplies,
        |value| json!({"service": service(value.service), "supply_token": value.supply_token.to_string(), "generation_published": value.generation_published}),
    );
    let effects = collection(value.effects, |value| {
        let cleanup = match value.cleanup {
            InspectedCleanupState::Unclaimed => "unclaimed",
            InspectedCleanupState::Claimed => "claimed",
            InspectedCleanupState::Running => "running",
            InspectedCleanupState::Complete => "complete",
        };
        json!({"id": value.id.to_string(), "open": value.open, "cleanup": cleanup, "cleanup_failures": value.cleanup_failures, "queued_entries": value.queued_entries})
    });
    json!({
        "id": value.id.0.to_string(), "generation": value.generation.0.to_string(),
        "factory": value.factory, "update_mode": update_mode, "state": state,
        "parent": value.parent.map(owner), "order": value.order.map(|positions| positions.into_iter().map(|position| position.to_string()).collect::<Vec<_>>()),
        "dependencies": dependencies, "supplies": supplies, "effects": effects,
        "retained_effect_entries": value.retained_effect_entries, "retained_effect_transactions": value.retained_effect_transactions,
        "cleanup_phase": value.cleanup_phase, "listeners": value.listeners, "children": value.children,
    })
}
fn usage(value: &ResourceUsageSnapshot) -> Value {
    json!({"current": value.current.to_string(), "limit": value.limit.to_string(), "high_watermark": value.high_watermark.to_string(), "rejected": value.rejected.to_string()})
}
fn resources(value: &RuntimeResourceSnapshot) -> Value {
    json!({
        "preparations": usage(&value.preparations), "fibers": usage(&value.fibers),
        "retained_plugin_bytes": usage(&value.retained_plugin_bytes), "dependency_edges": usage(&value.dependency_edges),
        "services": usage(&value.services), "effects": usage(&value.effects),
        "effect_transactions": usage(&value.effect_transactions), "listeners": usage(&value.listeners),
        "capability_entries": usage(&value.capability_entries), "queued_capability_references": usage(&value.queued_capability_references),
        "service_calls": usage(&value.service_calls), "buffered_message_bytes": usage(&value.buffered_message_bytes),
        "pending_message_sends": usage(&value.pending_message_sends), "reconciliations": usage(&value.reconciliations),
        "scheduler_workers": usage(&value.scheduler_workers), "cleanup_runs": usage(&value.cleanup_runs),
    })
}
pub(super) fn profile(
    status: &ProfileStatus,
    snapshot: &ProfileSnapshot,
    request: PageRequest,
) -> Value {
    let mut total = 0;
    let mut rows = Vec::new();
    nodes(snapshot.nodes(), None, &mut total, &mut rows, request);
    let next = request.offset.saturating_add(rows.len());
    let health = match status.health() {
        ProfileHealth::Converging => "converging",
        ProfileHealth::Converged => "converged",
        ProfileHealth::Degraded => "degraded",
        ProfileHealth::RestartRequired => "restart_required",
        ProfileHealth::Stopped => "stopped",
    };
    let watcher = match status.watcher() {
        WatcherHealth::Inactive => "inactive",
        WatcherHealth::Healthy => "healthy",
        WatcherHealth::Faulted => "faulted",
    };
    json!({
        "revision": snapshot.revision().to_string(), "source_digest": snapshot.source_digest(),
        "status_revision": status.revision().to_string(), "status_source_digest": status.source_digest(),
        "health": health, "watcher": watcher, "total": total,
        "next_offset": (next < total).then_some(next), "nodes": rows,
    })
}
fn nodes(
    values: &[SnapshotNode],
    parent: Option<&str>,
    total: &mut usize,
    rows: &mut Vec<Value>,
    request: PageRequest,
) {
    for node in values {
        if *total >= request.offset && rows.len() < request.limit {
            rows.push(json!({"id": node.id(), "parent": parent, "plugin": node.plugin(), "enabled": node.enabled()}));
        }
        *total += 1;
        nodes(node.children(), Some(node.id()), total, rows, request);
    }
}
