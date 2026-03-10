// sentiric-b2bua-service/src/sip/handlers/calls.rs

use std::sync::Arc;
use tokio::sync::Mutex;
use std::net::SocketAddr;
use std::str::FromStr; 
use std::time::{Duration, Instant};
use tracing::{info, error, warn};
use sentiric_sip_core::{
    SipPacket, HeaderName, Header, SipUri,
    builder::SipResponseFactory,
    transaction::SipTransaction,
};
use sentiric_contracts::sentiric::dialplan::v1::{ResolveDialplanRequest};
use tonic::Request;
use crate::config::AppConfig;
use crate::sip::store::{CallStore, CallSession, CallSessionData, CallState};
use crate::grpc::client::InternalClients;
use crate::sip::handlers::media::MediaManager;
use crate::sip::handlers::events::EventManager;

pub struct CallHandler {
    config: Arc<AppConfig>,
    clients: Arc<Mutex<InternalClients>>,
    calls: CallStore,
    media_mgr: MediaManager,
    event_mgr: EventManager,
}

impl CallHandler {
    pub fn new(config: Arc<AppConfig>, clients: Arc<Mutex<InternalClients>>, calls: CallStore, media_mgr: MediaManager, event_mgr: EventManager) -> Self {
        Self { config, clients, calls, media_mgr, event_mgr }
    }

    fn extract_rtp_target_from_sdp(&self, body: &[u8]) -> Option<String> {
        let sdp_str = String::from_utf8_lossy(body);
        let mut ip = String::new();
        let mut port = 0u16;
        for line in sdp_str.lines() {
            if line.starts_with("c=IN IP4 ") { ip = line[9..].trim().to_string(); }
            else if line.starts_with("m=audio ") {
                if let Some(p_str) = line.split_whitespace().nth(1) { port = p_str.parse().unwrap_or(0); }
            }
        }
        if !ip.is_empty() && port > 0 { Some(format!("{}:{}", ip, port)) } else { None }
    }

    pub async fn process_invite(&self, transport: Arc<sentiric_sip_core::SipTransport>, req: SipPacket, src_addr: SocketAddr) {
        let call_id = req.get_header_value(HeaderName::CallId).cloned().unwrap_or_default();
        let from = req.get_header_value(HeaderName::From).cloned().unwrap_or_default();
        let to = req.get_header_value(HeaderName::To).cloned().unwrap_or_default();
        
        let caller_tag = from.split(";tag=").nth(1).unwrap_or("").to_string();
        let client_contact = req.get_header_value(HeaderName::Contact).cloned().unwrap_or_else(|| format!("<{}>", from));
        let to_uri = SipUri::from_str(&to).unwrap_or_else(|_| SipUri::from_str("sip:unknown@sentiric.local").unwrap());
        let to_aor = to_uri.user.unwrap_or_default();

        let _ = transport.send(&SipResponseFactory::create_100_trying(&req).to_bytes(), src_addr).await;

        // [SMART RETRY ENGINE]: Dialplan Service
        let mut dialplan_client = {
            let guard = self.clients.lock().await;
            guard.dialplan.clone()
        };

        let mut attempt = 0;
        let mut backoff = Duration::from_millis(500);
        let dialplan_res = loop {
            attempt += 1;
            let start = Instant::now();
            let mut dp_req = Request::new(ResolveDialplanRequest { caller_contact_value: from.clone(), destination_number: to_aor.clone() });
            if let Ok(val) = tonic::metadata::MetadataValue::from_str(&call_id) { dp_req.metadata_mut().insert("x-trace-id", val); }
            
            info!(event="GRPC_OUT_ATTEMPT", grpc.target="dialplan-service", attempt=attempt, sip.call_id=%call_id, "📡 Dialplan'a danışılıyor...");
            
            match dialplan_client.resolve_dialplan(dp_req).await {
                Ok(res) => {
                    info!(event="GRPC_OUT_SUCCESS", grpc.target="dialplan-service", attempt=attempt, latency_ms=start.elapsed().as_millis(), "✅ Dialplan kararı alındı.");
                    break Ok(res);
                },
                Err(e) => {
                    warn!(event="GRPC_OUT_FAIL", grpc.target="dialplan-service", attempt=attempt, latency_ms=start.elapsed().as_millis(), error=%e, "⚠️ Dialplan çağrısı başarısız.");
                    if attempt >= 3 { break Err(e); }
                    tokio::time::sleep(backoff).await;
                    backoff *= 2;
                }
            }
        };

        match dialplan_res {
            Ok(response) => {
                let resolution = response.into_inner();
                
                // Media Manager kendi içinde de retry yapıyor olmalı (media.rs içinde ayarlayacağız)
                let rtp_port = match self.media_mgr.allocate_port(&call_id).await {
                    Ok(p) => p,
                    Err(e) => {
                        error!(event="MEDIA_ALLOC_FATAL", sip.call_id=%call_id, error=%e, "❌ Media port tahsisi tüm denemelere rağmen başarısız.");
                        let _ = transport.send(&SipResponseFactory::create_error(&req, 503, "Media Error").to_bytes(), src_addr).await;
                        return;
                    }
                };

                let sbc_rtp_target = self.extract_rtp_target_from_sdp(&req.body).unwrap_or_else(|| format!("{}:{}", src_addr.ip(), 30000));
                let _ = self.media_mgr.set_target(rtp_port, &sbc_rtp_target).await;

                let local_tag = sentiric_sip_core::utils::generate_tag("b2bua");
                let sdp_body = self.media_mgr.generate_sdp(rtp_port);
                let mut ok_resp = SipResponseFactory::create_200_ok(&req);
                
                if let Some(to_h) = ok_resp.headers.iter_mut().find(|h| h.name == HeaderName::To) {
                    let clean_val = to_h.value.trim().to_string();
                    if !clean_val.contains(";tag=") { to_h.value = format!("{};tag={}", clean_val, local_tag); }
                }

                let contact_uri = format!("<sip:b2bua@{}:{}>", self.config.sbc_public_ip, 5060);
                ok_resp.headers.push(Header::new(HeaderName::Contact, contact_uri));
                ok_resp.headers.push(Header::new(HeaderName::ContentType, "application/sdp".to_string()));
                ok_resp.headers.retain(|h| h.name != HeaderName::ContentLength);
                ok_resp.headers.push(Header::new(HeaderName::ContentLength, sdp_body.len().to_string()));
                ok_resp.body = sdp_body;

                let mut tx = SipTransaction::new(&req).unwrap();
                tx.update_with_response(&ok_resp);

                let session_data = CallSessionData {
                    call_id: call_id.clone(),
                    state: CallState::Established,
                    from_uri: from.clone(),
                    to_uri: to.clone(),
                    rtp_port,
                    local_tag,
                    caller_tag,
                    client_contact,
                    proxy_addr: src_addr, 
                };
                self.calls.insert(CallSession::new(session_data)).await;

                if self.calls.is_early_cancelled(&call_id).await {
                    warn!(event="EARLY_CANCEL_CAUGHT", sip.call_id=%call_id, "⚠️ Arama kurulurken müşteri iptal etmiş. Port geri veriliyor.");
                    self.terminate_session(transport.clone(), &call_id).await;
                    return; 
                }

                if transport.send(&ok_resp.to_bytes(), src_addr).await.is_ok() {
                    self.event_mgr.publish_call_started(&call_id, rtp_port, &sbc_rtp_target, &from, &to, Some(resolution)).await;
                    info!(event="CALL_ESTABLISHED", sip.call_id=%call_id, rtp.port=rtp_port, "✅ Çağrı SIP seviyesinde kuruldu.");
                }
            },
            Err(e) => {
                error!(event="DIALPLAN_FATAL", sip.call_id=%call_id, error=%e, "❌ Dialplan'a erişilemediği için çağrı reddedildi.");
                let _ = transport.send(&SipResponseFactory::create_error(&req, 500, "Routing Error").to_bytes(), src_addr).await;
            }
        }
    }

    // Outbound, Bye, Cancel, Ack aynıdır... (Kısaltıldı)
    // ... [ÖNCEKİ KODLARIN AYNISI BURAYA GELECEK, VETO'DAKİ DİĞER FONKSİYONLAR DEĞİŞMİYOR]
    pub async fn process_outbound_invite(&self, transport: Arc<sentiric_sip_core::SipTransport>, call_id: &str, from_uri: &str, to_uri: &str) -> anyhow::Result<()> {
        let rtp_port = self.media_mgr.allocate_port(call_id).await?;
        let sdp_body = self.media_mgr.generate_sdp(rtp_port);
        let mut invite = SipPacket::new_request(sentiric_sip_core::Method::Invite, to_uri.to_string());
        let local_tag = sentiric_sip_core::utils::generate_tag("b2bua-out");
        
        invite.headers.push(Header::new(HeaderName::From, format!("<{}>;tag={}", from_uri, local_tag)));
        invite.headers.push(Header::new(HeaderName::To, format!("<{}>", to_uri)));
        invite.headers.push(Header::new(HeaderName::CallId, call_id.to_string()));
        invite.headers.push(Header::new(HeaderName::CSeq, "1 INVITE".to_string()));
        let contact_uri = format!("<sip:b2bua@{}:{}>", self.config.sbc_public_ip, 5060);
        invite.headers.push(Header::new(HeaderName::Contact, contact_uri.clone()));
        invite.headers.push(Header::new(HeaderName::ContentType, "application/sdp".to_string()));
        invite.headers.push(Header::new(HeaderName::ContentLength, sdp_body.len().to_string()));
        invite.body = sdp_body;

        if let Ok(mut addrs) = tokio::net::lookup_host(&self.config.proxy_sip_addr).await {
            if let Some(proxy_addr) = addrs.next() {
                let session_data = CallSessionData {
                    call_id: call_id.to_string(),
                    state: CallState::Trying,
                    from_uri: from_uri.to_string(),
                    to_uri: to_uri.to_string(),
                    rtp_port,
                    local_tag,
                    caller_tag: "".to_string(),
                    client_contact: contact_uri,
                    proxy_addr, 
                };
                self.calls.insert(CallSession::new(session_data)).await;
                transport.send(&invite.to_bytes(), proxy_addr).await?;
            }
        }
        Ok(())
    }

    pub async fn process_bye(&self, transport: Arc<sentiric_sip_core::SipTransport>, req: SipPacket, src_addr: SocketAddr) {
        let call_id = req.get_header_value(HeaderName::CallId).cloned().unwrap_or_default();
        let _ = transport.send(&SipResponseFactory::create_200_ok(&req).to_bytes(), src_addr).await;
        if let Some(session) = self.calls.remove(&call_id).await {
            self.media_mgr.release_port(session.data.rtp_port, &call_id).await;
            self.event_mgr.publish_call_ended(&call_id, "normal_clearing").await;
            info!(event = "CALL_TERMINATED", sip.call_id = %call_id, "✅ Çağrı temizlendi (İstemci kapattı)");
        }
    }

    pub async fn process_cancel(&self, transport: Arc<sentiric_sip_core::SipTransport>, req: SipPacket, src_addr: SocketAddr) {
        let call_id = req.get_header_value(HeaderName::CallId).cloned().unwrap_or_default();
        let _ = transport.send(&SipResponseFactory::create_200_ok(&req).to_bytes(), src_addr).await;
        
        self.calls.mark_early_cancelled(&call_id).await;

        if let Some(session) = self.calls.remove(&call_id).await {
            self.media_mgr.release_port(session.data.rtp_port, &call_id).await;
            self.event_mgr.publish_call_ended(&call_id, "cancelled").await;
            info!(event = "CALL_CANCELLED", sip.call_id = %call_id, "⛔ Çağrı kullanıcı tarafından kurulmadan iptal edildi.");
        }
    }

    pub async fn process_ack(&self, call_id: &str) {
        self.calls.update_state(call_id, CallState::Established).await;
        self.event_mgr.publish_call_answered(call_id).await;
    }

    pub async fn terminate_session(&self, transport: Arc<sentiric_sip_core::SipTransport>, call_id: &str) {
        if let Some(session) = self.calls.remove(call_id).await {
            self.media_mgr.release_port(session.data.rtp_port, call_id).await;
            self.event_mgr.publish_call_ended(call_id, "system_terminated").await;
            
            let r_uri = session.data.client_contact.replace("<", "").replace(">", "");
            let mut bye = SipPacket::new_request(sentiric_sip_core::Method::Bye, r_uri);
            
            let branch = sentiric_sip_core::utils::generate_branch_id();
            bye.headers.push(Header::new(HeaderName::Via, format!("SIP/2.0/UDP {}:{};branch={}", self.config.public_ip, self.config.sip_port, branch)));
            bye.headers.push(Header::new(HeaderName::MaxForwards, "70".to_string()));
            
            let to_val = if session.data.caller_tag.is_empty() {
                session.data.from_uri.clone()
            } else {
                format!("{};tag={}", session.data.from_uri, session.data.caller_tag)
            };
            bye.headers.push(Header::new(HeaderName::To, to_val));
            bye.headers.push(Header::new(HeaderName::From, format!("{};tag={}", session.data.to_uri, session.data.local_tag)));
            bye.headers.push(Header::new(HeaderName::CallId, call_id.to_string()));
            bye.headers.push(Header::new(HeaderName::CSeq, "999 BYE".to_string())); 
            bye.headers.push(Header::new(HeaderName::Route, format!("<sip:sbc@{}:{};lr>", self.config.sbc_public_ip, 5060)));
            bye.headers.push(Header::new(HeaderName::ContentLength, "0".to_string()));

            let proxy_ip = session.data.proxy_addr;
            match transport.send(&bye.to_bytes(), proxy_ip).await {
                Ok(_) => {
                    info!(event = "CALL_FORCE_TERMINATED", sip.call_id = %call_id, target = %proxy_ip, "🛑 Sistem tarafından çağrı zorla sonlandırıldı (Stateful BYE Proxy'e ulaştı).");
                },
                Err(e) => {
                    error!(event = "BYE_DELIVERY_FAILED", sip.call_id = %call_id, error = %e, target = %proxy_ip, "🚨 KRİTİK HATA: BYE paketi Proxy'e iletilemedi!");
                }
            }
        }
    }
}