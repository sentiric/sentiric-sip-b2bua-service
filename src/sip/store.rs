// sentiric-sip-b2bua-service/src/sip/store.rs
use dashmap::{DashMap, DashSet};
use redis::aio::ConnectionManager;
use redis::AsyncCommands;
use sentiric_rtp_core::RtpEndpoint;
use sentiric_sip_core::transaction::SipTransaction;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;

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
    early_cancelled: Arc<DashSet<String>>,
    invites_in_flight: Arc<DashSet<String>>,
    // [ARCH-COMPLIANCE FIX] Redis artık opsiyonel.
    redis: Arc<tokio::sync::RwLock<Option<ConnectionManager>>>,
}

impl CallStore {
    pub async fn new(redis_url: &str) -> Self {
        let store = Self {
            local_cache: Arc::new(DashMap::new()),
            early_cancelled: Arc::new(DashSet::new()),
            invites_in_flight: Arc::new(DashSet::new()),
            redis: Arc::new(tokio::sync::RwLock::new(None)),
        };

        let redis_clone = store.redis.clone();
        let url = redis_url.to_string();

        tokio::spawn(async move {
            if let Ok(client) = redis::Client::open(url.as_str()) {
                let mut first_error = true;
                loop {
                    match ConnectionManager::new(client.clone()).await {
                        Ok(conn) => {
                            tracing::info!(
                                event = "REDIS_RECOVERED",
                                "✅ B2BUA Redis bağlantısı sağlandı."
                            );
                            *redis_clone.write().await = Some(conn);
                            break;
                        }
                        Err(e) => {
                            if first_error {
                                tracing::error!(
                                    event = "REDIS_ERROR",
                                    error = %e,
                                    "Redis yok! B2BUA RAM Cache (DashMap) üzerinde çalışacak."
                                );
                                first_error = false;
                            }
                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        }
                    }
                }
            }
        });

        store
    }

    pub async fn try_lock_invite(&self, call_id: &str) -> bool {
        self.invites_in_flight.insert(call_id.to_string())
    }

    pub async fn unlock_invite(&self, call_id: &str) {
        self.invites_in_flight.remove(call_id);
    }

    pub async fn insert(&self, session: CallSession) {
        let call_id = session.data.call_id.clone();
        self.local_cache.insert(call_id.clone(), session.clone());

        if let Some(mut conn) = self.redis.read().await.clone() {
            if let Ok(json) = serde_json::to_string(&session.data) {
                let key = format!("b2bua:call:{}", call_id);
                let _: redis::RedisResult<()> = conn.set_ex(&key, json, 86400).await;
            }
        }
    }

    pub async fn update_state(&self, call_id: &str, new_state: CallState) {
        if let Some(mut entry) = self.local_cache.get_mut(call_id) {
            entry.data.state = new_state.clone();

            if let Some(mut conn) = self.redis.read().await.clone() {
                if let Ok(json) = serde_json::to_string(&entry.data) {
                    let key = format!("b2bua:call:{}", call_id);
                    let _: redis::RedisResult<()> = conn.set_ex(&key, json, 86400).await;
                }
            }
        }
    }

    pub async fn get(&self, call_id: &str) -> Option<CallSession> {
        if let Some(session) = self.local_cache.get(call_id) {
            return Some(session.clone());
        }

        if let Some(mut conn) = self.redis.read().await.clone() {
            let key = format!("b2bua:call:{}", call_id);
            let result: redis::RedisResult<String> = conn.get(&key).await;
            if let Ok(json) = result {
                if let Ok(data) = serde_json::from_str::<CallSessionData>(&json) {
                    let session = CallSession::new(data);
                    self.local_cache
                        .insert(call_id.to_string(), session.clone());
                    return Some(session);
                }
            }
        }
        None
    }

    pub async fn remove(&self, call_id: &str) -> Option<CallSession> {
        if let Some(mut conn) = self.redis.read().await.clone() {
            let key = format!("b2bua:call:{}", call_id);
            let _: redis::RedisResult<()> = conn.del(&key).await;
        }
        self.early_cancelled.remove(call_id);
        self.local_cache.remove(call_id).map(|(_, s)| s)
    }

    pub async fn mark_early_cancelled(&self, call_id: &str) {
        self.early_cancelled.insert(call_id.to_string());
    }

    pub async fn is_early_cancelled(&self, call_id: &str) -> bool {
        self.early_cancelled.contains(call_id)
    }
}
