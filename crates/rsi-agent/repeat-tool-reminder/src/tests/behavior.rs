use super::*;

#[tokio::test]
async fn source_order_thresholds_failed_results_and_replay_share_one_cursor() {
    let mut f = Fixture::new(Config::default());
    f.input(true);
    for (count, expected) in [(2, 0), (1, 1), (2, 1), (3, 1), (2, 0)] {
        let settled: Vec<_> = (0..count)
            .map(|_| f.call("bash", json!({"command":"exit 7"})))
            .collect();
        let before = settled.clone();
        let output = f.run(&settled).await;
        assert_eq!(output.inputs.len(), expected);
        assert_eq!(settled, before, "advice cannot rewrite results");
        f.apply(&output);
        let reader = f.capture();
        let replay = f
            .reminder
            .contribute(&f.context, &settled, CancellationToken::new())
            .await
            .unwrap();
        assert!(replay.inputs.is_empty() && replay.domains.is_empty());
        assert!(
            reader.reads.lock().unwrap().is_empty(),
            "no history rescan on replay"
        );
    }
    assert_eq!(f.state().count, 10);
    let calls = [
        f.call("bash", json!({"command":"exit 7"})),
        f.call("read", json!({})),
        f.call("bash", json!({"command":"exit 7"})),
    ];
    let output = f.run(&calls).await;
    assert!(output.inputs.is_empty());
    f.apply(&output);
    assert_eq!(
        f.state().count,
        1,
        "interleaved tracked call breaks the chain"
    );
}

#[tokio::test]
async fn complete_nested_matching_and_transparent_filters() {
    let config = Config::parse(
        &json!({"thresholds":[2],"include":["read","ignored"],"exclude":["ignored"]}),
    )
    .unwrap();
    let mut f = Fixture::new(config);
    let calls = [
        f.call("read", json!({"a":{"x":1,"y":2},"b":3})),
        f.call("ignored", json!({})),
        f.call("outside", json!({})),
        f.call("read", json!({"b":3,"a":{"y":2,"x":1}})),
    ];
    let output = f.run(&calls).await;
    assert_eq!(output.inputs.len(), 1);
    f.apply(&output);
    assert_eq!(f.state().count, 2);
    let call = f.call("read", json!({"b":3,"a":{"y":3,"x":1}}));
    let output = f.run(&[call]).await;
    assert!(output.inputs.is_empty());
    f.apply(&output);
    assert_eq!(f.state().count, 1);
}

#[tokio::test]
async fn only_human_input_resets_within_turn_and_child_or_next_turn_reset_inherited_state() {
    let mut f = Fixture::new(Config::parse(&json!({"thresholds":[2]})).unwrap());
    let call = f.call("read", json!({}));
    let output = f.run(&[call]).await;
    f.apply(&output);
    f.input(false);
    let call = f.call("read", json!({}));
    let output = f.run(&[call]).await;
    assert_eq!(output.inputs.len(), 1);
    f.apply(&output);
    let before = f.call("read", json!({}));
    f.input(true);
    let after = f.call("read", json!({}));
    let output = f.run(&[before, after]).await;
    assert!(output.inputs.is_empty());
    f.apply(&output);
    assert_eq!(f.state().count, 1);
    for child in [false, true] {
        f.context.accepted_fact_seq = f.facts.len() as u64 + 1;
        if child {
            f.context.header = Arc::new(header("child"));
        } else {
            f.context.turn_id = TurnId::new("next").unwrap();
        }
        let call = f.call("read", json!({}));
        let reader = f.capture();
        let output = f
            .reminder
            .contribute(&f.context, &[call], CancellationToken::new())
            .await
            .unwrap();
        assert!(output.inputs.is_empty());
        assert_eq!(
            reader.reads.lock().unwrap()[0],
            f.context.accepted_fact_seq - 1
        );
        f.apply(&output);
        assert_eq!(f.state().count, 1);
        assert_eq!(
            f.state().session.as_ref(),
            Some(f.context.header.session_id())
        );
    }
}
