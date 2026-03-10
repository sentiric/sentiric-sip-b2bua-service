// sentiric-b2bua-service/src/sip/handlers/media.rs

use std::sync::Arc;
use tokio::sync::Mutex;
use tonic::Request;
use sentiric_contracts::sentiric::media::v1::{AllocatePortRequest, ReleasePortRequest, PlayAudioRequest};
use crate::grpc::client::InternalClients;
use crate::config::AppConfig;
use sentiric_sip_core::sdp::SdpBuilder;
use sentiric_rtp_core::AudioProfile; 
use tokio::time::{Duration, Instant};
use std::str::FromStr;
use tracing::{info, warn};

pub struct MediaManager {
    clients: Arc<Mutex<InternalClients>>,
    config: Arc<AppConfig>,
}

impl MediaManager {
    pub fn new(clients: Arc<Mutex<InternalClients>>, config: Arc<AppConfig>) -> Self {
        Self { clients, config }
    }

    pub async fn allocate_port(&self, call_id: &str) -> anyhow::Result<u32> {
        let mut media_client = { let guard = self.clients.lock().await; guard.media.clone() }; 
        
        let mut attempt = 0;
        let mut backoff = Duration::from_millis(500);

        loop {
            attempt += 1;
            let start = Instant::now();
            let mut req = Request::new(AllocatePortRequest { call_id: call_id.to_string() });
            if let Ok(val) = tonic::metadata::MetadataValue::from_str(call_id) {
                req.metadata_mut().insert("x-trace-id", val);
            }

            info!(event="GRPC_OUT_ATTEMPT", grpc.target="media-service", grpc.method="AllocatePort", attempt=attempt, sip.call_id=%call_id, "📡 Medya portu talep ediliyor...");

            match media_client.allocate_port(req).await {
                Ok(res) => {
                    let port = res.into_inner().rtp_port;
                    info!(event="GRPC_OUT_SUCCESS", grpc.target="media-service", grpc.method="AllocatePort", attempt=attempt, latency_ms=start.elapsed().as_millis(), rtp.port=port, "✅ Medya portu başarıyla alındı.");
                    return Ok(port);
                },
                Err(e) => {
                    warn!(event="GRPC_OUT_FAIL", grpc.target="media-service", grpc.method="AllocatePort", attempt=attempt, latency_ms=start.elapsed().as_millis(), error=%e, "⚠️ Media Service yanıt vermiyor.");
                    if attempt >= 3 { 
                        return Err(anyhow::anyhow!("Media Service erişilemez: {}", e)); 
                    }
                    tokio::time::sleep(backoff).await;
                    backoff *= 2;
                }
            }
        }
    }

    pub async fn set_target(&self, rtp_port: u32, rtp_target: &str) -> anyhow::Result<()> {
        let mut media_client = { let guard = self.clients.lock().await; guard.media.clone() };
        let req = tonic::Request::new(PlayAudioRequest {
            audio_uri: "control://set_target".to_string(),
            server_rtp_port: rtp_port,
            rtp_target_addr: rtp_target.to_string(),
        });
        media_client.play_audio(req).await?;
        Ok(())
    }

    pub async fn release_port(&self, port: u32, call_id: &str) {
        let mut media_client = { let guard = self.clients.lock().await; guard.media.clone() };
        let mut req = Request::new(ReleasePortRequest { rtp_port: port });
        if let Ok(val) = tonic::metadata::MetadataValue::from_str(call_id) {
            req.metadata_mut().insert("x-trace-id", val);
        }
        tokio::spawn(async move { 
            let _ = media_client.release_port(req).await; 
        });
    }

    pub fn generate_sdp(&self, rtp_port: u32) -> Vec<u8> {
        let profile = AudioProfile::default();
        let mut builder = SdpBuilder::new(self.config.public_ip.clone(), rtp_port as u16)
            .with_ptime(profile.ptime)
            .with_rtcp(false); 
        for codec_conf in profile.codecs {
            builder = builder.add_codec(codec_conf.payload_type, codec_conf.name, codec_conf.rate, codec_conf.fmtp.as_deref());
        }
        builder.build().as_bytes().to_vec()
    }
}