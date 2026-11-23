use super::*;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

const KEY: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one saturation matrix retains the same sockets through control progress, excess rejection and retirement"
)]
async fn stalled_applications_release_unclassified_admission() {
    for tls in [false, true] {
        for published in [false, true] {
            let retired = CancellationToken::new();
            let app = Arc::new(
                rsi_api_http::ApplicationUpstream::preparing(
                    SecretValue::new(KEY).unwrap(),
                    vec!["/document".into()],
                    retired.clone(),
                )
                .unwrap(),
            );
            let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
            if published {
                app.publish(upstream.local_addr().unwrap(), vec!["/document".into()])
                    .unwrap();
            }
            let h = Harness::start_with_surfaces(tls, None, None, Some(app.clone())).await;
            let client = reqwest::Client::builder()
                .no_proxy()
                .danger_accept_invalid_certs(true)
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap();
            // Establish the HTTP/2 pool before concurrent streams; TLS handshake
            // capacity is deliberately independent of application admission.
            let warm = client
                .post(format!("{}{}", h.origin, h.path("connection/caller")))
                .bearer_auth(TOKEN)
                .header("content-type", "application/json")
                .header("x-rsi-wire-version", "2")
                .header("x-rsi-host-epoch", h.epoch.as_str())
                .body("{}")
                .send()
                .await
                .unwrap();
            if tls {
                assert_eq!(warm.version(), reqwest::Version::HTTP_2);
            }
            warm.bytes().await.unwrap();
            let request = || {
                client
                    .get(format!("{}/document/", h.origin))
                    .header("sec-fetch-site", "same-origin")
                    .header("cookie", browser_cookie(&h.origin, TOKEN))
            };
            let mut requests = tokio::task::JoinSet::new();
            for _ in 0..32 {
                let request = request();
                requests.spawn(async move { request.send().await });
            }
            let device = AuthenticatedDevice {
                id: DeviceId::from_bytes([1; 16]),
                revoked: CancellationToken::new(),
            };
            let saturated = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    match app.open_callback(&device) {
                        Ok(probe) => drop(probe),
                        Err(ApiError::Capacity) => break,
                        Err(error) => panic!("unexpected callback admission: {error}"),
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await;
            if saturated.is_err() {
                let mut replies = Vec::new();
                while let Some(result) = requests.try_join_next() {
                    replies.push(format!(
                        "{:?}",
                        result.map(|value| value.map(|reply| reply.status()))
                    ));
                }
                panic!(
                    "application lane did not fill (tls={tls}, published={published}): {replies:?}"
                );
            }
            let mut sockets = Vec::new();
            if published {
                for _ in 0..32 {
                    let (socket, _) =
                        tokio::time::timeout(Duration::from_secs(5), upstream.accept())
                            .await
                            .unwrap()
                            .unwrap();
                    sockets.push(socket);
                }
            }
            let independent = reqwest::Client::builder()
                .no_proxy()
                .danger_accept_invalid_certs(true)
                .timeout(Duration::from_secs(3))
                .build()
                .unwrap();
            let response = independent
                .post(format!("{}{}", h.origin, h.path("connection/caller")))
                .bearer_auth(TOKEN)
                .header("content-type", "application/json")
                .header("x-rsi-wire-version", "2")
                .header("x-rsi-host-epoch", h.epoch.as_str())
                .body("{}")
                .send()
                .await
                .unwrap();
            assert!(
                response.status().is_success(),
                "control failed with full application lane"
            );
            response.bytes().await.unwrap();
            assert_eq!(
                independent
                    .get(format!("{}/document/", h.origin))
                    .header("sec-fetch-site", "same-origin")
                    .header("cookie", browser_cookie(&h.origin, TOKEN))
                    .send()
                    .await
                    .unwrap()
                    .status(),
                429
            );
            retired.cancel();
            while let Some(result) = requests.join_next().await {
                assert_eq!(result.unwrap().unwrap().status(), 409);
            }
            drop(sockets);
            h.close().await;
        }
    }
}

#[tokio::test]
async fn browser_domains_share_an_owner_while_credential_negotiation_and_bearers_remain_distinct() {
    let h = Harness::start().await;
    let principal = rsi_api_protocol::json_handler(|ctx, (): ()| async move {
        let (
            rsi_api_protocol::CallOrigin::Device(actual),
            rsi_api_protocol::CallOrigin::Device(actor),
        ) = (ctx.credential_origin(), ctx.origin)
        else {
            unreachable!()
        };
        Ok(Ok::<_, ()>((actor.id, actual.id)))
    });
    let registration = h
        .registry
        .register(
            spec("principal", OperationClass::Data, OperationEffect::Read),
            principal.clone(),
        )
        .unwrap();
    let mut credential = spec("credential", OperationClass::Data, OperationEffect::Read);
    credential.access = rsi_api_protocol::OperationAccess::Credential;
    let credential_registration = h.registry.register(credential, principal).unwrap();
    let mut owners = vec![];
    for (credential, id) in [
        (TOKEN, DeviceId::from_bytes([1; 16])),
        (SECOND_TOKEN, DeviceId::from_bytes([2; 16])),
    ] {
        let cookie = |op: &str| {
            Harness::client()
                .post(format!("{}{}", h.origin, h.path(op)))
                .header("origin", &h.origin)
                .header("x-rsi-csrf", "1")
                .header("x-rsi-wire-version", "2")
                .header("x-rsi-host-epoch", h.epoch.as_str())
                .header("x-rsi-expected-device", id.as_str())
                .header("cookie", browser_cookie(&h.origin, credential))
                .header("content-type", "application/json")
        };
        let identity: serde_json::Value = serde_json::from_slice(
            &cookie("connection/caller")
                .body("{}")
                .send()
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(identity["device_id"], id.as_str());
        let (actor, actual_id): (DeviceId, DeviceId) = serde_json::from_slice(
            &cookie("test/principal")
                .body("null")
                .send()
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(actual_id, id);
        owners.push(actor);
        let actual = cookie("test/credential").body("null").send().await.unwrap();
        assert_eq!(
            serde_json::from_slice::<(DeviceId, DeviceId)>(&actual.bytes().await.unwrap()).unwrap(),
            (id.clone(), id.clone())
        );
        let reply = Harness::client()
            .post(format!("{}{}", h.origin, h.path("test/principal")))
            .bearer_auth(credential)
            .header("x-rsi-host-epoch", h.epoch.as_str())
            .header("x-rsi-wire-version", "2")
            .header("content-type", "application/json")
            .body("null")
            .send()
            .await
            .unwrap();
        let native: (DeviceId, DeviceId) =
            serde_json::from_slice(&reply.bytes().await.unwrap()).unwrap();
        assert_eq!(native, (id.clone(), id));
    }
    assert_eq!(owners[0], owners[1]);
    drop((registration, credential_registration));
    h.close().await;
}

#[tokio::test]
async fn preparing_routes_wait_for_publication_and_retire_without_forwarding() {
    let retired = CancellationToken::new();
    let app = Arc::new(
        rsi_api_http::ApplicationUpstream::preparing(
            SecretValue::new(KEY).unwrap(),
            vec!["/document".into()],
            retired.clone(),
        )
        .unwrap(),
    );
    let h = Harness::start_with_surfaces(false, None, None, Some(app.clone())).await;
    let client = Harness::client();
    let url = format!("{}/document/", h.origin);
    assert_eq!(client.get(&url).send().await.unwrap().status(), 401);
    let request = client
        .get(&url)
        .header("sec-fetch-site", "none")
        .header("sec-fetch-mode", "navigate")
        .header("sec-fetch-dest", "document")
        .header("cookie", browser_cookie(&h.origin, TOKEN))
        .send();
    tokio::pin!(request);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut request)
            .await
            .is_err()
    );
    retired.cancel();
    assert_eq!(request.await.unwrap().status(), 409);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    assert!(
        app.publish(listener.local_addr().unwrap(), vec!["/other".into()])
            .is_err()
    );
}
async fn fixture() -> (Harness, TcpListener, Arc<rsi_api_http::ApplicationUpstream>) {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let routes = rsi_api_http::ApplicationUpstream::new(
        upstream.local_addr().unwrap(),
        SecretValue::new(KEY).unwrap(),
        vec!["/sidebar".into(), "/api".into()],
        CancellationToken::new(),
    )
    .unwrap();
    let routes = Arc::new(routes);
    (
        Harness::start_with_surfaces(false, None, None, Some(routes.clone())).await,
        upstream,
        routes,
    )
}

#[tokio::test]
async fn separately_authenticated_browsers_forward_as_one_owner() {
    let (h, target, app) = fixture().await;
    let client = Harness::client();
    let url = format!("{}/sidebar/file", h.origin);
    let request = || client.get(&url).header("sec-fetch-site", "same-origin");
    assert_eq!(request().send().await.unwrap().status(), 401);
    assert_eq!(
        request().bearer_auth(TOKEN).send().await.unwrap().status(),
        401
    );
    let worker = tokio::spawn(async move {
        let mut owners = vec![];
        for expected in [DeviceId::from_bytes([1; 16]), DeviceId::from_bytes([2; 16])] {
            let (mut socket, _) = target.accept().await.unwrap();
            let mut headers = vec![];
            while !headers.ends_with(b"\r\n\r\n") {
                headers.push(socket.read_u8().await.unwrap());
            }
            let text = String::from_utf8(headers).unwrap().to_ascii_lowercase();
            for header in [
                "cookie:",
                "x-rsi-expected-device:",
                "x-rsi-upstream-device:",
                "x-rsi-upstream-owner:",
            ] {
                assert!(!text.contains(header));
            }
            let key = text
                .lines()
                .find_map(|line| line.strip_prefix("x-rsi-upstream-request: "))
                .unwrap();
            let rsi_api_protocol::CallOrigin::Device(actor) = app.caller(key).unwrap() else {
                panic!("expected browser owner")
            };
            let rsi_api_protocol::CallOrigin::Device(credential) = app.credential(key).unwrap()
            else {
                panic!("expected original credential")
            };
            assert_eq!(credential.id, expected);
            owners.push(actor.id);
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .await
                .unwrap();
        }
        owners
    });
    let first = DeviceId::from_bytes([1; 16]);
    let second = DeviceId::from_bytes([2; 16]);
    assert_eq!(
        request()
            .header("cookie", browser_cookie(&h.origin, SECOND_TOKEN))
            .header("x-rsi-expected-device", first.as_str())
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    for (credential, id) in [(TOKEN, first), (SECOND_TOKEN, second)] {
        assert_eq!(
            request()
                .header("cookie", browser_cookie(&h.origin, credential))
                .header("x-rsi-expected-device", id.as_str())
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap(),
            "ok"
        );
    }
    let owners = worker.await.unwrap();
    assert_eq!(owners[0], owners[1]);
    h.close().await;
}

#[tokio::test]
async fn authorized_chunk_streams_without_device_credential_or_frame_limit() {
    let (h, listener, routes) = fixture().await;
    let url = format!("{}/sidebar/bundle/editor.js", h.origin);
    let caller = Harness::client();
    let native = caller
        .get(format!(
            "{}{}",
            h.origin,
            rsi_api_protocol::BROWSER_BOOTSTRAP_PATH
        ))
        .header("sec-fetch-site", "same-origin")
        .header("cookie", browser_cookie(&h.origin, TOKEN))
        .send()
        .await
        .unwrap();
    assert_ne!(native.status().as_u16(), 503);
    for site in ["same-origin", "cross-site"] {
        let mut request = caller.get(&url).header("sec-fetch-site", site);
        if site == "cross-site" {
            request = request.header("cookie", browser_cookie(&h.origin, TOKEN));
        }
        assert_eq!(request.send().await.unwrap().status(), 401);
    }
    let retained = routes.clone();
    let public_origin = h.origin.clone();
    let server = tokio::spawn(async move {
        let (mut peer, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let mut byte = [0; 1];
        while !bytes.ends_with(b"\r\n\r\n") {
            peer.read_exact(&mut byte).await.unwrap();
            bytes.push(byte[0]);
            assert!(bytes.len() < 8192);
        }
        let head = String::from_utf8(bytes).unwrap().to_ascii_lowercase();
        let id = head
            .lines()
            .find_map(|line| line.strip_prefix("x-rsi-upstream-request: "))
            .unwrap()
            .to_owned();
        assert!(matches!(
            retained.caller(&id),
            Ok(rsi_api_protocol::CallOrigin::Device(_))
        ));
        assert!(head.starts_with("get /sidebar/bundle/editor.js "));
        assert!(head.contains(&format!("x-rsi-upstream-key: {KEY}\r\n")));
        assert!(head.contains(&format!("x-rsi-upstream-origin: {public_origin}\r\n")));
        let public_peer: std::net::SocketAddr = head
            .lines()
            .find_map(|line| line.strip_prefix("x-rsi-upstream-peer: "))
            .unwrap()
            .parse()
            .unwrap();
        assert!(public_peer.ip().is_loopback() && public_peer.port() != 0);
        assert!(!head.contains("cookie:") && !head.contains(TOKEN) && !head.contains("spoof"));
        let size = 2 * 1024 * 1024;
        peer.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {size}\r\nETag: \"original\"\r\nContent-Type: text/javascript; charset=utf-8\r\nCache-Control: immutable, public, max-age=100\r\nSet-Cookie: spoof=1\r\n\r\n").as_bytes()).await.unwrap();
        peer.write_all(&vec![b'x'; size]).await.unwrap();
        id
    });
    let response = caller
        .get(&url)
        .header("cookie", browser_cookie(&h.origin, TOKEN))
        .header("sec-fetch-site", "same-origin")
        .header("x-rsi-upstream-key", "spoof")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["etag"], "\"original\"");
    assert_eq!(
        response.headers()["cache-control"],
        "immutable, max-age=100, private"
    );
    assert!(response.headers().get("set-cookie").is_none());
    assert_eq!(response.bytes().await.unwrap().len(), 2 * 1024 * 1024);
    let id = server.await.unwrap();
    assert!(routes.caller(&id).is_err());
    h.stop.cancel();
    h.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn unversioned_code_revalidates_privately_and_json_remains_uncached() {
    let (h, listener, _) = fixture().await;
    let server = tokio::spawn(async move {
        for (status, mime) in [
            ("200 OK", "Content-Type: text/javascript\r\n"),
            ("304 Not Modified", ""),
            ("200 OK", "Content-Type: application/json\r\n"),
        ] {
            let (mut peer, _) = listener.accept().await.unwrap();
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                head.push(peer.read_u8().await.unwrap());
                assert!(head.len() < 8192);
            }
            if status.starts_with("304") {
                assert!(
                    String::from_utf8(head)
                        .unwrap()
                        .to_ascii_lowercase()
                        .contains("if-none-match: \"v1\"")
                );
            }
            peer.write_all(format!("HTTP/1.1 {status}\r\n{mime}ETag: \"v1\"\r\nCache-Control: no-cache\r\nContent-Length: 0\r\n\r\n").as_bytes()).await.unwrap();
        }
    });
    let client = Harness::client();
    for (n, expected) in ["private, no-cache", "private, no-cache", "no-store"]
        .iter()
        .enumerate()
    {
        let mut request = client
            .get(format!("{}/sidebar/resource", h.origin))
            .header("cookie", browser_cookie(&h.origin, TOKEN))
            .header("sec-fetch-site", "same-origin");
        if n == 1 {
            request = request.header("if-none-match", "\"v1\"");
        }
        let response = request.send().await.unwrap();
        assert_eq!(response.status().as_u16(), if n == 1 { 304 } else { 200 });
        assert_eq!(response.headers()["cache-control"], *expected);
    }
    server.await.unwrap();
    h.stop.cancel();
    h.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn application_upgrade_is_duplex_and_revocation_closes_it() {
    let (h, listener, _) = fixture().await;
    let server = tokio::spawn(async move {
        let (peer, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(peer).await.unwrap();
        let msg = ws.next().await.unwrap().unwrap();
        ws.send(msg).await.unwrap();
        let _ = ws.next().await;
    });
    let mut request = format!("ws://{}/sidebar/ws/agent-opens?session=test", h.address)
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("origin", h.origin.parse().unwrap());
    request
        .headers_mut()
        .insert("cookie", browser_cookie(&h.origin, TOKEN).parse().unwrap());
    for (origin, site) in [
        (None, None),
        (Some("https://foreign.invalid"), None),
        (Some(h.origin.as_str()), Some("cross-site")),
    ] {
        let mut denied = request.clone();
        denied.headers_mut().remove("origin");
        if let Some(origin) = origin {
            denied
                .headers_mut()
                .insert("origin", origin.parse().unwrap());
        }
        if let Some(site) = site {
            denied
                .headers_mut()
                .insert("sec-fetch-site", site.parse().unwrap());
        }
        let error =
            tokio_tungstenite::client_async(denied, TcpStream::connect(h.address).await.unwrap())
                .await
                .unwrap_err();
        assert!(
            matches!(error, tokio_tungstenite::tungstenite::Error::Http(r) if r.status() == 401)
        );
    }
    let (mut ws, _) =
        tokio_tungstenite::client_async(request, TcpStream::connect(h.address).await.unwrap())
            .await
            .unwrap();
    ws.send(Message::text("one")).await.unwrap();
    assert_eq!(ws.next().await.unwrap().unwrap(), Message::text("one"));
    h.authentication.first.cancel();
    let closed = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .unwrap();
    assert!(!matches!(closed, Some(Ok(Message::Text(_)))));
    server.await.unwrap();
    h.stop.cancel();
    h.task.await.unwrap().unwrap();
}
