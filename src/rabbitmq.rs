// sentiric-sip-b2bua-service/src/rabbitmq.rs
use anyhow::Result;
use futures::StreamExt;
use lapin::{
    options::*, types::FieldTable, BasicProperties, Channel, Connection, ConnectionProperties,
};
use prost::Message;
use sentiric_contracts::sentiric::event::v1::GenericEvent;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

pub struct RabbitMqClient {
    connection: Arc<RwLock<Option<Connection>>>,
    channel: Arc<RwLock<Option<Channel>>>,
}

impl RabbitMqClient {
    pub async fn new(url: &str) -> Self {
        let client = Self {
            connection: Arc::new(RwLock::new(None)),
            channel: Arc::new(RwLock::new(None)),
        };

        let conn_clone = client.connection.clone();
        let chan_clone = client.channel.clone();
        let url_clone = url.to_string();

        tokio::spawn(async move {
            let mut attempt = 0;
            loop {
                attempt += 1;
                if attempt == 1 {
                    info!(
                        event = "RABBITMQ_CONNECTING",
                        "🐇 RabbitMQ'ya bağlanılıyor..."
                    );
                }

                match Connection::connect(&url_clone, ConnectionProperties::default()).await {
                    Ok(conn) => match conn.create_channel().await {
                        Ok(channel) => {
                            if let Err(e) = channel
                                .confirm_select(ConfirmSelectOptions::default())
                                .await
                            {
                                error!(event = "RABBITMQ_CONFIRM_ERROR", error = %e, "🚨 RabbitMQ Confirm Mode Error");
                            }
                            conn.on_error(|err| {
                                error!(event = "RABBITMQ_CONN_ERROR", error = %err, "🚨 RabbitMQ Connection Error")
                            });

                            *conn_clone.write().await = Some(conn);
                            *chan_clone.write().await = Some(channel);

                            info!(
                                event = "RABBITMQ_RECOVERED",
                                "✅ [MQ] RabbitMQ bağlantısı ve channel sağlandı."
                            );
                            break;
                        }
                        Err(e) => {
                            error!(event = "RABBITMQ_CHANNEL_FAIL", error = %e, "❌ RabbitMQ kanalı oluşturulamadı.")
                        }
                    },
                    Err(e) => {
                        if attempt == 1 {
                            warn!(event = "RABBITMQ_UNREACHABLE", error = %e, "⚠️ RabbitMQ yok. Servis Ghost Publisher modunda çalışacak.");
                        } else {
                            tracing::debug!(
                                event = "RABBITMQ_RETRY",
                                attempt = attempt,
                                "RabbitMQ bekleniyor..."
                            );
                        }
                    }
                }
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            }
        });

        client
    }

    async fn ensure_healthy_channel(&self) -> Result<()> {
        let channel_guard = self.channel.read().await;
        if let Some(ch) = channel_guard.as_ref() {
            if ch.status().connected() {
                return Ok(());
            }
        }
        drop(channel_guard);

        warn!(
            event = "MQ_CHANNEL_DROPPED",
            "⚠️ [MQ] Channel koptu, yeniden oluşturuluyor..."
        );

        let conn_guard = self.connection.read().await;
        if let Some(conn) = conn_guard.as_ref() {
            if conn.status().connected() {
                let new_channel = conn.create_channel().await?;
                drop(conn_guard);
                *self.channel.write().await = Some(new_channel);
                info!(
                    event = "MQ_CHANNEL_RESTORED",
                    "✅ [MQ] Channel yeniden oluşturuldu."
                );
                return Ok(());
            }
        }

        anyhow::bail!("RabbitMQ Connection is dead.");
    }

    pub async fn publish_event_bytes(&self, routing_key: &str, payload: &[u8]) -> Result<()> {
        if self.channel.read().await.is_none() {
            tracing::debug!(event="GHOST_PUBLISH", routing_key=%routing_key, "RabbitMQ yok, mesaj RAM'de yutuldu (Ghost Mode).");
            return Ok(());
        }

        const MAX_RETRIES: u32 = 3;
        for _attempt in 0..MAX_RETRIES {
            if let Err(e) = self.ensure_healthy_channel().await {
                error!(event="MQ_CHANNEL_RECOVERY_FAILED", error=%e, "❌ [MQ] Channel sağlıklı hale getirilemedi");
                tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
                continue;
            }

            let channel_guard = self.channel.read().await;
            if let Some(channel) = channel_guard.as_ref() {
                match channel
                    .basic_publish(
                        "sentiric_events",
                        routing_key,
                        BasicPublishOptions::default(),
                        payload,
                        BasicProperties::default().with_delivery_mode(2),
                    )
                    .await
                {
                    Ok(_) => {
                        debug!(event="MQ_EVENT_PUBLISHED", routing_key=%routing_key, "📨 [MQ] Event yayınlandı");
                        return Ok(());
                    }
                    Err(e) => {
                        error!(event="MQ_PUBLISH_FAILED", error=%e, "❌ [MQ] Publish başarısız");
                        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                    }
                }
            }
        }
        anyhow::bail!("RabbitMQ publish başarısız")
    }

    pub async fn start_termination_consumer(&self, engine: Arc<crate::sip::engine::B2BuaEngine>) {
        let conn_arc = self.connection.clone();

        tokio::spawn(async move {
            loop {
                let conn_guard = conn_arc.read().await;
                if let Some(conn) = conn_guard.as_ref() {
                    if conn.status().connected() {
                        if let Ok(ch) = conn.create_channel().await {
                            let consumer_channel = Arc::new(ch);
                            drop(conn_guard);

                            let queue_name = "sentiric.b2bua_service.commands";
                            let _ = consumer_channel
                                .queue_declare(
                                    queue_name,
                                    QueueDeclareOptions {
                                        durable: true,
                                        ..Default::default()
                                    },
                                    FieldTable::default(),
                                )
                                .await;
                            let _ = consumer_channel
                                .queue_bind(
                                    queue_name,
                                    "sentiric_events",
                                    "call.terminate.request",
                                    QueueBindOptions::default(),
                                    FieldTable::default(),
                                )
                                .await;

                            if let Ok(mut consumer) = consumer_channel
                                .basic_consume(
                                    queue_name,
                                    "b2bua_term_consumer",
                                    BasicConsumeOptions::default(),
                                    FieldTable::default(),
                                )
                                .await
                            {
                                info!("👂 [MQ] B2BUA Termination Consumer dinlemeye başladı.");
                                while let Some(delivery) = consumer.next().await {
                                    if let Ok(delivery) = delivery {
                                        if let Ok(generic_event) =
                                            GenericEvent::decode(&delivery.data[..])
                                        {
                                            if let Ok(json) =
                                                serde_json::from_str::<serde_json::Value>(
                                                    &generic_event.payload_json,
                                                )
                                            {
                                                if let Some(call_id) = json["callId"].as_str() {
                                                    engine.terminate_session(call_id).await;
                                                }
                                            }
                                        }
                                        let _ = delivery.ack(BasicAckOptions::default()).await;
                                    }
                                }
                            }
                            break;
                        }
                    }
                }
                drop(conn_guard);
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            }
        });
    }
}
