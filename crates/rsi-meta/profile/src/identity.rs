//! Content identity of validated evaluated programs; no executable recipe format.
use super::*;

struct DigestWriter<'a>(&'a mut Sha256);
impl std::io::Write for DigestWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn field(digest: &mut Sha256, value: &str) {
    digest.update((value.len() as u64).to_le_bytes());
    digest.update(value.as_bytes());
}
fn json_field(digest: &mut Sha256, value: &impl serde::Serialize) -> Result<()> {
    serde_json::to_writer(DigestWriter(digest), value)
        .map_err(|_| ProfileError::InvalidProgram("evaluated Profile hashing failed".into()))?;
    // Literal NUL cannot occur in JSON text, so adjacent JSON fields cannot alias.
    digest.update([0]);
    Ok(())
}

impl ProfileCandidate {
    /// Hashes evaluated values without copying configuration or reevaluating sources.
    pub fn evaluated_digest(&self) -> Result<String> {
        let mut digest = Sha256::new();
        digest.update(b"rsi-profile-evaluated-v1\0");
        let mut pending: Vec<_> = self.tree.iter().rev().map(|node| (node, 0_u64)).collect();
        while let Some((node, depth)) = pending.pop() {
            digest.update(depth.to_le_bytes());
            field(&mut digest, node.id());
            match node {
                TreeNode::Plugin(plugin) => {
                    digest.update([0, u8::from(plugin.enabled)]);
                    field(&mut digest, plugin.plugin.as_str());
                    json_field(&mut digest, &plugin.config)?;
                }
                TreeNode::Group(group) => {
                    digest.update([1, u8::from(group.enabled)]);
                    json_field(
                        &mut digest,
                        &(
                            group.isolation.local.as_slice(),
                            group.isolation.events.as_slice(),
                            group.isolation.portable.as_slice(),
                            group.isolation.named.as_slice(),
                        ),
                    )?;
                    pending.extend(group.children.iter().rev().map(|node| (node, depth + 1)));
                }
            }
        }
        Ok(hex_lower(digest.finalize().as_slice()))
    }
}
