use rsi_agent_presets::{
    EXECUTOR_FACTORY, KERNEL_FACTORY, SESSION_EXECUTOR_INSTANCE, SESSION_FRAGMENT_ID,
    SESSION_KERNEL_INSTANCE, SESSION_STORE_INSTANCE, SQLITE_STORE_FACTORY, SessionAgentConfig,
    session_fragment,
};

#[test]
fn session_fragment_has_fixed_order_and_explicit_authority() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("sessions");
    let config = SessionAgentConfig::new(&root)
        .unwrap()
        .with_executor_id("executor-test")
        .unwrap();
    let fragment = session_fragment(&config);

    assert_eq!(fragment.id(), SESSION_FRAGMENT_ID);
    let entries = fragment.entries();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].id().as_str(), SESSION_STORE_INSTANCE);
    assert_eq!(entries[0].plugin().as_str(), SQLITE_STORE_FACTORY);
    assert_eq!(entries[0].config()["root"], root.to_string_lossy().as_ref());
    assert_eq!(entries[1].id().as_str(), SESSION_KERNEL_INSTANCE);
    assert_eq!(entries[1].plugin().as_str(), KERNEL_FACTORY);
    assert!(entries[1].config().is_null());
    assert_eq!(entries[2].id().as_str(), SESSION_EXECUTOR_INSTANCE);
    assert_eq!(entries[2].plugin().as_str(), EXECUTOR_FACTORY);
    assert_eq!(entries[2].config()["executor_id"], "executor-test");
}

#[test]
fn rejects_ambient_or_invalid_identity_inputs() {
    assert!(SessionAgentConfig::new("relative").is_err());
    let temporary = tempfile::tempdir().unwrap();
    assert!(
        SessionAgentConfig::new(temporary.path())
            .unwrap()
            .with_executor_id("contains space")
            .is_err()
    );
    assert!(
        SessionAgentConfig::new(temporary.path())
            .unwrap()
            .with_executor_id("contains/slash")
            .is_err()
    );
}
