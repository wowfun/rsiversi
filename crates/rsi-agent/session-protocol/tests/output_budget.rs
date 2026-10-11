use rsi_agent_session_protocol::{
    BudgetDimension, EffectId, FrozenAgentSettings, MAXIMUM_TURN_OUTPUT_RECOVERIES,
    ModelOutputBudget, ModelSelection, OutputBudgetTable, SessionFact, SessionFactBody, TurnBudget,
    TurnId, TurnOutcome,
};
use rsi_ai_protocol::ModelRef;
use rsi_sandbox::SandboxMode;
use serde_json::json;

#[test]
fn output_budget_table_is_exact_bounded_and_decode_validated() {
    let model = ModelRef::new("a", "model").unwrap();
    let entry = ModelOutputBudget {
        model: model.clone(),
        max_output_tokens: 32768,
    };
    let table = OutputBudgetTable::new(vec![entry.clone()]).unwrap();
    assert_eq!(table.cap(&model), Some(32768));
    assert_eq!(table.cap(&ModelRef::new("b", "model").unwrap()), None);
    assert!(OutputBudgetTable::new(vec![entry.clone(), entry]).is_err());
    for cap in [0, 1_000_001] {
        assert!(
            OutputBudgetTable::new(vec![ModelOutputBudget {
                model: model.clone(),
                max_output_tokens: cap,
            }])
            .is_err()
        );
        assert!(
            serde_json::from_value::<OutputBudgetTable>(
                json!([{"model":{"deployment":"a","model":"model"},"max_output_tokens":cap}])
            )
            .is_err()
        );
    }
    let entries = (0..257)
        .map(|n| ModelOutputBudget {
            model: ModelRef::new("a", format!("m{n}")).unwrap(),
            max_output_tokens: 1,
        })
        .collect();
    assert!(OutputBudgetTable::new(entries).is_err());
    let large = (0..256)
        .map(|n| ModelOutputBudget {
            model: ModelRef::new("x".repeat(200), format!("m{n}{}", "y".repeat(200))).unwrap(),
            max_output_tokens: 1,
        })
        .collect();
    assert!(OutputBudgetTable::new(large).is_err());
    assert_eq!(
        serde_json::from_value::<OutputBudgetTable>(serde_json::to_value(&table).unwrap()).unwrap(),
        table
    );
}

#[test]
fn output_budget_table_accepts_the_exact_encoded_byte_limit() {
    let mut entries: Vec<_> = (0..256)
        .map(|n| ModelOutputBudget {
            model: ModelRef::new("a", format!("m{n:03}")).unwrap(),
            max_output_tokens: 1,
        })
        .collect();
    let remaining = 64 * 1024 - serde_json::to_vec(&entries).unwrap().len();
    for (index, entry) in entries.iter_mut().enumerate() {
        let padding = remaining / 256 + usize::from(index < remaining % 256);
        entry.model = ModelRef::new("a", format!("m{index:03}{}", "x".repeat(padding))).unwrap();
    }
    let wire = serde_json::to_vec(&entries).unwrap();
    assert_eq!(wire.len(), 64 * 1024);
    let table = OutputBudgetTable::new(entries.clone()).unwrap();
    assert_eq!(
        serde_json::from_slice::<OutputBudgetTable>(&wire).unwrap(),
        table
    );
    assert_eq!(table.cap(&entries[255].model), Some(1));

    entries[0].model = ModelRef::new("a", format!("{}x", entries[0].model.model())).unwrap();
    let oversized = serde_json::to_vec(&entries).unwrap();
    assert_eq!(oversized.len(), 64 * 1024 + 1);
    assert!(OutputBudgetTable::new(entries).is_err());
    assert!(serde_json::from_slice::<OutputBudgetTable>(&oversized).is_err());
}

#[test]
fn frozen_output_policy_is_independent_of_selection_and_validated_on_decode() {
    let default_model = ModelRef::new("a", "model").unwrap();
    let selected_model = ModelRef::new("b", "model").unwrap();
    let table = OutputBudgetTable::new(vec![ModelOutputBudget {
        model: selected_model.clone(),
        max_output_tokens: 32768,
    }])
    .unwrap();
    let settings = FrozenAgentSettings::new(
        "test",
        "system",
        default_model.clone(),
        SandboxMode::WorkspaceWrite,
        false,
    )
    .unwrap()
    .with_output_budgets(table.clone());
    settings.validate().unwrap();
    assert_eq!(settings.output_budgets().cap(&default_model), None);
    let settings = settings
        .with_model_selection(ModelSelection {
            model: selected_model.clone(),
            reasoning_effort: None,
        })
        .unwrap();
    assert_eq!(settings.output_budgets(), &table);
    assert_eq!(settings.output_budgets().cap(&selected_model), Some(32768));
    let wire = serde_json::to_value(&settings).unwrap();
    assert_eq!(
        serde_json::from_value::<FrozenAgentSettings>(wire.clone()).unwrap(),
        settings
    );
    for cap in [0, 1_000_001] {
        let mut invalid = wire.clone();
        invalid["output_budgets"][0]["max_output_tokens"] = json!(cap);
        assert!(serde_json::from_value::<FrozenAgentSettings>(invalid).is_err());
    }
}

#[test]
fn output_recovery_ordinals_are_bounded_at_fact_construction() {
    for ordinal in [
        0,
        1,
        MAXIMUM_TURN_OUTPUT_RECOVERIES,
        MAXIMUM_TURN_OUTPUT_RECOVERIES + 1,
        u64::MAX,
    ] {
        let fact = SessionFact::new(
            1,
            42,
            SessionFactBody::ModelOutputRecovery {
                turn_id: TurnId::new("turn").unwrap(),
                source_model_effect_id: EffectId::new("source").unwrap(),
                ordinal,
            },
        );
        assert_eq!(
            fact.is_ok(),
            (1..=MAXIMUM_TURN_OUTPUT_RECOVERIES).contains(&ordinal)
        );
    }
}

#[test]
fn zero_recovery_limit_is_valid_only_for_output_exhaustion() {
    assert_eq!(TurnBudget::default().maximum_output_recoveries(), 2);
    for limit in [0, 2, 16] {
        let budget = TurnBudget::default()
            .with_maximum_output_recoveries(limit)
            .unwrap();
        assert_eq!(
            serde_json::from_value::<TurnBudget>(serde_json::to_value(budget).unwrap())
                .unwrap()
                .maximum_output_recoveries(),
            limit
        );
    }
    assert!(
        TurnBudget::default()
            .with_maximum_output_recoveries(17)
            .is_err()
    );
    assert!(
        TurnOutcome::BudgetExceeded {
            dimension: BudgetDimension::OutputRecoveries,
            consumed: 0,
            limit: 0
        }
        .validate()
        .is_ok()
    );
    assert!(
        TurnOutcome::BudgetExceeded {
            dimension: BudgetDimension::ProviderAttempts,
            consumed: 0,
            limit: 0
        }
        .validate()
        .is_err()
    );
}
