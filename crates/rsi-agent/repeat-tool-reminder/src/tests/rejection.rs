use super::*;

#[tokio::test]
async fn invalid_or_missing_intents_and_cancellation_never_propose_partial_state() {
    let mut f = Fixture::new(Config::default());
    let call = f.call("read", json!({}));
    f.capture();
    for batch in [
        vec![call.clone(), call.clone()],
        vec![f.facts[0].clone()],
        vec![call.clone(); 257],
    ] {
        assert!(
            f.reminder
                .contribute(&f.context, &batch, CancellationToken::new())
                .await
                .is_err()
        );
    }
    let token = CancellationToken::new();
    token.cancel();
    assert!(matches!(
        f.reminder
            .contribute(&f.context, std::slice::from_ref(&call), token)
            .await,
        Err(ContributionError::Closed)
    ));
    f.context.facts = Arc::new(Reader {
        facts: vec![call.clone()],
        reads: Mutex::default(),
    });
    assert!(
        f.reminder
            .contribute(
                &f.context,
                std::slice::from_ref(&call),
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    f.context.turn_id = TurnId::new("other").unwrap();
    assert!(
        f.reminder
            .contribute(&f.context, &[call], CancellationToken::new())
            .await
            .is_err()
    );
    assert_eq!(f.state(), State::default());
}

#[test]
fn configuration_and_durable_state_reject_ambiguous_or_unbounded_values() {
    for value in [
        json!({"unknown":true}),
        json!({"thresholds":[]}),
        json!({"thresholds":[1]}),
        json!({"thresholds":[2,2]}),
        json!({"thresholds":[1_000_001]}),
        json!({"thresholds":(2..35).collect::<Vec<_>>()}),
        json!({"include":["a","a"]}),
        json!({"exclude":["has space"]}),
        json!({"include":(0..65).map(|n|format!("tool{n}")).collect::<Vec<_>>()}),
    ] {
        assert!(
            RepeatToolReminderFactory.prepare(&value).is_err(),
            "{value}"
        );
    }
    assert!(
        RepeatToolReminderFactory
            .prepare(&ConfigValue::Null)
            .is_ok()
    );
    assert!(
        RepeatToolReminderFactory
            .prepare(&json!({"thresholds":[1_000_000,2],"include":[]}))
            .is_ok()
    );
    let valid = State {
        session: Some(SessionId::new("s").unwrap()),
        turn: Some(TurnId::new("t").unwrap()),
        through_fact_seq: 2,
        name: Some("read".into()),
        signature: Some("a".repeat(64)),
        count: MAXIMUM_COUNT,
    };
    assert!(valid.validate().is_ok());
    for invalid in [
        State {
            session: None,
            ..valid.clone()
        },
        State {
            through_fact_seq: 0,
            ..valid.clone()
        },
        State {
            signature: Some("A".repeat(64)),
            ..valid.clone()
        },
        State {
            count: MAXIMUM_COUNT + 1,
            ..valid
        },
    ] {
        assert!(invalid.validate().is_err());
    }
}
