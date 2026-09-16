use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

use hickory_resolver::{
    config::{NameServerConfig, ResolverConfig},
    net::runtime::TokioRuntimeProvider,
    proto::{
        op::{Message, ResponseCode},
        rr::{
            RData, Record, RecordType,
            rdata::{A, AAAA},
        },
    },
};

#[tokio::test]
async fn real_dns_answers_are_checked_and_dns64_discovery_is_cached_without_fallback() {
    let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let mut nameserver = NameServerConfig::udp_and_tcp("127.0.0.1".parse().unwrap());
    nameserver.trust_negative_responses = false;
    for connection in &mut nameserver.connections {
        connection.port = socket.local_addr().unwrap().port();
    }
    let config = ResolverConfig::from_name_servers(vec![nameserver]);
    let mut builder = TokioResolver::builder_with_config(config, TokioRuntimeProvider::default());
    builder.options_mut().ip_strategy = LookupIpStrategy::Ipv4AndIpv6;
    builder.options_mut().attempts = 1;
    let resolver = Arc::new(builder.build().unwrap());
    let discovery = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let deny_discovery = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = discovery.clone();
    let deny = deny_discovery.clone();
    let server = tokio::spawn(async move {
        let mut bytes = [0; 4096];
        loop {
            let (length, peer) = socket.recv_from(&mut bytes).await.unwrap();
            let request = Message::from_vec(&bytes[..length]).unwrap();
            let query = &request.queries[0];
            let host = query.name().to_ascii();
            let is_discovery = host == "ipv4only.arpa.";
            let mut response = Message::response(request.id, request.op_code);
            response.metadata.recursion_desired = true;
            response.metadata.recursion_available = true;
            response.add_query(query.clone());
            if is_discovery {
                observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            if is_discovery && deny.load(std::sync::atomic::Ordering::SeqCst) {
                response.metadata.response_code = ResponseCode::Refused;
            } else {
                let address = match (host.as_str(), query.query_type()) {
                    ("ipv4only.arpa.", RecordType::A) => "192.0.0.170",
                    ("ipv4only.arpa.", _) => "2606:4700:1234::c000:aa",
                    ("mixed.example.", RecordType::A) => "127.0.0.1",
                    ("translated.example.", RecordType::AAAA) => "2606:4700:1234::7f00:1",
                    (_, RecordType::A) => "8.8.8.8",
                    _ => "2606:4700:4700::1111",
                };
                let data = if query.query_type() == RecordType::A {
                    RData::A(A(address.parse().unwrap()))
                } else {
                    RData::AAAA(AAAA(address.parse().unwrap()))
                };
                response.add_answer(Record::from_rdata(query.name().clone(), 60, data));
            }
            socket
                .send_to(&response.to_vec().unwrap(), peer)
                .await
                .unwrap();
        }
    });
    let dns = Ok(resolver.clone());
    let url = |host| parse_url(&format!("https://{host}/")).unwrap();
    assert_eq!(
        resolve(&url("mixed.example"), &dns).await,
        Err(Error::BlockedUrl)
    );
    let discovered = discovery.load(std::sync::atomic::Ordering::SeqCst);
    assert!(discovered >= 2, "both address families must be queried");
    assert_eq!(
        resolve(&url("translated.example"), &dns).await,
        Err(Error::BlockedUrl)
    );
    assert_eq!(
        resolve(&url("public.example"), &dns).await.unwrap().len(),
        2
    );
    assert_eq!(
        discovery.load(std::sync::atomic::Ordering::SeqCst),
        discovered,
        "subsequent hosts must reuse TTL-cached DNS64 discovery"
    );
    deny_discovery.store(true, std::sync::atomic::Ordering::SeqCst);
    resolver.clear_cache();
    assert_eq!(
        resolve(&url("public.example"), &dns).await,
        Err(Error::Resolution),
        "failed discovery must not silently permit unknown translation prefixes"
    );
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}

#[test]
fn full_answer_sets_reject_private_transition_and_discovered_nat64_destinations() {
    for ip in [
        "0.1.2.3",
        "10.0.0.1",
        "100.64.0.1",
        "127.0.0.1",
        "169.254.169.254",
        "172.31.255.255",
        "192.0.0.9",
        "192.0.2.1",
        "192.88.99.1",
        "192.168.1.2",
        "198.18.0.1",
        "198.51.100.2",
        "203.0.113.2",
        "224.0.0.1",
        "255.255.255.255",
        "::",
        "::1",
        "::ffff:127.0.0.1",
        "fe80::1",
        "fc00::1",
        "ff00::1",
        "2001::1",
        "2001:2::1",
        "2001:db8::1",
        "2002:7f00:1::1",
        "64:ff9b::7f00:1",
        "3fff::1",
        "3ffe::1",
        "2001:4860::5efe:7f00:1",
    ] {
        assert!(
            !public(ip.parse().unwrap()),
            "unexpected public address {ip}"
        );
    }
    for ip in [
        "8.8.8.8",
        "1.1.1.1",
        "100.128.0.1",
        "172.32.0.1",
        "198.20.0.1",
        "::ffff:8.8.8.8",
        "2001:4860:4860::8888",
        "2606:4700:4700::1111",
    ] {
        assert!(
            public(ip.parse().unwrap()),
            "unexpected blocked address {ip}"
        );
    }
    let public_answer: SocketAddr = "8.8.8.8:443".parse().unwrap();
    assert_eq!(
        checked(vec![public_answer, "127.0.0.1:443".parse().unwrap()], &[]),
        Err(Error::BlockedUrl)
    );
    assert_eq!(checked(vec![public_answer; 65], &[]), Err(Error::Capacity));
    assert_eq!(checked(vec![], &[]), Err(Error::Resolution));
    for length in [32, 40, 48, 56, 64, 96] {
        let translated = |v4: [u8; 4]| {
            let mut bytes = [0; 16];
            bytes[..4].copy_from_slice(&[0x26, 0x06, 0x47, 0x00]);
            if length == 96 {
                bytes[12..].copy_from_slice(&v4);
            } else {
                let before = 8 - length / 8;
                bytes[length / 8..8].copy_from_slice(&v4[..before]);
                bytes[9..9 + 4 - before].copy_from_slice(&v4[before..]);
            }
            SocketAddr::new(std::net::Ipv6Addr::from(bytes).into(), 443)
        };
        let prefix = translated([192, 0, 0, 170]);
        assert_eq!(
            checked(vec![translated([127, 0, 0, 1])], &[prefix]),
            Err(Error::BlockedUrl),
            "NAT64 /{length}"
        );
        assert!(
            checked(vec![translated([8, 8, 8, 8])], &[prefix]).is_ok(),
            "NAT64 /{length}"
        );
    }
}
#[test]
fn url_parsing_canonicalizes_obfuscated_literals_before_policy() {
    for value in [
        "http://2130706433/",
        "http://0x7f000001/",
        "http://127.1/",
        "file:///etc/passwd",
        "https://localhost./",
        "https://a.localhost/",
        "http://user:pass@example.com/",
        "https://[::ffff:127.0.0.1]/",
    ] {
        assert_eq!(parse_url(value), Err(Error::BlockedUrl), "{value}");
    }
    assert_eq!(
        parse_url("https://EXAMPLE.com:443/x#fragment")
            .unwrap()
            .as_str(),
        "https://example.com/x"
    );
    assert!(parse_url(&format!("https://example.com/{}", "x".repeat(MAX_URL))).is_err());
}

#[tokio::test]
async fn redirect_walk_rechecks_each_hop_and_refuses_cross_origin_or_sixth_redirect() {
    for location in [
        "https://other.example/",
        "http://example.com/",
        "https://example.com:444/",
        "https://127.0.0.1/",
    ] {
        let mut calls = 0;
        let result = walk("https://example.com/start", |_| {
            calls += 1;
            std::future::ready(Ok(Hop::Redirect(location.into())))
        })
        .await;
        assert!(matches!(result, Err(Error::Redirect | Error::BlockedUrl)));
        assert_eq!(calls, 1);
    }
    let mut calls = 0;
    let result = walk("https://example.com/start", |_| {
        calls += 1;
        std::future::ready(Ok(Hop::Redirect("/again".into())))
    })
    .await;
    assert!(matches!(result, Err(Error::Redirect)));
    assert_eq!(calls, 6);
    let mut visited = vec![];
    let result = walk("https://example.com/start", |url| {
        visited.push(url.to_string());
        std::future::ready(if visited.len() == 1 {
            Ok(Hop::Redirect("/next".into()))
        } else {
            Err(Error::BlockedUrl)
        })
    })
    .await;
    assert!(matches!(result, Err(Error::BlockedUrl)));
    assert_eq!(
        visited,
        ["https://example.com/start", "https://example.com/next"]
    );
}
async fn fixture(reply: Vec<u8>) -> (SocketAddr, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = vec![];
        let mut piece = [0; 1024];
        while !request.windows(4).any(|part| part == b"\r\n\r\n") {
            let n = socket.read(&mut piece).await.unwrap();
            assert!(n > 0);
            request.extend_from_slice(&piece[..n]);
            assert!(request.len() < 64 * 1024);
        }
        let end = request
            .windows(4)
            .position(|part| part == b"\r\n\r\n")
            .unwrap()
            + 4;
        let length: usize = String::from_utf8_lossy(&request[..end])
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse().unwrap())
            })
            .unwrap_or(0);
        assert!(length < 32 * 1024);
        while request.len() < end + length {
            let n = socket.read(&mut piece).await.unwrap();
            assert!(n > 0);
            request.extend_from_slice(&piece[..n]);
        }
        let _ = socket.write_all(&reply).await;
        String::from_utf8(request).unwrap()
    });
    (address, task)
}
#[tokio::test]
async fn actual_pinned_connection_preserves_host_and_compressed_wire_bytes_without_dns() {
    // The test supplies an address directly only to the private transport primitive.
    // Production can reach it only after resolve() has validated the complete set.
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut gzip, b"hello pinned origin").unwrap();
    let compressed = gzip.finish().unwrap();
    let mut reply = format!("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",compressed.len()).into_bytes();
    reply.extend_from_slice(&compressed);
    let (address, task) = fixture(reply).await;
    let url = Url::parse(&format!(
        "http://pinned-origin.invalid:{}/source",
        address.port()
    ))
    .unwrap();
    let response = send(&url, &[address], None).await.unwrap();
    let actual = body(url, response).await.unwrap();
    assert_eq!(actual.bytes, compressed);
    let request = task.await.unwrap();
    assert!(
        request
            .to_ascii_lowercase()
            .contains(&format!("host: pinned-origin.invalid:{}", address.port()))
    );
    assert!(!request.contains("authorization:"));
    assert_eq!(
        crate::decode::extract(&actual, &tokio_util::sync::CancellationToken::new())
            .unwrap()
            .text,
        "hello pinned origin"
    );
}
#[tokio::test]
async fn declared_chunked_and_duplicate_response_metadata_fail_at_wire_boundary() {
    let reply = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
        MAX_WIRE + 1
    )
    .into_bytes();
    let (address, task) = fixture(reply).await;
    let url = Url::parse(&format!("http://wire.invalid:{}/", address.port())).unwrap();
    let response = send(&url, &[address], None).await.unwrap();
    assert!(matches!(body(url, response).await, Err(Error::Capacity)));
    task.await.unwrap();
    let bytes = vec![b'x'; MAX_WIRE + 1];
    let mut reply = format!(
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n",
        bytes.len()
    )
    .into_bytes();
    reply.extend(bytes);
    reply.extend_from_slice(b"\r\n0\r\n\r\n");
    let (address, task) = fixture(reply).await;
    let url = Url::parse(&format!("http://wire.invalid:{}/", address.port())).unwrap();
    let response = send(&url, &[address], None).await.unwrap();
    assert!(matches!(body(url, response).await, Err(Error::Capacity)));
    task.await.unwrap();
    let mut headers = HeaderMap::new();
    headers.append("content-type", HeaderValue::from_static("text/plain"));
    headers.append("content-type", HeaderValue::from_static("text/html"));
    assert_eq!(header(&headers, "content-type", 1024), Err(Error::Protocol));
}

#[tokio::test]
async fn credential_post_keeps_exact_body_and_never_follows_redirects() {
    let payload = serde_json::json!({"query":"中文 source", "numResults":5});
    let bytes = serde_json::to_vec(&payload).unwrap();
    let mut key = HeaderValue::from_static("Bearer isolated-exa-key");
    key.set_sensitive(true);
    for status in ["200 OK", "307 Temporary Redirect"] {
        let reply = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nLocation: /credential-must-not-be-replayed\r\nContent-Length: 14\r\nConnection: close\r\n\r\n{{\"results\":[]}}").into_bytes();
        let (address, task) = fixture(reply).await;
        let url = Url::parse(&format!(
            "http://search-origin.invalid:{}/search",
            address.port()
        ))
        .unwrap();
        let response = send(&url, &[address], Some((key.clone(), bytes.clone())))
            .await
            .unwrap();
        if status.starts_with("200") {
            assert_eq!(
                body(url, response).await.unwrap().bytes,
                br#"{"results":[]}"#
            );
        } else {
            assert_eq!(response.status().as_u16(), 307);
            assert!(matches!(body(url, response).await, Err(Error::HttpStatus)));
        }
        let request = task.await.unwrap();
        let (headers, actual) = request.split_once("\r\n\r\n").unwrap();
        assert!(headers.starts_with("POST /search HTTP/1.1\r\n"));
        let headers = headers.to_ascii_lowercase();
        assert!(headers.contains("authorization: bearer isolated-exa-key\r\n"));
        assert!(headers.contains("content-type: application/json\r\n"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(actual).unwrap(),
            payload
        );
    }
}

#[test]
fn model_fetches_reject_non_web_tcp_ports_before_resolution() {
    for port in [0, 25, 6379, 11211, 65535] {
        assert_eq!(
            parse_url(&format!("http://example.com:{port}/")),
            Err(Error::BlockedUrl)
        );
    }
    for url in [
        "http://example.com/",
        "https://example.com/",
        "http://example.com:80/",
        "https://example.com:443/",
    ] {
        assert!(parse_url(url).is_ok());
    }
}
