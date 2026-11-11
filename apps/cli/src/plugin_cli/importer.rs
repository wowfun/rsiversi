use super::error;
use rsi_packages::{PackageStore, Resolution};
use std::{ffi::OsString, path::PathBuf, sync::Arc};
use tokio_util::sync::CancellationToken;
#[cfg(test)]
use {serde_json::json, std::time::Duration};

struct Invocation {
    package: String,
    export: String,
    program: PathBuf,
    specifications: Vec<String>,
}
fn parse(arguments: &[OsString]) -> rsi::Result<Invocation> {
    if !(4..=131).contains(&arguments.len()) || arguments[1] != "--program" {
        return Err(error(
            "expected PACKAGE/EXPORT --program ABSOLUTE_EXE SPEC...",
        ));
    }
    let (package, export) = arguments[0]
        .to_str()
        .and_then(|value| value.split_once('/'))
        .ok_or_else(|| error("importer must be PACKAGE/EXPORT"))?;
    rsi_packages::validate_identifier(package).map_err(error)?;
    rsi_packages::validate_identifier(export).map_err(error)?;
    let program = PathBuf::from(&arguments[2]);
    if !program.is_absolute() {
        return Err(error("importer program must be absolute"));
    }
    let specifications = arguments[3..]
        .iter()
        .map(|value| {
            value
                .to_str()
                .filter(|value| !value.is_empty() && !value.contains('\0'))
                .map(str::to_owned)
                .ok_or_else(|| error("invalid import specification"))
        })
        .collect::<rsi::Result<Vec<_>>>()?;
    if specifications.len() > 128
        || specifications.iter().map(String::len).sum::<usize>() > 64 * 1024
    {
        return Err(error("import specification capacity"));
    }
    Ok(Invocation {
        package: package.into(),
        export: export.into(),
        program,
        specifications,
    })
}

pub(super) async fn install(
    store: Arc<PackageStore>,
    arguments: &[OsString],
) -> rsi::Result<Resolution> {
    let stop = CancellationToken::new();
    let work = install_inner(store, arguments, stop.clone());
    tokio::pin!(work);
    tokio::select! {
        result = &mut work => result,
        signal = tokio::signal::ctrl_c() => {
            stop.cancel();
            let result = work.await;
            signal.map_err(error)?;
            result
        }
    }
}
async fn install_inner(
    store: Arc<PackageStore>,
    arguments: &[OsString],
    stop: CancellationToken,
) -> rsi::Result<Resolution> {
    let invocation = parse(arguments)?;
    rsi::import_packages(
        store,
        rsi::PackageImport {
            digest: None,
            package: invocation.package,
            export: invocation.export,
            program: invocation.program,
            options: None,
            workspace: std::env::current_dir().map_err(error)?,
            specifications: invocation.specifications,
        },
        rsi_serve::supported_protocols(),
        stop,
    )
    .await
    .map(|admission| admission.resolution)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    #[ignore = "requires an explicitly selected Node via RSI_EXTENSION_NODE"]
    async fn native_importer_stages_drains_and_publishes_without_service_authority() {
        use sha2::{Digest as _, Sha256};
        let hash = |bytes: &[u8]| hex::encode(Sha256::digest(bytes));
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        std::fs::create_dir(&source).unwrap();
        let script = include_bytes!("../../tests/fixtures/package-importer.mjs");
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
            "requires":[{"name":"rsi.extension.process","version":1},{"name":"rsi.packages.importer","version":1}],
            "files":[{"path":"payload.rsi","bytes":payload.len(),"digest":hash(&payload),"executable":false},
                {"path":"import.json","bytes":descriptor.len(),"digest":hash(&descriptor),"executable":false}],
            "exports":{"runtime":{"kind":"payload_tree","path":"payload.rsi","index_digest":index_digest},"import":{"kind":"package_importer","path":"import.json"}}})).unwrap();
        let store = Arc::new(PackageStore::open(&root.path().join("store")).unwrap());
        store
            .install(
                &[rsi_packages::InstallSource {
                    directory: source,
                    manifest,
                    provenance: rsi_packages::Provenance::Local,
                }],
                &[],
                &rsi_serve::supported_protocols(),
                "portable",
            )
            .unwrap();
        let mut args: Vec<OsString> = vec![
            "importer/import".into(),
            "--program".into(),
            std::env::var_os("RSI_EXTENSION_NODE").unwrap(),
            "fixture".into(),
        ];
        let accepted = install(store.clone(), &args).await.unwrap();
        let digest = accepted.packages["imported"].digest.clone();
        assert_eq!(
            store.acquire("imported").unwrap().manifest().files[0].digest,
            hash(b"verified native importer publication")
        );
        args[3] = "escape".into();
        assert!(install(store.clone(), &args).await.is_err());
        assert_eq!(
            store.resolution().unwrap().packages["imported"].digest,
            digest
        );
        let marker = root.path().join("entered");
        args[3] = "wait".into();
        args.push(marker.clone().into_os_string());
        let stop = CancellationToken::new();
        let (cancelled, ()) =
            tokio::join!(install_inner(store.clone(), &args, stop.clone()), async {
                tokio::time::timeout(Duration::from_secs(10), async {
                    while !marker.exists() {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .unwrap();
                stop.cancel();
            });
        assert!(cancelled.is_err());
        assert_eq!(
            store.resolution().unwrap().packages["imported"].digest,
            digest
        );
    }
}
