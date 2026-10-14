use super::{CredentialRef, CredentialsError, Failure, Result, SecretValue};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io::Write};
use zeroize::Zeroizing;
pub(super) const MAX_BYTES: usize = 4 * 1024 * 1024;
const MAX_ENTRIES: usize = 4096;
pub(super) type Entries = BTreeMap<CredentialRef, SecretValue>;
fn failure(kind: Failure) -> CredentialsError {
    CredentialsError::Store(kind)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    entries: Vec<Entry>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    reference: CredentialRef,
    #[serde(deserialize_with = "secret")]
    secret: SecretValue,
}
fn secret<'de, D: serde::Deserializer<'de>>(
    decoder: D,
) -> std::result::Result<SecretValue, D::Error> {
    SecretValue::new(String::deserialize(decoder)?).map_err(serde::de::Error::custom)
}
pub(super) fn decode(bytes: &[u8]) -> Result<Entries> {
    let document: Document =
        serde_json::from_slice(bytes).map_err(|_| failure(Failure::Corrupt))?;
    if document.version != 1 {
        return Err(failure(Failure::Corrupt));
    }
    if document.entries.len() > MAX_ENTRIES {
        return Err(failure(Failure::TooLarge));
    }
    let mut entries = Entries::new();
    for entry in document.entries {
        if entries.insert(entry.reference, entry.secret).is_some() {
            return Err(failure(Failure::Corrupt));
        }
    }
    Ok(entries)
}
#[derive(Serialize)]
struct EncodedDocument<'a> {
    version: u32,
    entries: Vec<EncodedEntry<'a>>,
}
#[derive(Serialize)]
struct EncodedEntry<'a> {
    reference: &'a CredentialRef,
    secret: &'a str,
}
pub(super) fn encode(entries: &Entries) -> Result<Zeroizing<Vec<u8>>> {
    if entries.len() > MAX_ENTRIES {
        return Err(failure(Failure::TooLarge));
    }
    let document = EncodedDocument {
        version: 1,
        entries: entries
            .iter()
            .map(|(reference, secret)| EncodedEntry {
                reference,
                secret: secret.expose_secret(),
            })
            .collect(),
    };
    let mut counter = ByteCount(0);
    serde_json::to_writer(&mut counter, &document).map_err(|_| failure(Failure::TooLarge))?;
    let mut bytes = Zeroizing::new(Vec::with_capacity(counter.0));
    let mut writer = BoundedWriter(&mut bytes);
    serde_json::to_writer(&mut writer, &document).map_err(|_| failure(Failure::TooLarge))?;
    Ok(bytes)
}
struct BoundedWriter<'a>(&'a mut Vec<u8>);
struct ByteCount(usize);
impl Write for ByteCount {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_BYTES.saturating_sub(self.0) {
            return Err(std::io::Error::other("credential document exceeds limit"));
        }
        self.0 += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl Write for BoundedWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_BYTES.saturating_sub(self.0.len()) {
            return Err(std::io::Error::other("credential document exceeds limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
