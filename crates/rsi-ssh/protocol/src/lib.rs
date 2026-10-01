//! Bounded SSH connection data, without target-use or trust authority.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]
pub mod control;
pub mod execution;
pub mod frame;
pub mod initialization;
pub mod rpc;

use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Invalid external connection data, without echoing its contents.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SshInputError {
    /// Connection coordinates exceed the accepted grammar or bounds.
    #[error("invalid SSH connection coordinates")]
    Endpoint,
    /// Public key encoding, algorithm or size is unsupported.
    #[error("invalid or unsupported SSH host key")]
    HostKey,
}
/// Connection input result.
pub type Result<T> = std::result::Result<T, SshInputError>;

/// Exact network endpoint, without an OpenSSH alias or configurable options.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SshEndpoint {
    host: String,
    port: u16,
    user: String,
}
impl SshEndpoint {
    /// Validates a literal IP or DNS name and explicit account without network access.
    pub fn new(host: impl Into<String>, port: u16, user: impl Into<String>) -> Result<Self> {
        let (host, user) = (host.into(), user.into());
        let dns = host.len() <= 253
            && host.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && label.as_bytes()[0].is_ascii_alphanumeric()
                    && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
                    && label
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            });
        if host.is_empty()
            || (!dns && host.parse::<std::net::IpAddr>().is_err())
            || port == 0
            || user.is_empty()
            || user.len() > 64
            || !user.as_bytes()[0].is_ascii_alphanumeric()
            || !user
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            return Err(SshInputError::Endpoint);
        }
        Ok(Self { host, port, user })
    }
    /// Exact peer hostname or IP literal.
    pub fn host(&self) -> &str {
        &self.host
    }
    /// Explicit TCP port.
    pub const fn port(&self) -> u16 {
        self.port
    }
    /// Explicit target account.
    pub fn user(&self) -> &str {
        &self.user
    }
}
impl<'de> Deserialize<'de> for SshEndpoint {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            host: String,
            port: u16,
            user: String,
        }
        let raw = Raw::deserialize(deserializer)?;
        Self::new(raw.host, raw.port, raw.user).map_err(serde::de::Error::custom)
    }
}

/// Canonical bounded public-key wire bytes selected by the Local trust owner.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SshHostKey {
    algorithm: String,
    base64: String,
}
impl SshHostKey {
    /// Parses a public-key algorithm and canonical padded base64 payload.
    pub fn new(algorithm: impl Into<String>, base64: impl Into<String>) -> Result<Self> {
        let (algorithm, base64) = (algorithm.into(), base64.into());
        if algorithm.len() > 32 || base64.len() > 2048 {
            return Err(SshInputError::HostKey);
        }
        let bytes = STANDARD
            .decode(&base64)
            .map_err(|_| SshInputError::HostKey)?;
        if STANDARD.encode(&bytes) != base64 {
            return Err(SshInputError::HostKey);
        }
        let mut wire = bytes.as_slice();
        if string(&mut wire)? != algorithm.as_bytes() {
            return Err(SshInputError::HostKey);
        }
        match algorithm.as_str() {
            "ssh-ed25519" if string(&mut wire)?.len() == 32 => {}
            "ssh-rsa" => {
                let exponent = mpint(&mut wire)?;
                let modulus = mpint(&mut wire)?;
                if exponent.len() > 8
                    || exponent.last().is_none_or(|last| last & 1 == 0)
                    || (exponent.len() == 1 && exponent[0] < 3)
                {
                    return Err(SshInputError::HostKey);
                }
                let bits = modulus.len() * 8 - modulus[0].leading_zeros() as usize;
                if !(2048..=8192).contains(&bits) {
                    return Err(SshInputError::HostKey);
                }
            }
            "ecdsa-sha2-nistp256" => curve(&mut wire, b"nistp256", 65)?,
            "ecdsa-sha2-nistp384" => curve(&mut wire, b"nistp384", 97)?,
            "ecdsa-sha2-nistp521" => curve(&mut wire, b"nistp521", 133)?,
            _ => return Err(SshInputError::HostKey),
        }
        if !wire.is_empty() {
            return Err(SshInputError::HostKey);
        }
        Ok(Self { algorithm, base64 })
    }
    /// Parses one public-key line; an optional comment is ignored, never emitted.
    pub fn parse(line: &str) -> Result<Self> {
        if line.len() > 4096 || line.contains(['\r', '\n', '\0']) {
            return Err(SshInputError::HostKey);
        }
        let mut parts = line.split_ascii_whitespace();
        Self::new(
            parts.next().ok_or(SshInputError::HostKey)?,
            parts.next().ok_or(SshInputError::HostKey)?,
        )
    }
    /// Public-key algorithm recorded in `known_hosts`.
    pub fn algorithm(&self) -> &str {
        &self.algorithm
    }
    /// Canonical public-key payload, containing no private key material.
    pub fn base64(&self) -> &str {
        &self.base64
    }
    /// Allowed handshake signatures for this exact pinned key.
    pub fn signature_algorithms(&self) -> &str {
        if self.algorithm == "ssh-rsa" {
            "rsa-sha2-512,rsa-sha2-256"
        } else {
            &self.algorithm
        }
    }
    /// SHA-256 public-key fingerprint for an explicit Local trust confirmation.
    #[expect(
        clippy::missing_panics_doc,
        reason = "Private bytes are constructor-validated canonical base64"
    )]
    pub fn fingerprint(&self) -> String {
        let bytes = STANDARD
            .decode(&self.base64)
            .expect("validated canonical host key");
        format!("SHA256:{}", STANDARD_NO_PAD.encode(Sha256::digest(bytes)))
    }
}
impl<'de> Deserialize<'de> for SshHostKey {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            algorithm: String,
            base64: String,
        }
        let raw = Raw::deserialize(deserializer)?;
        Self::new(raw.algorithm, raw.base64).map_err(serde::de::Error::custom)
    }
}
fn string<'a>(input: &mut &'a [u8]) -> Result<&'a [u8]> {
    let length = input.get(..4).ok_or(SshInputError::HostKey)?;
    let length = u32::from_be_bytes(length.try_into().expect("four bytes")) as usize;
    let body = input.get(4..).ok_or(SshInputError::HostKey)?;
    let (value, tail) = body
        .split_at_checked(length)
        .ok_or(SshInputError::HostKey)?;
    *input = tail;
    Ok(value)
}
fn mpint<'a>(input: &mut &'a [u8]) -> Result<&'a [u8]> {
    let value = string(input)?;
    let first = *value.first().ok_or(SshInputError::HostKey)?;
    if first & 0x80 != 0 {
        return Err(SshInputError::HostKey);
    }
    if first == 0 {
        if value.get(1).is_none_or(|byte| byte & 0x80 == 0) {
            return Err(SshInputError::HostKey);
        }
        Ok(&value[1..])
    } else {
        Ok(value)
    }
}
fn curve(input: &mut &[u8], name: &[u8], bytes: usize) -> Result<()> {
    if string(input)? != name {
        return Err(SshInputError::HostKey);
    }
    let point = string(input)?;
    if point.len() != bytes || point.first() != Some(&4) {
        return Err(SshInputError::HostKey);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn field(bytes: &[u8]) -> Vec<u8> {
        [
            u32::try_from(bytes.len()).unwrap().to_be_bytes().as_slice(),
            bytes,
        ]
        .concat()
    }
    #[test]
    fn endpoint_rejects_options_alias_syntax_and_unknown_fields_without_io() {
        for host in [
            "-oProxyCommand=x",
            "user@host",
            "ssh://host",
            "host\nLocalCommand evil",
            "$(command)",
            "host/name",
            "*.test",
            "host.",
            "",
            ".host",
            "bad_name",
        ] {
            assert!(SshEndpoint::new(host, 22, "user").is_err(), "{host}");
        }
        for user in ["-u", "u\nHost *", "u@host", "u;command", ""] {
            assert!(SshEndpoint::new("host", 22, user).is_err());
        }
        for host in [
            "localhost",
            "build-1.example.test",
            "127.0.0.1",
            "::1",
            "2001:db8::1",
        ] {
            assert!(SshEndpoint::new(host, 2222, "account-name").is_ok());
        }
        assert!(SshEndpoint::new("host", 0, "user").is_err());
        assert!(
            serde_json::from_value::<SshEndpoint>(
                serde_json::json!({"host":"host","port":22,"user":"user","ProxyCommand":"evil"})
            )
            .is_err()
        );
    }
    #[test]
    fn public_key_wire_is_bounded_canonical_and_completely_consumed() {
        let bytes = [field(b"ssh-ed25519"), field(&[7; 32])].concat();
        let key = SshHostKey::new("ssh-ed25519", STANDARD.encode(&bytes)).unwrap();
        assert!(key.fingerprint().starts_with("SHA256:"));
        assert_eq!(
            key,
            serde_json::from_value(serde_json::to_value(&key).unwrap()).unwrap()
        );
        for length in 0..bytes.len() {
            assert!(SshHostKey::new("ssh-ed25519", STANDARD.encode(&bytes[..length])).is_err());
        }
        assert!(
            SshHostKey::new(
                "ssh-ed25519",
                STANDARD.encode([bytes.as_slice(), &[0]].concat())
            )
            .is_err()
        );
        assert!(SshHostKey::new("ssh-rsa", key.base64()).is_err());
        assert!(SshHostKey::new("ssh-ed25519", format!("{}=", key.base64())).is_err());
        assert!(
            SshHostKey::parse(&format!(
                "{} {}\nInclude bad",
                key.algorithm(),
                key.base64()
            ))
            .is_err()
        );
        let tiny_rsa = [field(b"ssh-rsa"), field(&[3]), field(&[1])].concat();
        assert!(SshHostKey::new("ssh-rsa", STANDARD.encode(tiny_rsa)).is_err());
    }
}
