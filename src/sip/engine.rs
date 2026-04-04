// sentiric-b2bua-service/src/sip/engine.rs
use crate::config::AppConfig;
use crate::grpc::client::InternalClients;
use crate::rabbitmq::RabbitMqClient;
use crate::sip::handlers::{calls::CallHandler, events::EventManager, media::MediaManager};
use crate::sip::store::CallStore;
use sentiric_sip_core::{
    transaction::{TransactionAction, TransactionEngine},
    HeaderName, Method, SipPacket,
};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, warn};

pub struct B2BuaEngine {
    calls: CallStore,
    transport: Arc<sentiric_sip_core::SipTransport>,
    call_handler: CallHandler,
}

impl B2BuaEngine {
    pub fn new(
        config: Arc<AppConfig>,
        clients: Arc<Mutex<InternalClients>>,
        calls: CallStore,
        transport: Arc<sentiric_sip_core::SipTransport>,
        rabbitmq: Arc<RabbitMqClient>,
    ) -> Self {
        let media_mgr = MediaManager::new(clients.clone(), config.clone());
        let event_mgr = EventManager::new(rabbitmq, config.tenant_id.clone());
        let call_handler = CallHandler::new(config, clients, calls.clone(), media_mgr, event_mgr);

        Self {
            calls,
            transport,
            call_handler,
        }
    }

    pub async fn send_outbound_invite(
        &self,
        call_id: &str,
        from_uri: &str,
        to_uri: &str,
    ) -> anyhow::Result<()> {
        self.call_handler
            .process_outbound_invite(self.transport.clone(), call_id, from_uri, to_uri)
            .await
    }

    pub async fn handle_packet(&self, packet: SipPacket, src_addr: SocketAddr) {
        let call_id = packet
            .get_header_value(HeaderName::CallId)
            .cloned()
            .unwrap_or_default();

        // ACK'lar stateless işlenir.
        if packet.method == Method::Ack {
            self.call_handler.process_ack(&call_id).await;
            return;
        }

        let session_opt = self.calls.get(&call_id).await;

        let action = if let Some(session) = session_opt {
            TransactionEngine::check(&session.active_transaction, &packet)
        } else {
            TransactionAction::ForwardToApp
        };

        match action {
            TransactionAction::Retransmit(cached_resp) => {
                debug!(event="SIP_RETRANSMIT", sip.call_id=%call_id, "Tekrar eden paket");
                let _ = self.transport.send(&cached_resp.to_bytes(), src_addr).await;
            }
            TransactionAction::Ignore => return,
            TransactionAction::ForwardToApp => {
                if packet.is_request() {
                    // [ARCH-COMPLIANCE] Non-exhaustive match hatası düzeltildi.
                    match packet.method {
                        Method::Invite => {
                            // [ARCH-COMPLIANCE] re-INVITE kontrolü eklenerek çağrı çatallanması engellendi.
                            if packet.is_in_dialog_request() {
                                self.call_handler
                                    .process_reinvite(self.transport.clone(), packet, src_addr)
                                    .await;
                            } else {
                                if self.calls.try_lock_invite(&call_id).await {
                                    self.call_handler
                                        .process_invite(self.transport.clone(), packet, src_addr)
                                        .await;
                                    self.calls.unlock_invite(&call_id).await;
                                } else {
                                    warn!(event="SIP_RACE_CONDITION_PREVENTED", sip.call_id=%call_id, "⚠️ Aynı INVITE için paralel işlem durduruldu (Retransmission koruması).");
                                }
                            }
                        }
                        Method::Bye => {
                            self.call_handler
                                .process_bye(self.transport.clone(), packet, src_addr)
                                .await
                        }
                        Method::Cancel => {
                            self.call_handler
                                .process_cancel(self.transport.clone(), packet, src_addr)
                                .await
                        }
                        // Diğer metodlar B2BUA için şu an stateless yoksayılabilir.
                        _ => {
                            debug!(event="SIP_METHOD_IGNORED", method=?packet.method, "Method ignored or not handled by B2BUA logic")
                        }
                    }
                }
            }
        }
    }

    pub async fn terminate_session(&self, call_id: &str) {
        self.call_handler
            .terminate_session(self.transport.clone(), call_id)
            .await;
    }
}
