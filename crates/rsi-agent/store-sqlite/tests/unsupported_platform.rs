#![cfg(not(any(unix, windows)))]

use rsi_agent_store_protocol::StoreError;
use rsi_agent_store_sqlite::SqliteStore;

#[test]
fn unsupported_open_verify_and_reset_leave_the_root_and_old_data_untouched() {
    let temporary = tempfile::tempdir().unwrap();
    for exists in [false, true] {
        let root = temporary
            .path()
            .join(if exists { "existing" } else { "missing" });
        if exists {
            std::fs::create_dir(&root).unwrap();
            std::fs::write(root.join("sessions.sqlite3"), b"retained").unwrap();
        }
        assert!(matches!(
            SqliteStore::open(&root),
            Err(StoreError::Unsupported(_))
        ));
        assert!(matches!(
            SqliteStore::verify(&root),
            Err(StoreError::Unsupported(_))
        ));
        let inspection = SqliteStore::inspect_cas(
            &root,
            rsi_agent_store_sqlite::CasInspectionMode::Metadata,
            &tokio_util::sync::CancellationToken::new(),
            |_| std::ops::ControlFlow::Continue(()),
        )
        .unwrap_err();
        assert!(matches!(inspection.source, StoreError::Unsupported(_)));
        let failure = SqliteStore::reset_and_open(&root).unwrap_err();
        assert!(matches!(failure.source, StoreError::Unsupported(_)));
        assert!(failure.receipt.backup.is_none());
        assert_eq!(failure.receipt.root, root);
        if exists {
            assert_eq!(
                std::fs::read(root.join("sessions.sqlite3")).unwrap(),
                b"retained"
            );
            assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        } else {
            assert!(!root.exists());
        }
    }
    assert_eq!(std::fs::read_dir(temporary.path()).unwrap().count(), 1);
}
