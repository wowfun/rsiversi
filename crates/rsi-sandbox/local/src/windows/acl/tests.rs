use super::*;

fn fixture() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(".rsi-acl-fixture-")
        .tempdir_in(std::env::current_dir().unwrap())
        .unwrap()
}

#[test]
#[ignore = "native Windows ACL integration; edits only a new owned directory"]
fn concurrent_workspace_grants_merge_and_exact_grants_are_reused() {
    let directory = fixture();
    let path = canonical(directory.path().canonicalize().unwrap());
    let capabilities: Vec<_> = (1..=4)
        .map(|n| format!("S-1-4-781912345-903812345-{n}"))
        .collect();
    std::thread::scope(|scope| {
        for capability in &capabilities {
            let path = &path;
            scope.spawn(move || {
                assert!(grant_workspace(path, &sid(capability).unwrap()).unwrap());
            });
        }
    });
    let current = Security::read(&path).unwrap();
    for capability in &capabilities {
        let capability = sid(capability).unwrap();
        assert!(
            exact(
                &current,
                current.dacl,
                ACCESS_ALLOWED_ACE_TYPE,
                INHERIT,
                GRANT_MASK,
                capability.0
            )
            .unwrap()
        );
        assert!(!grant_workspace(&path, &capability).unwrap());
    }
}

#[test]
#[ignore = "native Windows ACL integration; edits only a new owned directory"]
fn revocation_preserves_another_capability_and_its_low_label() {
    let directory = fixture();
    let path = canonical(directory.path().canonicalize().unwrap());
    let first = sid("S-1-4-781912345-903812345-1").unwrap();
    let second = sid("S-1-4-781912345-903812345-2").unwrap();
    let low = sid("S-1-16-4096").unwrap();
    grant_private(&path, &first).unwrap();
    grant_private(&path, &second).unwrap();
    revoke_private(&path, &first).unwrap();
    let current = Security::read(&path).unwrap();
    assert!(
        !exact(
            &current,
            current.dacl,
            ACCESS_ALLOWED_ACE_TYPE,
            INHERIT,
            GRANT_MASK,
            first.0
        )
        .unwrap()
    );
    assert!(
        exact(
            &current,
            current.dacl,
            ACCESS_ALLOWED_ACE_TYPE,
            INHERIT,
            GRANT_MASK,
            second.0
        )
        .unwrap()
    );
    assert!(
        exact(
            &current,
            current.label,
            SYSTEM_MANDATORY_LABEL_ACE_TYPE,
            INHERIT,
            SYSTEM_MANDATORY_LABEL_NO_WRITE_UP,
            low.0
        )
        .unwrap()
    );
    revoke_private(&path, &second).unwrap();
    let current = Security::read(&path).unwrap();
    assert!(
        !exact(
            &current,
            current.dacl,
            ACCESS_ALLOWED_ACE_TYPE,
            INHERIT,
            GRANT_MASK,
            second.0
        )
        .unwrap()
    );
    assert!(
        !exact(
            &current,
            current.label,
            SYSTEM_MANDATORY_LABEL_ACE_TYPE,
            INHERIT,
            SYSTEM_MANDATORY_LABEL_NO_WRITE_UP,
            low.0
        )
        .unwrap()
    );
}
