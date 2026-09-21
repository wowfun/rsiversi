use crate::wire::{self, Operation};
use rsi_acp_protocol::service::ExternalConversations;
use rsi_api_protocol::{ApiRegistrar, ApiRegistration, Result, json_handler};
use std::sync::Arc;
/// Owning endpoint registrations; retiring these does not close Host-owned peers.
#[derive(Debug)]
pub struct Endpoint {
    registrations: Vec<ApiRegistration>,
}
impl Endpoint {
    /// Registers the complete bounded external-conversation domain.
    pub fn register(
        registrar: &dyn ApiRegistrar,
        service: &Arc<dyn ExternalConversations>,
    ) -> Result<Self> {
        let mut registrations = Vec::new();
        macro_rules! endpoint {
            ($operation:ident, $input:ty, $owner:ident, $request:ident, $body:expr) => {{
                let owner = service.clone();
                registrations.push(registrar.register(
                    Operation::$operation.spec(),
                    json_handler(move |_, $request: $input| {
                        let $owner = owner.clone();
                        async move { Ok($body.await) }
                    }),
                )?);
            }};
        }
        endpoint!(Endpoints, wire::Empty, owner, _request, owner.endpoints());
        endpoint!(Residents, wire::Empty, owner, _request, owner.residents());
        endpoint!(List, wire::List, owner, request, owner.list(request.after));
        endpoint!(View, wire::Target, owner, request, owner.view(&request.id));
        endpoint!(
            Start,
            wire::Start,
            owner,
            request,
            owner.start(request.id, &request.endpoint)
        );
        endpoint!(
            Reconnect,
            wire::Reconnect,
            owner,
            request,
            owner.reconnect(&request.id, request.setup)
        );
        endpoint!(
            Submit,
            wire::Submit,
            owner,
            request,
            owner.submit(&request.id, &request.text)
        );
        endpoint!(
            Cancel,
            wire::Target,
            owner,
            request,
            owner.cancel(&request.id)
        );
        endpoint!(
            Close,
            wire::Target,
            owner,
            request,
            owner.close(&request.id)
        );
        endpoint!(Answer, wire::Answer, owner, request, async {
            owner
                .answer(
                    &request.id,
                    wire::number(&request.generation)?,
                    &request.permission,
                    &request.option,
                )
                .await
        });
        endpoint!(Page, wire::PageRequest, owner, request, async {
            let page = owner
                .page(
                    &request.id,
                    wire::number(&request.epoch)?,
                    wire::number(&request.after)?,
                )
                .await?;
            Ok::<_, rsi_acp_protocol::service::Error>(wire::PageReply {
                source: request,
                page,
            })
        });
        endpoint!(Window, wire::WindowRequest, owner, request, async {
            let bytes = owner
                .window(
                    &request.id,
                    wire::number(&request.epoch)?,
                    wire::number(&request.sequence)?,
                    request.start,
                )
                .await?;
            Ok::<_, rsi_acp_protocol::service::Error>(wire::WindowReply {
                source: request,
                hex: hex::encode(bytes),
            })
        });
        Ok(Self { registrations })
    }
    /// Withdraws endpoint admission and drains API handlers, leaving peer ownership in Host.
    pub async fn close(self) {
        for registration in self.registrations {
            registration.close().await;
        }
    }
}
