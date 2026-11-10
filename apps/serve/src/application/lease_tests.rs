use super::{Importer, Imports};
use rsi_packages::{
    Export, ExportKind, FileRecord, InstallSource, Manifest, PackageStore, PayloadHeader,
    Provenance,
};
use serde_json::json;
use sha2::{Digest as _, Sha256};
use std::{collections::BTreeMap, path::Path, sync::Arc};
use tokio_util::sync::CancellationToken;

fn install(store: &PackageStore, directory: &Path, version: u8) -> String {
    let directory =
        rsi_workspace_protocol::canonical_native_path(directory.canonicalize().unwrap());
    let data = [version];
    let entry = FileRecord {
        path: "entry".into(),
        bytes: 1,
        digest: hex::encode(Sha256::digest(data)),
        executable: false,
    };
    let header = serde_json::to_vec(&PayloadHeader {
        format: 1,
        files: vec![entry],
    })
    .unwrap();
    let mut archive = b"RSITREE1".to_vec();
    archive.extend(u32::try_from(header.len()).unwrap().to_le_bytes());
    archive.extend(header);
    let index_digest = hex::encode(Sha256::digest(&archive));
    archive.extend(data);
    std::fs::write(directory.join("payload"), &archive).unwrap();
    let manifest = Manifest {
        format: 2,
        name: "plugin".into(),
        version: format!("0.0.{version}"),
        target: "portable".into(),
        dependencies: vec![],
        requires: vec![],
        files: vec![FileRecord {
            path: "payload".into(),
            bytes: archive.len() as u64,
            digest: hex::encode(Sha256::digest(&archive)),
            executable: false,
        }],
        exports: BTreeMap::from([(
            "profile".into(),
            Export {
                kind: ExportKind::PayloadTree,
                path: "payload".into(),
                index_digest: Some(index_digest),
            },
        )]),
    };
    let source = InstallSource {
        directory,
        manifest: serde_json::to_vec(&manifest).unwrap(),
        provenance: Provenance::Local,
    };
    store
        .install(&[source], &[], &[], "portable")
        .unwrap()
        .packages["plugin"]
        .digest
        .clone()
}

#[tokio::test]
async fn releasing_a_digest_returns_capacity_and_permits_collection() {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(PackageStore::open(&root.path().join("store")).unwrap());
    let service = Imports {
        profile: "fixture".into(),
        selection: Importer {
            digest: "a".repeat(64),
            package: "unused".into(),
            export: "unused".into(),
            program: root.path().into(),
        },
        workspace: root.path().into(),
        store: store.clone(),
        lane: tokio::sync::Mutex::default(),
        leases: tokio::sync::Mutex::default(),
    };
    let stop = CancellationToken::new();
    let request = |digest: &str| json!({"package":"plugin","digest":digest,"export":"profile","dependency_export":"runtime"});
    let mut digests = Vec::new();
    for version in 0..32 {
        let digest = install(&store, root.path(), version);
        service.payloads(request(&digest), &stop).await.unwrap();
        digests.push(digest);
    }
    let next = install(&store, root.path(), 32);
    assert!(matches!(
        service.payloads(request(&next), &stop).await,
        Err(rsi_extension::Error::Capacity)
    ));
    let first = &digests[0];
    for _ in 0..2 {
        service.release(json!({"digest":first})).await.unwrap();
    }
    service.payloads(request(&next), &stop).await.unwrap();
    assert_eq!(
        store.collect().unwrap().as_slice(),
        std::slice::from_ref(first)
    );
}
