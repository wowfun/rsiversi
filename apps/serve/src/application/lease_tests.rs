use super::{Importer, Imports};
use rsi_extension::Handler;
use rsi_packages::{
    Export, ExportKind, FileRecord, InstallSource, Manifest, PackageStore, PayloadHeader,
    Provenance,
};
use serde_json::json;
use sha2::{Digest as _, Sha256};
use std::{collections::BTreeMap, path::Path, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

fn install(store: &PackageStore, directory: &Path, version: u8) -> String {
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
        directory: directory.to_owned(),
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
    let root = tempfile::tempdir_in(rsi_workspace_protocol::canonical_native_path(
        std::env::temp_dir().canonicalize().unwrap(),
    ))
    .unwrap();
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
        temporary_root: root.path().into(),
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
    let held = store.acquire_for_process("plugin", first).unwrap();
    for id in ["one", "two"] {
        service.leases.lock().await.stages.insert(
            id.into(),
            rsi_packages::StagedPackages {
                packages: BTreeMap::from([("plugin".into(), first.clone())]),
                leases: BTreeMap::from([(first.clone(), held.clone())]),
            },
        );
    }
    drop(held);
    for _ in 0..2 {
        service.release(json!({"digest":first})).await.unwrap();
    }
    assert!(matches!(
        service.payloads(request(&next), &stop).await,
        Err(rsi_extension::Error::Capacity)
    ));
    service.release_stage(json!({"stage":"one"})).await.unwrap();
    assert!(store.collect().unwrap().is_empty());
    for _ in 0..2 {
        service.release_stage(json!({"stage":"two"})).await.unwrap();
    }
    service.payloads(request(&next), &stop).await.unwrap();
    assert_eq!(
        store.collect().unwrap().as_slice(),
        std::slice::from_ref(first)
    );
}

fn stage_service() -> (tempfile::TempDir, Arc<Imports>) {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    std::fs::create_dir(&source).unwrap();
    let hash = |bytes: &[u8]| hex::encode(Sha256::digest(bytes));
    let script = include_bytes!("../../../cli/tests/fixtures/package-importer.mjs");
    let header = serde_json::to_vec(&json!({"format":1,"files":[{"path":"entry.mjs","bytes":script.len(),"digest":hash(script),"executable":false}]})).unwrap();
    let mut payload = b"RSITREE1".to_vec();
    payload.extend(u32::try_from(header.len()).unwrap().to_le_bytes());
    payload.extend(header);
    let index_digest = hash(&payload);
    payload.extend(script);
    std::fs::write(source.join("payload.rsi"), &payload).unwrap();
    let descriptor = serde_json::to_vec(&json!({"format":1,"cwd":{"payload":"runtime","path":""},
        "arguments":[{"kind":"path","location":{"payload":"runtime","path":"entry.mjs"}},{"kind":"generation"}]})).unwrap();
    std::fs::write(source.join("import.json"), &descriptor).unwrap();
    let manifest = serde_json::to_vec(&json!({"format":2,"name":"importer","version":"1","target":"portable","dependencies":[],
        "requires":[{"name":"rsi.extension.process","version":2},{"name":"rsi.packages.importer","version":1}],
        "files":[{"path":"payload.rsi","bytes":payload.len(),"digest":hash(&payload),"executable":false},
            {"path":"import.json","bytes":descriptor.len(),"digest":hash(&descriptor),"executable":false}],
        "exports":{"runtime":{"kind":"payload_tree","path":"payload.rsi","index_digest":index_digest},"import":{"kind":"package_importer","path":"import.json"}}})).unwrap();
    let store = Arc::new(PackageStore::open(&root.path().join("store")).unwrap());
    let protocols = crate::supported_protocols();
    let installed = store
        .install(
            &[InstallSource {
                directory: source,
                manifest,
                provenance: Provenance::Local,
            }],
            &[],
            &protocols,
            "portable",
        )
        .unwrap();
    let service = Arc::new(Imports {
        profile: "stage-callback".into(),
        selection: Importer {
            digest: installed.packages["importer"].digest.clone(),
            package: "importer".into(),
            export: "import".into(),
            program: std::env::var_os("RSI_EXTENSION_NODE")
                .expect("select Node explicitly")
                .into(),
        },
        workspace: rsi_workspace_protocol::canonical_native_path(
            root.path().canonicalize().unwrap(),
        ),
        temporary_root: root.path().into(),
        store: store.clone(),
        lane: tokio::sync::Mutex::default(),
        leases: tokio::sync::Mutex::default(),
    });
    (root, service)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires an explicitly selected Node via RSI_EXTENSION_NODE"]
async fn stage_callback_pins_releases_bounds_and_confirms_cleanup() {
    let (_root, service) = stage_service();
    let store = &service.store;
    assert!(service.drain_on_cancel("application.stage"));
    assert!(!service.drain_on_cancel("application.payloads"));
    let input = json!({"specifications":["fixture"]});
    let first = service
        .call("application.stage", input.clone(), CancellationToken::new())
        .await
        .unwrap();
    let second = service
        .call("application.stage", input.clone(), CancellationToken::new())
        .await
        .unwrap();
    assert_ne!(first["stage"], second["stage"]);
    assert_eq!(first["packages"], second["packages"]);
    let digest = first["packages"]["imported"].as_str().unwrap();
    assert!(store.profile_roots("stage-callback").unwrap().is_empty());
    assert!(
        !store
            .resolution()
            .unwrap()
            .packages
            .contains_key("imported")
    );
    for _ in 0..2 {
        service
            .call(
                "application.stage.release",
                json!({"stage":first["stage"]}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
    }
    assert!(
        store.collect().unwrap().is_empty(),
        "second handle must retain the same digest"
    );
    let held = store.acquire_for_process("imported", digest).unwrap();
    service
        .call(
            "application.stage.release",
            json!({"stage":second["stage"]}),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    // Exercise the post-work seam with admission complete and the unrelated lock held.
    let prepared = rsi_packages::StagedPackages {
        packages: BTreeMap::from([("imported".into(), digest.into())]),
        leases: BTreeMap::from([(digest.into(), held.clone())]),
    };
    let leases = service.leases.lock().await;
    let stop = CancellationToken::new();
    let retaining = service.retain_stage(prepared, &stop);
    tokio::pin!(retaining);
    assert!(futures_util::poll!(&mut retaining).is_pending());
    stop.cancel();
    let reply = tokio::time::timeout(Duration::from_secs(2), retaining)
        .await
        .expect("cancellation must not wait for unrelated payload work")
        .unwrap();
    assert_eq!(reply, json!({"cancelled":true}));
    assert!(leases.stages.is_empty());
    drop(leases);
    assert_eq!(Arc::strong_count(&held), 1);

    // Exhaust stage handles, then exercise the real post-import admission refusal.
    {
        let mut leases = service.leases.lock().await;
        for i in 0..32 {
            leases.stages.insert(
                format!("fixture-{i}"),
                rsi_packages::StagedPackages {
                    packages: BTreeMap::from([("imported".into(), digest.into())]),
                    leases: BTreeMap::from([(digest.into(), held.clone())]),
                },
            );
        }
    }
    assert!(matches!(
        service
            .call("application.stage", input, CancellationToken::new())
            .await,
        Err(rsi_extension::Error::Capacity)
    ));
    service.leases.lock().await.stages.clear();
    drop(held);
    assert_eq!(store.collect().unwrap(), [digest]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires an explicitly selected Node via RSI_EXTENSION_NODE"]
async fn stage_callback_cancellation_confirms_cleanup() {
    let (root, service) = stage_service();
    let store = &service.store;
    // Both queued and running importer cancellation use the owner's reply contract.
    let lane = service.lane.lock().await;
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert_eq!(
        service
            .call(
                "application.stage",
                json!({"specifications":["fixture"]}),
                cancelled
            )
            .await
            .unwrap(),
        json!({"cancelled":true})
    );
    drop(lane);
    let marker = root.path().join("entered");
    let stop = CancellationToken::new();
    let task = tokio::spawn({
        let service = service.clone();
        let stop = stop.clone();
        let marker = marker.clone();
        async move {
            service
                .call(
                    "application.stage",
                    json!({"specifications":["wait",marker]}),
                    stop,
                )
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(20), async {
        while !marker.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let marker: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&marker).unwrap()).unwrap();
    stop.cancel();
    assert_eq!(task.await.unwrap().unwrap(), json!({"cancelled":true}));
    assert!(
        !Path::new(marker["staging"].as_str().unwrap()).exists(),
        "acknowledgment follows process shutdown and scratch cleanup"
    );
    assert!(service.leases.lock().await.stages.is_empty());
    assert!(store.profile_roots("stage-callback").unwrap().is_empty());
}
