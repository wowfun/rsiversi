//! Bounded repository transport. Authentication and publication stay in rsi-packages.
use super::error;
use rsi_packages::{
    InstallSource, Manifest, PackageStore, Provenance, Resolution, SignedRepository,
};
use sha2::{Digest as _, Sha256};
use std::{
    collections::BTreeMap,
    io::Write as _,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

fn root(value: &str) -> rsi::Result<reqwest::Url> {
    let mut url = reqwest::Url::parse(value).map_err(error)?;
    let loopback = url
        .host_str()
        .is_some_and(|host| matches!(host, "127.0.0.1" | "[::1]" | "::1" | "localhost"));
    if !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(error(
            "repository requires HTTPS (or loopback HTTP), without credentials, query or fragment",
        ));
    }
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url)
}
fn child(base: &reqwest::Url, path: &str) -> rsi::Result<reqwest::Url> {
    let mut url = base.clone();
    url.path_segments_mut()
        .map_err(|()| error("repository URL cannot contain path segments"))?
        .pop_if_empty()
        .extend(path.split('/'));
    Ok(url)
}
async fn receive(
    client: &reqwest::Client,
    url: reqwest::Url,
    limit: u64,
    mut sink: impl std::io::Write,
) -> rsi::Result<String> {
    let mut response = client.get(url).send().await.map_err(error)?;
    if response.status() != reqwest::StatusCode::OK {
        return Err(error(format!(
            "repository returned HTTP {}",
            response.status()
        )));
    }
    if response
        .content_length()
        .is_some_and(|length| length > limit)
    {
        return Err(error("download exceeds declared length"));
    }
    let mut total = 0_u64;
    let mut digest = Sha256::new();
    while let Some(chunk) = response.chunk().await.map_err(error)? {
        total = total
            .checked_add(chunk.len() as u64)
            .ok_or_else(|| error("download overflow"))?;
        if total > limit {
            return Err(error("download exceeds declared length"));
        }
        digest.update(&chunk);
        sink.write_all(&chunk).map_err(error)?;
    }
    Ok(hex::encode(digest.finalize()))
}
pub(super) fn configured(
    value: Option<rsi::LaunchRepository>,
) -> rsi::Result<rsi::LaunchRepository> {
    value.or_else(|| Some(rsi::LaunchRepository {
        name: option_env!("RSI_PACKAGE_REPOSITORY_NAME")?.into(),
        url: option_env!("RSI_PACKAGE_REPOSITORY_URL")?.into(),
        public_key: option_env!("RSI_PACKAGE_REPOSITORY_KEY")?.into(),
    })).ok_or_else(|| error("this build has no release repository identity; configure [repository] in rsi.toml or use plugin install --local DIR"))
}
pub(super) fn configured_core(
    value: Option<rsi::LaunchRepository>,
) -> rsi::Result<rsi::LaunchRepository> {
    core_repository(configured(None).ok(), value)
}
fn core_repository(
    pinned: Option<rsi::LaunchRepository>,
    configured: Option<rsi::LaunchRepository>,
) -> rsi::Result<rsi::LaunchRepository> {
    let pinned = pinned.ok_or_else(|| {
        error("this build has no pinned core repository; use rsi core upgrade --local DIRECTORY")
    })?;
    if let Some(configured) = configured {
        if configured.name != pinned.name || configured.public_key != pinned.public_key {
            return Err(error(
                "core upgrade repository must retain the compiled name and public key",
            ));
        }
        Ok(configured)
    } else {
        Ok(pinned)
    }
}
#[allow(clippy::too_many_lines)] // Resolve and verify the bounded signed closure before its single publication.
pub(super) async fn install(
    store: &PackageStore,
    repository: rsi::LaunchRepository,
    names: Vec<String>,
    interfaces: &[rsi_packages::Interface],
    target: &str,
) -> rsi::Result<Resolution> {
    let base = root(&repository.url)?;
    let key: [u8; 32] = hex::decode(&repository.public_key)
        .map_err(error)?
        .try_into()
        .map_err(|_| error("repository key must have 32 bytes"))?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(90))
        .timeout(Duration::from_mins(30))
        .build()
        .map_err(error)?;
    let mut encoded = Vec::new();
    receive(
        &client,
        child(&base, "repository.json")?,
        (rsi_packages::MAX_DOCUMENT * 2 + 256) as u64,
        &mut encoded,
    )
    .await?;
    let signed: SignedRepository = serde_json::from_slice(&encoded).map_err(error)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(error)?
        .as_secs();
    let verified = store
        .accept_repository(&signed, &key, &repository.name, now)
        .map_err(error)?;
    let index = verified.repository();
    let receipt = verified.receipt();
    let mut pending: Vec<_> = names.into_iter().map(|name| (name, None)).collect();
    let mut seen = BTreeMap::new();
    let mut sources = Vec::new();
    let staging = tempfile::tempdir().map_err(error)?;
    let mut total = 0_u64;
    while let Some((name, required)) = pending.pop() {
        let selection = index
            .packages
            .get(&name)
            .and_then(|targets| targets.get(target).or_else(|| targets.get("portable")))
            .ok_or_else(|| error(format!("repository has no {name} package for {target}")))?;
        if required
            .as_ref()
            .is_some_and(|digest| digest != &selection.digest)
        {
            return Err(error(format!(
                "repository dependency {name} does not match its exact digest"
            )));
        }
        if seen
            .insert(name.clone(), selection.digest.clone())
            .is_some()
        {
            continue;
        }
        if seen.len() > 128 {
            return Err(error("dependency closure exceeds 128 packages"));
        }
        let mut bytes = Vec::new();
        let digest = receive(
            &client,
            child(&base, &selection.manifest)?,
            rsi_packages::MAX_DOCUMENT as u64,
            &mut bytes,
        )
        .await?;
        if digest != selection.digest {
            return Err(error("manifest digest differs from signed metadata"));
        }
        let manifest = Manifest::parse(&bytes).map_err(error)?;
        if manifest.name != name || (manifest.target != "portable" && manifest.target != target) {
            return Err(error(
                "manifest name or target differs from repository selection",
            ));
        }
        manifest.admit_interfaces(interfaces).map_err(error)?;
        total = total
            .checked_add(manifest.files.iter().map(|file| file.bytes).sum::<u64>())
            .ok_or_else(|| error("download total overflow"))?;
        if total > rsi_packages::MAX_STORE_BYTES {
            return Err(error(format!(
                "download transaction exceeds {} bytes",
                rsi_packages::MAX_STORE_BYTES
            )));
        }
        let directory = staging.path().join(&name);
        std::fs::create_dir(&directory).map_err(error)?;
        let prefix = selection
            .manifest
            .rsplit_once('/')
            .map_or("", |(parent, _)| parent);
        for file in &manifest.files {
            let destination = directory.join(&file.path);
            std::fs::create_dir_all(destination.parent().expect("admitted relative path"))
                .map_err(error)?;
            let mut output = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(destination)
                .map_err(error)?;
            let relative = if prefix.is_empty() {
                file.path.clone()
            } else {
                format!("{prefix}/{}", file.path)
            };
            let digest =
                receive(&client, child(&base, &relative)?, file.bytes, &mut output).await?;
            if output.metadata().map_err(error)?.len() != file.bytes || digest != file.digest {
                return Err(error("downloaded package file digest or size differs"));
            }
            output.flush().map_err(error)?;
        }
        pending.extend(
            manifest
                .dependencies
                .iter()
                .map(|dependency| (dependency.name.clone(), Some(dependency.digest.clone()))),
        );
        sources.push(InstallSource {
            directory,
            manifest: bytes,
            provenance: Provenance::Signed {
                repository: receipt.repository.clone(),
                sequence: receipt.sequence,
                payload: receipt.digest.clone(),
            },
        });
    }
    store
        .install(&sources, &[verified], interfaces, target)
        .map_err(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::KeyPair as _;
    use std::sync::{Arc, Mutex};
    #[tokio::test]
    async fn desktop_download_adds_missing_web_and_preserves_an_installed_web() {
        let key = ring::signature::Ed25519KeyPair::from_seed_unchecked(&[8; 32]).unwrap();
        let content = b"format = 1\n".to_vec();
        let mut packages = serde_json::Map::new();
        let mut routes = BTreeMap::new();
        for name in ["desktop", "web"] {
            let manifest = serde_json::to_vec(&serde_json::json!({
                "format":2,"name":name,"version":"1","target":"portable","dependencies":[],"requires":[],
                "files":[{"path":"profile.toml","bytes":content.len(),"digest":hex::encode(Sha256::digest(&content)),"executable":false}],
                "exports":{"main":{"kind":"application_profile","path":"profile.toml"}}
            })).unwrap();
            packages.insert(name.into(), serde_json::json!({"portable":{"manifest":format!("{name}/package.json"),"digest":hex::encode(Sha256::digest(&manifest))}}));
            routes.insert(format!("/{name}/package.json"), manifest);
            routes.insert(format!("/{name}/profile.toml"), content.clone());
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let payload = serde_json::to_vec(&serde_json::json!({"format":1,"name":"fixture","sequence":1,"issued":now,"expires":now+3600,"packages":packages})).unwrap();
        routes.insert(
            "/repository.json".into(),
            serde_json::to_vec(&SignedRepository {
                payload: hex::encode(&payload),
                signature: hex::encode(key.sign(&payload).as_ref()),
            })
            .unwrap(),
        );
        let routes = Arc::new(Mutex::new(routes));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let responses = routes.clone();
        let server = tokio::spawn(async move {
            let app = axum::Router::new().fallback(move |uri: axum::http::Uri| {
                let responses = responses.clone();
                async move {
                    responses
                        .lock()
                        .unwrap()
                        .get(uri.path())
                        .cloned()
                        .map_or((axum::http::StatusCode::NOT_FOUND, Vec::new()), |bytes| {
                            (axum::http::StatusCode::OK, bytes)
                        })
                }
            });
            axum::serve(listener, app).await.unwrap();
        });
        let repository = rsi::LaunchRepository {
            name: "fixture".into(),
            url: format!("http://{address}/"),
            public_key: hex::encode(key.public_key()),
        };
        let home = tempfile::tempdir().unwrap();
        let store = PackageStore::open(&home.path().join("packages")).unwrap();
        let first = install(
            &store,
            repository.clone(),
            vec!["desktop".into()],
            &[],
            "windows-x86_64",
        )
        .await
        .unwrap();
        assert!(first.packages.contains_key("desktop"));
        assert!(first.packages.contains_key("web"));
        routes.lock().unwrap().remove("/web/package.json");
        let next = install(
            &store,
            repository.clone(),
            vec!["desktop".into()],
            &[],
            "windows-x86_64",
        )
        .await
        .unwrap();
        assert_eq!(next.packages["web"], first.packages["web"]);
        assert!(
            install(
                &store,
                repository,
                vec!["desktop".into(), "web".into()],
                &[],
                "windows-x86_64"
            )
            .await
            .is_err()
        );
        assert_eq!(store.resolution().unwrap().packages, next.packages);
        server.abort();
        let _ = server.await;
    }
    #[test]
    fn core_repository_accepts_only_compiled_identity_or_its_mirror() {
        let repository = |name: &str, key: &str, url: &str| rsi::LaunchRepository {
            name: name.into(),
            public_key: key.into(),
            url: url.into(),
        };
        assert!(
            core_repository(None, Some(repository("custom", "b", "https://example.com"))).is_err()
        );
        assert!(
            core_repository(
                Some(repository("release", "a", "https://example.com")),
                Some(repository("custom", "b", "https://example.com"))
            )
            .is_err()
        );
        assert_eq!(
            core_repository(
                Some(repository("release", "a", "https://example.com")),
                Some(repository("release", "a", "https://mirror.example.com"))
            )
            .unwrap()
            .url,
            "https://mirror.example.com"
        );
    }
    #[test]
    fn repository_transport_policy_and_segment_encoding() {
        for input in [
            "http://example.com/",
            "https://user@example.com/",
            "https://example.com/?key=x",
            "https://example.com/#fragment",
            "file:///tmp/repo",
        ] {
            assert!(root(input).is_err(), "{input}");
        }
        let base = root("http://127.0.0.1:4000/repo").unwrap();
        assert_eq!(
            child(&base, "web/a#b%20.json").unwrap().as_str(),
            "http://127.0.0.1:4000/repo/web/a%23b%2520.json"
        );
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)] // One retained trust receipt is exercised through tamper, rollback and expiry.
    async fn signed_download_publishes_exact_bytes_and_refuses_tamper_and_rollback() {
        let key = ring::signature::Ed25519KeyPair::from_seed_unchecked(&[7; 32]).unwrap();
        let content = b"format = 1\n".to_vec();
        let manifest = serde_json::to_vec(&serde_json::json!({
            "format": 1, "name": "fixture", "version": "1", "target": "portable",
            "dependencies": [], "requires": [],
            "files": [{"path":"profile.toml","bytes":content.len(),"digest":hex::encode(Sha256::digest(&content)),"executable":false}],
            "exports":{"main":{"kind":"application_profile","path":"profile.toml"}}
        })).unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let signed = |sequence| {
            let payload = serde_json::to_vec(&serde_json::json!({
                "format":1,"name":"fixture","sequence":sequence,"issued":now,"expires":now+3600,
                "packages":{"fixture":{"portable":{
                    "manifest":"fixture/package.json","digest":hex::encode(Sha256::digest(&manifest))
                }}}
            })).unwrap();
            serde_json::to_vec(&SignedRepository {
                payload: hex::encode(&payload),
                signature: hex::encode(key.sign(&payload).as_ref()),
            })
            .unwrap()
        };
        let responses = Arc::new(Mutex::new(BTreeMap::from([
            ("/repository.json".to_owned(), signed(2)),
            ("/fixture/package.json".to_owned(), manifest.clone()),
            ("/fixture/profile.toml".to_owned(), content.clone()),
        ])));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server_responses = responses.clone();
        let server = tokio::spawn(async move {
            let app = axum::Router::new().fallback(move |uri: axum::http::Uri| {
                let responses = server_responses.clone();
                async move {
                    responses
                        .lock()
                        .unwrap()
                        .get(uri.path())
                        .cloned()
                        .map_or((axum::http::StatusCode::NOT_FOUND, Vec::new()), |bytes| {
                            (axum::http::StatusCode::OK, bytes)
                        })
                }
            });
            axum::serve(listener, app).await.unwrap();
        });
        let configuration = rsi::LaunchRepository {
            name: "fixture".into(),
            url: format!("http://{address}/"),
            public_key: hex::encode(key.public_key()),
        };
        let home = tempfile::tempdir().unwrap();
        let store = PackageStore::open(&home.path().join("packages")).unwrap();
        let first = install(
            &store,
            configuration.clone(),
            vec!["fixture".into()],
            &[],
            "windows-x86_64",
        )
        .await
        .unwrap();
        let lease = store.acquire("fixture").unwrap();
        assert_eq!(
            std::fs::read(
                lease
                    .export("main", rsi_packages::ExportKind::ApplicationProfile)
                    .unwrap()
            )
            .unwrap(),
            content
        );
        assert!(matches!(
            &first.packages["fixture"].provenance,
            Provenance::Signed { sequence: 2, .. }
        ));
        responses
            .lock()
            .unwrap()
            .insert("/repository.json".into(), signed(3));
        responses
            .lock()
            .unwrap()
            .insert("/fixture/profile.toml".into(), b"tamper".to_vec());
        assert!(
            install(
                &store,
                configuration.clone(),
                vec!["fixture".into()],
                &[],
                "windows-x86_64"
            )
            .await
            .is_err()
        );
        assert_eq!(store.resolution().unwrap().packages, first.packages);
        assert_eq!(store.resolution().unwrap().trust["fixture"].sequence, 3);
        responses
            .lock()
            .unwrap()
            .insert("/repository.json".into(), signed(2));
        let failure = install(
            &store,
            configuration.clone(),
            vec!["fixture".into()],
            &[],
            "windows-x86_64",
        )
        .await
        .unwrap_err();
        assert!(failure.to_string().contains("rollback"));
        let mut invalid_signature: SignedRepository = serde_json::from_slice(&signed(4)).unwrap();
        let changed = u8::from_str_radix(&invalid_signature.signature[..2], 16).unwrap() ^ 1;
        invalid_signature
            .signature
            .replace_range(0..2, &format!("{changed:02x}"));
        responses.lock().unwrap().insert(
            "/repository.json".into(),
            serde_json::to_vec(&invalid_signature).unwrap(),
        );
        assert!(
            install(
                &store,
                configuration,
                vec!["fixture".into()],
                &[],
                "windows-x86_64"
            )
            .await
            .is_err()
        );
        assert_eq!(store.resolution().unwrap().trust["fixture"].sequence, 3);
        server.abort();
        let _ = server.await;
    }
}
