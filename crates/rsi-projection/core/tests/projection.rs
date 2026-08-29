use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
use rsi_projection::{ProjectionFactory, ProjectionRegistryContract, ProjectionUnit, Result};
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Debug)]
struct Copy;

impl ProjectionUnit for Copy {
    fn project(&self, input: &Value) -> Result<Value> {
        Ok(input.clone())
    }
}

#[derive(Debug)]
struct Fixed(Value);

impl ProjectionUnit for Fixed {
    fn project(&self, _input: &Value) -> Result<Value> {
        Ok(self.0.clone())
    }
}

#[tokio::test]
async fn projection_is_derived_and_registration_is_lease_owned() {
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.projection",
                "test",
                UpdateMode::Replayable,
                Arc::new(ProjectionFactory),
            ),
            Value::Null,
        )
        .await
        .unwrap();
    let registry = runtime
        .root()
        .lookup_local::<ProjectionRegistryContract>()
        .unwrap();
    let lease = registry.register("copy", Arc::new(Copy)).unwrap();
    assert_eq!(
        registry.project_all(&json!({"raw":true})).unwrap()["copy"],
        json!({"raw":true})
    );
    let second = registry.register("copy-again", Arc::new(Copy)).unwrap();
    let large = json!({"value": "x".repeat(9 * 1024 * 1024)});
    assert!(registry.project_all(&large).is_err());
    drop(second);
    drop(lease);
    assert!(registry.project_all(&json!({})).unwrap().is_empty());
    drop(registry);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn complete_output_bound_includes_object_keys_and_json_structure() {
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.projection",
                "aggregate-bound",
                UpdateMode::Replayable,
                Arc::new(ProjectionFactory),
            ),
            Value::Null,
        )
        .await
        .unwrap();
    let registry = runtime
        .root()
        .lookup_local::<ProjectionRegistryContract>()
        .unwrap();
    let half = 8 * 1024 * 1024;
    let first = registry
        .register(
            "first",
            Arc::new(Fixed(Value::String("a".repeat(half - 2)))),
        )
        .unwrap();
    let second = registry
        .register(
            "second",
            Arc::new(Fixed(Value::String("b".repeat(half - 2)))),
        )
        .unwrap();

    assert!(registry.project_all(&Value::Null).is_err());

    drop(second);
    drop(first);
    drop(registry);
    assert!(fiber.dispose().await.is_clean());
}
