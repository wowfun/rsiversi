#![cfg(not(unix))]

use rsi_automation::{AdmissionError, AutomationFactory, Ledger, PolicyOwner};
use rsi_meta::PluginFactory as _;

#[test]
fn unsupported_owners_leave_missing_and_existing_directories_untouched() {
    let root = tempfile::tempdir().unwrap();
    for existing in [false, true] {
        let directory = root
            .path()
            .join(if existing { "existing" } else { "missing" });
        if existing {
            std::fs::create_dir(&directory).unwrap();
            std::fs::write(directory.join("sentinel"), b"retained").unwrap();
        }
        assert!(matches!(
            PolicyOwner::open(directory.clone()),
            Err(AdmissionError::Unsupported)
        ));
        assert!(matches!(
            Ledger::open(&directory, 1),
            Err(AdmissionError::Unsupported)
        ));
        if existing {
            assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
            assert_eq!(
                std::fs::read(directory.join("sentinel")).unwrap(),
                b"retained"
            );
        } else {
            assert!(!directory.exists());
        }
    }
}

#[test]
fn unsupported_factory_rejects_before_activation() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("automation");
    let error = AutomationFactory::default()
        .prepare(&serde_json::json!({ "directory": directory }))
        .unwrap_err();
    assert!(error.to_string().contains("unsupported on this platform"));
    assert!(!directory.exists());
}
