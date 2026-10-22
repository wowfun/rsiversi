use crate::{Error, Result, invalid};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Repository selection identifies one manifest on one target.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryTarget {
    /// Exact manifest digest.
    pub digest: String,
    /// Relative manifest location below the repository URL.
    pub manifest: String,
}
/// Bounded signed payload; times are Unix seconds.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Repository {
    /// Format revision.
    pub format: u32,
    /// Repository authority.
    pub name: String,
    /// Strictly increasing publication sequence.
    pub sequence: u64,
    /// Issue time.
    pub issued: u64,
    /// Expiry time.
    pub expires: u64,
    /// Package name -> target -> manifest.
    pub packages: BTreeMap<String, BTreeMap<String, RepositoryTarget>>,
}
/// Signed bytes are verified exactly; reserialization is never signature input.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SignedRepository {
    /// Hex-encoded bounded UTF-8 JSON payload.
    pub payload: String,
    /// Hex-encoded Ed25519 signature.
    pub signature: String,
}
/// Durable rollback protection retained as soon as metadata is accepted.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TrustReceipt {
    /// Repository identity.
    pub repository: String,
    /// Highest accepted publication sequence.
    pub sequence: u64,
    /// Exact payload at that sequence.
    pub digest: String,
}
/// Authenticated repository metadata. Only signature verification constructs this value.
#[derive(Debug)]
pub struct VerifiedRepository {
    repository: Repository,
    receipt: TrustReceipt,
}
impl VerifiedRepository {
    /// Borrows the authenticated package index.
    pub fn repository(&self) -> &Repository {
        &self.repository
    }
    /// Borrows its exact authenticated publication identity.
    pub fn receipt(&self) -> &TrustReceipt {
        &self.receipt
    }
}
impl SignedRepository {
    /// Verifies a pinned authority, freshness and the previous accepted high-water mark.
    pub fn verify(
        &self,
        key: &[u8; 32],
        authority: &str,
        now: u64,
        previous: Option<&TrustReceipt>,
    ) -> Result<VerifiedRepository> {
        let fail = |message: &str| Error::Trust(message.into());
        if self.payload.len() > crate::MAX_DOCUMENT * 2 || self.signature.len() != 128 {
            return Err(fail("signature envelope bounds"));
        }
        let payload = hex::decode(&self.payload).map_err(|_| fail("payload encoding"))?;
        let signature = hex::decode(&self.signature).map_err(|_| fail("signature encoding"))?;
        ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key)
            .verify(&payload, &signature)
            .map_err(|_| fail("invalid signature"))?;
        let repository: Repository =
            serde_json::from_slice(&payload).map_err(|e| invalid(e.to_string()))?;
        crate::identifier(&repository.name)?;
        if repository.format != 1
            || repository.name != authority
            || repository.sequence == 0
            || repository.issued > now
            || repository.expires <= now
            || repository.expires <= repository.issued
            || repository.expires - repository.issued > 31 * 24 * 60 * 60
            || repository.packages.len() > 128
        {
            return Err(fail("authority, freshness or repository bounds"));
        }
        for (name, targets) in &repository.packages {
            crate::identifier(name)?;
            if targets.is_empty() || targets.len() > 8 {
                return Err(fail("target bounds"));
            }
            for (platform, target) in targets {
                crate::identifier(platform)?;
                crate::hash(&target.digest)?;
                crate::manifest::relative(&target.manifest)?;
            }
        }
        let receipt = TrustReceipt {
            repository: repository.name.clone(),
            sequence: repository.sequence,
            digest: crate::digest(&payload),
        };
        if let Some(previous) = previous
            && (previous.repository != authority
                || previous.sequence > receipt.sequence
                || (previous.sequence == receipt.sequence && previous.digest != receipt.digest))
        {
            return Err(fail("rollback or same-sequence equivocation"));
        }
        Ok(VerifiedRepository {
            repository,
            receipt,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::KeyPair as _;
    #[test]
    fn signed_metadata_rejects_tampering_expiry_rollback_and_equivocation() {
        let key = ring::signature::Ed25519KeyPair::from_seed_unchecked(&[7; 32]).unwrap();
        let public: [u8; 32] = key.public_key().as_ref().try_into().unwrap();
        let sign = |sequence, issued| {
            let payload = serde_json::to_vec(&Repository {
                format: 1,
                name: "fixture".into(),
                sequence,
                issued,
                expires: 200,
                packages: BTreeMap::new(),
            })
            .unwrap();
            SignedRepository {
                payload: hex::encode(&payload),
                signature: hex::encode(key.sign(&payload).as_ref()),
            }
        };
        let signed = sign(2, 100);
        let verified = signed.verify(&public, "fixture", 150, None).unwrap();
        let receipt = verified.receipt();
        signed
            .verify(&public, "fixture", 150, Some(receipt))
            .unwrap();
        assert!(signed.verify(&public, "fixture", 200, None).is_err());
        assert!(signed.verify(&public, "fixture", 99, None).is_err());
        assert!(signed.verify(&public, "other", 150, None).is_err());
        assert!(
            sign(1, 100)
                .verify(&public, "fixture", 150, Some(receipt))
                .is_err()
        );
        assert!(
            sign(2, 101)
                .verify(&public, "fixture", 150, Some(receipt))
                .is_err()
        );
        let mut altered = signed.clone();
        altered.payload.replace_range(0..2, "00");
        assert!(altered.verify(&public, "fixture", 150, None).is_err());
        assert!(signed.verify(&[0; 32], "fixture", 150, None).is_err());
    }
}
