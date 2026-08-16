use super::{DesiredState, GraphRevision, PendingEffect, Persistence, Result};

impl Persistence {
    pub(crate) fn reserve_apply(
        &mut self,
        composition_id: &str,
        command_id: &str,
        request_hash: &[u8],
        requested_desired: &DesiredState,
        graph_revision: GraphRevision,
    ) -> Result<()> {
        self.reserve_pending(
            composition_id,
            command_id,
            request_hash,
            &PendingEffect::Apply {
                requested_desired: requested_desired.clone(),
                graph_revision,
            },
            None,
        )
    }

    pub(crate) fn abandon_uncommitted_operation(&mut self, command_id: &str) -> Result<()> {
        let deleted = self.connection.execute(
            "DELETE FROM command_outcome WHERE command_id = ?1 AND status = 'pending'\
             AND NOT EXISTS(SELECT 1 FROM apply_journal WHERE command_id = ?1)",
            [command_id],
        )?;
        if deleted != 1 {
            return Err(crate::HostError::InvalidEnvelope(format!(
                "uncommitted operation {command_id:?} cannot be released"
            )));
        }
        Ok(())
    }
}
