use sentiric_contracts::sentiric::sip::v1::{
    b2bua_service_server::B2buaService,
    InitiateCallRequest, InitiateCallResponse,
    TransferCallRequest, TransferCallResponse,
};
use tonic::{Request, Response, Status};
use tracing::{info, instrument, error, warn};
use std::sync::Arc;
use crate::sip::engine::B2BuaEngine;
use uuid::Uuid;

pub struct MyB2BuaService {
    engine: Arc<B2BuaEngine>,
}

impl MyB2BuaService {
    pub fn new(engine: Arc<B2BuaEngine>) -> Self {
        Self { engine }
    }
}

#[tonic::async_trait]
impl B2buaService for MyB2BuaService {
    
    #[instrument(skip(self), fields(from = %request.get_ref().from_uri, to = %request.get_ref().to_uri))]
    async fn initiate_call(
        &self,
        request: Request<InitiateCallRequest>,
    ) -> Result<Response<InitiateCallResponse>, Status> {
        let req = request.into_inner();
        
        let call_id = if req.call_id.is_empty() {
             Uuid::new_v4().to_string()
        } else {
             req.call_id
        };

        // [ARCH-COMPLIANCE] ARCH-007
        info!(event="RPC_INITIATE_CALL_RECEIVED", sip.call_id=%call_id, "🚀 [RPC] Dış arama başlatma emri alındı.");

        match self.engine.send_outbound_invite(&call_id, &req.from_uri, &req.to_uri).await {
            Ok(_) => {
                Ok(Response::new(InitiateCallResponse {
                    success: true,
                    new_call_id: call_id,
                }))
            },
            Err(e) => {
                // [ARCH-COMPLIANCE] ARCH-007
                error!(event="RPC_INITIATE_CALL_FAILED", sip.call_id=%call_id, error=%e, "❌ [RPC] Arama başlatılamadı.");
                Err(Status::internal(e.to_string()))
            }
        }
    }

    async fn transfer_call(&self, _req: Request<TransferCallRequest>) -> Result<Response<TransferCallResponse>, Status> {
        // [ARCH-COMPLIANCE] ARCH-007
        warn!(event="RPC_TRANSFER_CALL_UNSUPPORTED", "⚠️ TransferCall henüz desteklenmiyor.");
        Err(Status::unimplemented("Transfer not implemented"))
    }
}