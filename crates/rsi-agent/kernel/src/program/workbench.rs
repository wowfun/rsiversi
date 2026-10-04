use super::*;
impl AgentKernel {
    pub(super) async fn read_program_blob(
        &self,
        binding: &rsi_agent_session_protocol::ProgramBlob,
    ) -> TurnResult<rsi_api_protocol::RetainedBytes> {
        self.inner
            .store
            .read_cas(
                &rsi_agent_store_protocol::CasObjectRef {
                    sha256: binding.sha256.clone(),
                    byte_len: binding.bytes,
                },
                self.inner.program_bytes.clone().into(),
            )
            .await
            .map_err(|error| match error {
                StoreError::NotFound(digest) => {
                    TurnError::Store(format!("missing workflow CAS object {digest}"))
                }
                error => turn_store_error(error),
            })
    }
}
