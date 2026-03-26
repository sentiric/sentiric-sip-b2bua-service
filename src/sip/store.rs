// sentiric-b2bua-service/src/sip/store.rs

use std::sync::Arc;
use dashmap::{DashMap, DashSet};
use redis::AsyncCommands;
use redis::aio::ConnectionManager;
use serde::{Deserialize, Serialize};
use tracing::error;
use sentiric_sip_core::transaction::SipTransaction;
use sentiric_rtp_core::RtpEndpoint;
use std::net::SocketAddr;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum CallState {
    Null,
    Trying,
    Ringing,
    Established,
    Terminated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallSessionData {
    pub call_id: String,
    pub state: CallState,
    pub from_uri: String,
    pub to_uri: String,
    pub rtp_port: u32,
    pub local_tag: String,
    pub caller_tag: String,      
    pub client_contact: String,  
    pub proxy_addr: SocketAddr, 
}

#[derive(Debug, Clone)]
pub struct CallSession {
    pub data: CallSessionData,
    pub endpoint: RtpEndpoint,
    pub active_transaction: Option<SipTransaction>,
}

impl CallSession {
    pub fn new(data: CallSessionData) -> Self {
        Self {
            data,
            endpoint: RtpEndpoint::new(None),
            active_transaction: None,
        }
    }
}

#[derive(Clone)]
pub struct CallStore {
    local_cache: Arc<DashMap<String, CallSession>>,
    // [YENİ]: Henüz kurulmadan iptal edilen (Race Condition) çağrıların kara listesi
    early_cancelled: Arc<DashSet<String>>,
    redis: ConnectionManager,
}

impl CallStore {
    pub async fn new(redis_url: &str) -> anyhow::Result<Self> {
        let client = redis::Client::open(redis_url)?;
        let conn = ConnectionManager::new(client).await?;
        Ok(Self { 
            local_cache: Arc::new(DashMap::new()), 
            early_cancelled: Arc::new(DashSet::new()),
            redis: conn 
        })
    }

    pub async fn insert(&self, session: CallSession) {
        let call_id = session.data.call_id.clone();
        self.local_cache.insert(call_id.clone(), session.clone());
        if let Ok(json) = serde_json::to_string(&session.data) {
            let mut conn = self.redis.clone();
            let key = format!("b2bua:call:{}", call_id);
            let result: redis::RedisResult<()> = conn.set_ex(&key, json, 86400).await;
            if let Err(e) = result { 
                // [ARCH-COMPLIANCE] ARCH-007
                error!(event="REDIS_WRITE_ERROR", sip.call_id=%call_id, error=%e, "Redis write error for session"); 
            }
        }
    }

    pub async fn update_state(&self, call_id: &str, new_state: CallState) {
        if let Some(mut entry) = self.local_cache.get_mut(call_id) {
            entry.data.state = new_state.clone();
            if let Ok(json) = serde_json::to_string(&entry.data) {
                let mut conn = self.redis.clone();
                let key = format!("b2bua:call:{}", call_id);
                let _: redis::RedisResult<()> = conn.set_ex(&key, json, 86400).await;
            }
        }
    }

    pub async fn get(&self, call_id: &str) -> Option<CallSession> {
        if let Some(session) = self.local_cache.get(call_id) { return Some(session.clone()); }
        let key = format!("b2bua:call:{}", call_id);
        let mut conn = self.redis.clone();
        let result: redis::RedisResult<String> = conn.get(&key).await;
        if let Ok(json) = result {
            if let Ok(data) = serde_json::from_str::<CallSessionData>(&json) {
                let session = CallSession::new(data);
                self.local_cache.insert(call_id.to_string(), session.clone());
                return Some(session);
            }
        }
        None
    }

    pub async fn remove(&self, call_id: &str) -> Option<CallSession> {
        let key = format!("b2bua:call:{}", call_id);
        let mut conn = self.redis.clone();
        let _: redis::RedisResult<()> = conn.del(&key).await;
        self.early_cancelled.remove(call_id); // Temizlik
        self.local_cache.remove(call_id).map(|(_, s)| s)
    }

    // [YENİ]: Erken iptal işaretleme
    pub async fn mark_early_cancelled(&self, call_id: &str) {
        self.early_cancelled.insert(call_id.to_string());
    }

    // [YENİ]: Erken iptal kontrolü
    pub async fn is_early_cancelled(&self, call_id: &str) -> bool {
        self.early_cancelled.contains(call_id)
    }
}