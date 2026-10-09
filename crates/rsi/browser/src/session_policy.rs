use crate::BrowserPolicy;
use serde::{Deserialize, Serialize};
use url::Url;

/// Frozen anonymous Session destination policy. `LocalDev` is issued only after
/// the product admits the actual human mutation or approves the exact Tool open.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum SessionPolicy {
    /// Anonymous public HTTPS, with public-address validation at every connection.
    PublicWeb {},
    /// One literal loopback HTTP origin, without aliases or DNS expansion.
    LocalDev { origin: String },
}
impl SessionPolicy {
    /// # Errors
    /// Rejects noncanonical or unsupported local origins.
    pub fn validate(&self) -> Result<(), String> {
        if let Self::LocalDev { origin } = self {
            let url = Url::parse(origin).map_err(|_| "invalid local origin")?;
            if origin.len() > 8192
                || url.scheme() != "http"
                || !matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
                || !url.username().is_empty()
                || url.password().is_some()
                || url.path() != "/"
                || url.query().is_some()
                || url.fragment().is_some()
                || url.origin().ascii_serialization() != *origin
            {
                return Err("LocalDev requires one canonical literal HTTP loopback origin".into());
            }
        }
        Ok(())
    }
    /// # Errors
    /// Rejects URLs outside this frozen destination policy.
    pub fn navigate(&self, value: &str) -> Result<Url, String> {
        self.validate()?;
        if value.len() > 8192 || value.chars().any(char::is_control) {
            return Err("navigation URL exceeds 8192 UTF-8 bytes or contains controls".into());
        }
        let url = Url::parse(value).map_err(|_| "invalid navigation URL")?;
        if !url.username().is_empty() || url.password().is_some() {
            return Err("anonymous browser URL required".into());
        }
        match self {
            Self::PublicWeb {}
                if url.scheme() == "https"
                    && url.port_or_known_default() == Some(443)
                    && url.host_str().is_some() =>
            {
                Ok(url)
            }
            Self::LocalDev { origin } if url.origin().ascii_serialization() == *origin => Ok(url),
            _ => Err("navigation outside frozen Session policy".into()),
        }
    }
    pub(crate) fn local_destination(&self) -> Option<(String, u16, std::net::IpAddr)> {
        let Self::LocalDev { origin } = self else {
            return None;
        };
        let url = Url::parse(origin).ok()?;
        let host = url.host_str()?.to_owned();
        let ip = if host == "[::1]" {
            std::net::Ipv6Addr::LOCALHOST.into()
        } else {
            std::net::Ipv4Addr::LOCALHOST.into()
        };
        Some((host, url.port_or_known_default()?, ip))
    }
    pub(crate) fn allows(&self, host: &str, port: u16, transport: &str) -> bool {
        if transport == "connect" {
            return port == 443;
        }
        matches!(transport, "http" | "ws")
            && self
                .local_destination()
                .is_some_and(|(h, p, _)| h == host && p == port)
    }
}
#[derive(Clone, Debug)]
pub(crate) enum RuntimePolicy {
    Preview(BrowserPolicy),
    Session(SessionPolicy),
}
impl From<BrowserPolicy> for RuntimePolicy {
    fn from(value: BrowserPolicy) -> Self {
        Self::Preview(value)
    }
}
impl RuntimePolicy {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Preview(p) => p.validate(),
            Self::Session(p) => p.validate(),
        }
    }
    pub fn navigate(&self, url: &str) -> Result<Url, String> {
        match self {
            Self::Preview(p) => p.navigate(url),
            Self::Session(p) => p.navigate(url),
        }
    }
    pub fn allows(&self, host: &str, port: u16, transport: &str) -> bool {
        match self {
            Self::Preview(p) => transport == "connect" && p.allows_destination(host, port),
            Self::Session(p) => p.allows(host, port, transport),
        }
    }
    pub fn preview(&self) -> Result<&BrowserPolicy, String> {
        match self {
            Self::Preview(p) => Ok(p),
            Self::Session(_) => Err("preview operation on Session browser".into()),
        }
    }
    pub fn local_destination(&self) -> Option<(String, u16, std::net::IpAddr)> {
        match self {
            Self::Session(p) => p.local_destination(),
            Self::Preview(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_grant_is_an_exact_origin_and_public_connect_remains_separate() {
        let policy = SessionPolicy::LocalDev {
            origin: "http://localhost:3000".into(),
        };
        policy.validate().unwrap();
        assert!(policy.navigate("http://localhost:3000/app").is_ok());
        for url in [
            "http://127.0.0.1:3000/app",
            "http://localhost:3001/",
            "https://localhost:3000/",
        ] {
            assert!(policy.navigate(url).is_err());
        }
        assert!(policy.allows("localhost", 3000, "ws"));
        assert!(!policy.allows("127.0.0.1", 3000, "http"));
        assert!(!policy.allows("localhost", 3000, "connect"));
        assert!(policy.allows("public.example", 443, "connect"));
        for origin in [
            "http://0x7f000001:3000",
            "http://localhost:3000/",
            "http://user@localhost:3000",
            "http://localhost:3000/path",
        ] {
            assert!(
                SessionPolicy::LocalDev {
                    origin: origin.into()
                }
                .validate()
                .is_err()
            );
        }
    }
}
