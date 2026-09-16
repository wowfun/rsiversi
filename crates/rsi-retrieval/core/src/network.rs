use crate::service::Work;
use crate::{MAX_URL, MAX_WIRE, RetrievalError as Error};
use futures_util::StreamExt;
use hickory_resolver::{TokioResolver, config::LookupIpStrategy};
use reqwest::{
    Client, Response,
    header::{HeaderMap, HeaderValue},
};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
};
use url::{Host, Url};

pub(crate) fn parse_url(value: &str) -> Result<Url, Error> {
    if !crate::safe_url(value) {
        return Err(Error::BlockedUrl);
    }
    let mut url = Url::parse(value).map_err(|_| Error::BlockedUrl)?;
    if !matches!(url.port_or_known_default(), Some(80 | 443)) {
        return Err(Error::BlockedUrl);
    }
    match url.host().ok_or(Error::BlockedUrl)? {
        Host::Ipv4(ip) if !public(ip.into()) => return Err(Error::BlockedUrl),
        Host::Ipv6(ip) if !public(ip.into()) => return Err(Error::BlockedUrl),
        Host::Domain(host)
            if host.trim_end_matches('.').eq_ignore_ascii_case("localhost")
                || host
                    .trim_end_matches('.')
                    .to_ascii_lowercase()
                    .ends_with(".localhost") =>
        {
            return Err(Error::BlockedUrl);
        }
        _ => {}
    }
    url.set_fragment(None);
    Ok(url)
}
fn v4_public(ip: Ipv4Addr) -> bool {
    let v = u32::from(ip);
    ![
        (0x0000_0000, 8),
        (0x0a00_0000, 8),
        (0x6440_0000, 10),
        (0x7f00_0000, 8),
        (0xa9fe_0000, 16),
        (0xac10_0000, 12),
        (0xc000_0000, 24),
        (0xc000_0200, 24),
        (0xc058_6300, 24),
        (0xc0a8_0000, 16),
        (0xc612_0000, 15),
        (0xc633_6400, 24),
        (0xcb00_7100, 24),
        (0xe000_0000, 3),
    ]
    .iter()
    .any(|&(prefix, bits)| v >> (32 - bits) == prefix >> (32 - bits))
}
/// Conservative globally-routable destination policy, independent of URL parsing.
pub(crate) fn public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => v4_public(ip),
        IpAddr::V6(ip) => {
            if let Some(v4) = ip.to_ipv4_mapped() {
                return v4_public(v4);
            }
            let v = u128::from(ip);
            let b = ip.octets();
            v >> 125 == 1
                && v >> 105 != 0x0020_0100_u128 >> 1 // 2001::/23: protocol assignments and transition space
                && v >> 96 != 0x2001_0db8
                && v >> 112 != 0x2002
                && v >> 112 != 0x3ffe
                && v >> 108 != 0x3fff0
                && !((b[8..12] == [0,0,0x5e,0xfe]) || (b[8..12] == [2,0,0x5e,0xfe]))
        }
    }
}
fn embedded(bytes: &[u8; 16], length: usize) -> Option<Ipv4Addr> {
    let start = length / 8;
    if length == 96 {
        return Some(Ipv4Addr::new(bytes[12], bytes[13], bytes[14], bytes[15]));
    }
    if bytes[8] != 0 {
        return None;
    }
    let mut out = [0; 4];
    let before = 8 - start;
    out[..before].copy_from_slice(&bytes[start..8]);
    out[before..].copy_from_slice(&bytes[9..9 + 4 - before]);
    Some(out.into())
}
fn checked(addresses: Vec<SocketAddr>, discovery: &[SocketAddr]) -> Result<Vec<SocketAddr>, Error> {
    if addresses.is_empty() {
        return Err(Error::Resolution);
    }
    if addresses.len() > 64 || discovery.len() > 64 {
        return Err(Error::Capacity);
    }
    let mut prefixes = Vec::new();
    for answer in discovery {
        let IpAddr::V6(ip) = answer.ip() else {
            continue;
        };
        let bytes = ip.octets();
        for length in [32, 40, 48, 56, 64, 96] {
            if embedded(&bytes, length)
                .is_some_and(|v4| matches!(v4.octets(), [192, 0, 0, 170 | 171]))
            {
                prefixes.push((bytes, length));
            }
        }
    }
    for address in &addresses {
        if !public(address.ip()) {
            return Err(Error::BlockedUrl);
        }
        if let IpAddr::V6(ip) = address.ip() {
            let bytes = ip.octets();
            for (prefix, length) in &prefixes {
                if bytes[..length / 8] == prefix[..length / 8]
                    && embedded(&bytes, *length).is_none_or(|v4| !v4_public(v4))
                {
                    return Err(Error::BlockedUrl);
                }
            }
        }
    }
    Ok(addresses)
}
pub(crate) fn system_resolver() -> Result<Arc<TokioResolver>, Error> {
    let mut builder = TokioResolver::builder_tokio().map_err(|_| Error::Resolution)?;
    let options = builder.options_mut();
    options.ip_strategy = LookupIpStrategy::Ipv4AndIpv6;
    options.timeout = std::time::Duration::from_secs(2);
    options.attempts = 2;
    options.cache_size = 64;
    Ok(Arc::new(builder.build()))
}
async fn lookup(resolver: &TokioResolver, host: &str, port: u16) -> Result<Vec<SocketAddr>, Error> {
    // Absolute URL authorities never acquire a resolver search-domain suffix.
    let name = format!("{}.", host.trim_end_matches('.'));
    let answer = resolver
        .lookup_ip(name)
        .await
        .map_err(|_| Error::Resolution)?;
    let addresses = answer
        .iter()
        .take(65)
        .map(|ip| SocketAddr::new(ip, port))
        .collect::<Vec<_>>();
    if addresses.len() > 64 {
        return Err(Error::Capacity);
    }
    Ok(addresses)
}
async fn resolve(
    url: &Url,
    dns: &Result<Arc<TokioResolver>, Error>,
) -> Result<Vec<SocketAddr>, Error> {
    let host = url
        .host_str()
        .ok_or(Error::BlockedUrl)?
        .trim_matches(['[', ']']);
    let port = url.port_or_known_default().ok_or(Error::BlockedUrl)?;
    let addresses = if let Ok(ip) = host.parse::<IpAddr>() {
        vec![SocketAddr::new(ip, port)]
    } else {
        lookup(dns.as_ref().map_err(|error| *error)?, host, port).await?
    };
    let discovery = if addresses.iter().any(SocketAddr::is_ipv6) {
        lookup(dns.as_ref().map_err(|error| *error)?, "ipv4only.arpa", port).await?
    } else {
        vec![]
    };
    checked(addresses, &discovery)
}
fn client(url: &Url, addresses: &[SocketAddr]) -> Result<Client, Error> {
    Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd()
        .connect_timeout(std::time::Duration::from_secs(10))
        .resolve_to_addrs(url.host_str().ok_or(Error::BlockedUrl)?, addresses)
        .build()
        .map_err(|_| Error::Network)
}
async fn send(
    url: &Url,
    addresses: &[SocketAddr],
    post: Option<(HeaderValue, Vec<u8>)>,
) -> Result<Response, Error> {
    let client = client(url, addresses)?;
    let request = if let Some((key, body)) = post {
        client
            .post(url.clone())
            .header("authorization", key)
            .header("content-type", "application/json")
            .body(body)
    } else {
        client.get(url.clone())
    };
    request
        .header(
            "accept",
            "text/html,application/xhtml+xml,text/plain,application/json;q=0.9,*/*;q=0.1",
        )
        .header("accept-encoding", "gzip, br")
        .header("user-agent", "rsiversi/0.0.1")
        .send()
        .await
        .map_err(|_| Error::Network)
}
fn header(headers: &HeaderMap, name: &'static str, maximum: usize) -> Result<String, Error> {
    if headers.get_all(name).iter().count() > 1 {
        return Err(Error::Protocol);
    }
    headers.get(name).map_or(Ok(String::new()), |value| {
        if value.as_bytes().len() > maximum {
            return Err(Error::Capacity);
        }
        value
            .to_str()
            .map(str::to_owned)
            .map_err(|_| Error::Protocol)
    })
}
#[derive(Debug)]
pub(crate) struct Body {
    pub url: Url,
    pub media: String,
    pub encoding: String,
    pub bytes: Vec<u8>,
}
async fn body(url: Url, response: Response) -> Result<Body, Error> {
    if !response.status().is_success() {
        return Err(Error::HttpStatus);
    }
    if response
        .content_length()
        .is_some_and(|n| n > MAX_WIRE as u64)
    {
        return Err(Error::Capacity);
    }
    let media = header(response.headers(), "content-type", 1024)?;
    let encoding = header(response.headers(), "content-encoding", 64)?;
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| Error::Network)?;
        if chunk.len() > MAX_WIRE.saturating_sub(bytes.len()) {
            return Err(Error::Capacity);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(Body {
        url,
        media,
        encoding,
        bytes,
    })
}
enum Hop {
    Redirect(String),
    Complete(Body),
}
async fn walk<F, Fut>(value: &str, mut request: F) -> Result<Body, Error>
where
    F: FnMut(Url) -> Fut,
    Fut: std::future::Future<Output = Result<Hop, Error>>,
{
    let mut url = parse_url(value)?;
    let origin = url.origin();
    for hop in 0..=5 {
        match request(url.clone()).await? {
            Hop::Complete(body) => return Ok(body),
            Hop::Redirect(location) => {
                if hop == 5 || location.is_empty() || location.len() > MAX_URL {
                    return Err(Error::Redirect);
                }
                let next = url.join(&location).map_err(|_| Error::Redirect)?;
                url = parse_url(next.as_str())?;
                if url.origin() != origin {
                    return Err(Error::Redirect);
                }
            }
        }
    }
    Err(Error::Redirect)
}
pub(crate) async fn fetch(value: &str, work: &Work) -> Result<Body, Error> {
    walk(value, |url| async move {
        // Every admitted hop resolves, checks all answers, and pins the actual connection.
        let addresses = resolve(&url, &work.dns).await?;
        let response = send(&url, &addresses, None).await?;
        if response.status().is_redirection() {
            if !matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
                return Err(Error::Redirect);
            }
            Ok(Hop::Redirect(header(
                response.headers(),
                "location",
                MAX_URL,
            )?))
        } else {
            body(url, response).await.map(Hop::Complete)
        }
    })
    .await
}
pub(crate) async fn exa(key: HeaderValue, bytes: Vec<u8>, work: &Work) -> Result<Body, Error> {
    let url = parse_url("https://api.exa.ai/search")?;
    let addresses = resolve(&url, &work.dns).await?;
    let response = send(&url, &addresses, Some((key, bytes))).await?;
    // Exa is an exact credential target; redirects never receive the key.
    body(url, response).await
}

#[cfg(test)]
mod tests;
