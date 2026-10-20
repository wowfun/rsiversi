use std::{
    collections::BTreeMap,
    io::{Read as _, Write as _},
    time::Duration,
};

struct Response {
    status: u16,
    headers: BTreeMap<String, String>,
}

fn request(origin: &str, method: &str, path: &str, headers: &[(&str, &str)]) -> Response {
    let address = rsi_dev_control::loopback_address(origin).unwrap();
    let mut stream =
        std::net::TcpStream::connect_timeout(&address, Duration::from_secs(5)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(stream, "{method} {path} HTTP/1.1\r\n").unwrap();
    if !headers.iter().any(|(name, _)| *name == "Host") {
        write!(stream, "Host: {address}\r\n").unwrap();
    }
    for (name, value) in headers {
        write!(stream, "{name}: {value}\r\n").unwrap();
    }
    write!(stream, "Content-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
    let mut bytes = Vec::new();
    stream.take(256 * 1024).read_to_end(&mut bytes).unwrap();
    let text = String::from_utf8(bytes).unwrap();
    let (head, _) = text
        .split_once("\r\n\r\n")
        .expect("complete response headers");
    let mut lines = head.lines();
    let status = lines
        .next()
        .unwrap()
        .split_ascii_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers = lines
        .map(|line| {
            let (name, value) = line.split_once(':').unwrap();
            (name.to_ascii_lowercase(), value.trim().to_owned())
        })
        .collect();
    Response { status, headers }
}

pub(super) fn assert_public_document(origin: &str) {
    let response = request(origin, "GET", "/", &[]);
    assert_eq!(response.status, 200, "public document must be available");
    assert!(response.headers.contains_key("content-security-policy"));
}

pub(super) fn assert_browser_handoff(url: &str) {
    let (origin, ticket) = url.split_once("#rsi-launch=").expect("one-use launch link");
    let bootstrap =
        |headers: &[(&str, &str)]| request(origin, "POST", "/api/v1/browser-bootstrap", headers);
    for (case, host, browser_origin, csrf) in [
        ("foreign host", "127.0.0.1:1", origin, "1"),
        (
            "foreign origin",
            origin.trim_start_matches("http://"),
            "http://127.0.0.1:1",
            "1",
        ),
        (
            "missing csrf",
            origin.trim_start_matches("http://"),
            origin,
            "",
        ),
    ] {
        let mut headers = vec![
            ("Host", host),
            ("Origin", browser_origin),
            ("X-Rsi-Launch-Ticket", ticket),
        ];
        if !csrf.is_empty() {
            headers.push(("X-Rsi-Csrf", csrf));
        }
        assert_eq!(
            bootstrap(&headers).status,
            401,
            "{case} must be rejected before ticket consumption"
        );
    }
    let headers = [
        ("Origin", origin),
        ("X-Rsi-Csrf", "1"),
        ("X-Rsi-Launch-Ticket", ticket),
    ];
    let admitted = bootstrap(&headers);
    assert_eq!(
        admitted.status, 200,
        "the same ticket must still redeem through the proxy"
    );
    let cookie = admitted
        .headers
        .get("set-cookie")
        .expect("bootstrap cookie")
        .split(';')
        .next()
        .unwrap();
    assert_eq!(
        bootstrap(&headers).status,
        401,
        "one-use ticket replay must fail"
    );
    assert_eq!(
        bootstrap(&[("Origin", origin), ("X-Rsi-Csrf", "1"), ("Cookie", cookie)]).status,
        200
    );
    assert_eq!(
        bootstrap(&[("Origin", origin), ("Cookie", cookie)]).status,
        401
    );
    assert_eq!(
        request(
            origin,
            "POST",
            "/api/v1/not-a-registered-route",
            &[("Origin", "http://127.0.0.1:1"), ("Cookie", cookie)]
        )
        .status,
        401,
        "the native fence must run before route dispatch"
    );
}
