use super::*;
use rsi_agent_session_protocol::{ProjectionCursor, SessionProjectionSnapshot};

pub(super) fn snapshot(cursor: ProjectionCursor) -> Value {
    serde_json::to_value(
        SessionProjectionSnapshot::new(
            header().session_id().clone(),
            header().fingerprint().unwrap(),
            "a".repeat(64),
            cursor,
            Vec::new(),
        )
        .unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn projections_validate_both_bindings_and_each_cursor_before_exposure() {
    let (remote, _, handle) = fixture().await;
    let good = snapshot(ProjectionCursor::Durable {
        fact_seq: 8,
        control_seq: 6,
    });
    for (key, value) in [
        ("session_id", json!("foreign")),
        ("header_sha256", json!("b".repeat(64))),
        ("generation_sha256", json!("bad-hash")),
        ("generation_rejection", json!("x".repeat(4097))),
    ] {
        let mut bad = good.clone();
        bad[key] = value;
        remote.stream(&[envelope(bad)]);
        let mut source = handle.observe_projections().await.unwrap();
        assert!(source.next().await.unwrap().is_err());
    }
    for cursor in [
        ProjectionCursor::Durable {
            fact_seq: 7,
            control_seq: 7,
        },
        ProjectionCursor::Durable {
            fact_seq: 9,
            control_seq: 5,
        },
        ProjectionCursor::Draft { revision: 9 },
    ] {
        remote.stream(&[envelope(good.clone()), envelope(snapshot(cursor))]);
        let mut source = handle.observe_projections().await.unwrap();
        source.next().await.unwrap().unwrap();
        assert!(source.next().await.unwrap().is_err());
    }
    remote.stream(&[
        envelope(snapshot(ProjectionCursor::Draft { revision: 2 })),
        envelope(snapshot(ProjectionCursor::Draft { revision: 3 })),
        envelope(good.clone()),
        envelope(snapshot(ProjectionCursor::Durable {
            fact_seq: 8,
            control_seq: 7,
        })),
    ]);
    let mut source = handle.observe_projections().await.unwrap();
    for _ in 0..4 {
        source.next().await.unwrap().unwrap();
    }
    assert!(source.next().await.is_none());
    let mut rebuilding = good.clone();
    rebuilding["generation_rebuilding"] = json!(true);
    remote.stream(&[envelope(rebuilding), envelope(good.clone())]);
    let mut source = handle.observe_projections().await.unwrap();
    let rebuilding = source.next().await.unwrap().unwrap();
    let ready = source.next().await.unwrap().unwrap();
    assert!(rebuilding.snapshot().generation_rebuilding());
    assert!(!ready.snapshot().generation_rebuilding());
    assert_eq!(rebuilding.snapshot().cursor(), ready.snapshot().cursor());
    let mut rejected = good.clone();
    rejected["generation_rejection"] = json!("candidate state rejected");
    remote.stream(&[envelope(rejected), envelope(good)]);
    let mut source = handle.observe_projections().await.unwrap();
    let rejected = source.next().await.unwrap().unwrap();
    let cleared = source.next().await.unwrap().unwrap();
    assert_eq!(rejected.snapshot().cursor(), cleared.snapshot().cursor());
    assert_eq!(
        rejected.snapshot().generation_rejection(),
        Some("candidate state rejected")
    );
    assert!(cleared.snapshot().generation_rejection().is_none());
}

#[tokio::test]
async fn decoded_projection_ownership_outlives_wire_bytes_until_the_last_clone() {
    let (remote, client, handle) = fixture().await;
    remote.stream(&[envelope(snapshot(ProjectionCursor::Draft { revision: 0 }))]);
    let mut source = handle.observe_projections().await.unwrap();
    let retained = source.next().await.unwrap().unwrap();
    assert_eq!(remote.output.used(), 0);
    let bytes = client.state.projections.retained_bytes();
    assert!(bytes > 0);
    let clone = retained.clone();
    drop(retained);
    drop(source);
    assert_eq!(client.state.projections.retained_bytes(), bytes);
    drop(clone);
    assert_eq!(client.state.projections.retained_bytes(), 0);
}
