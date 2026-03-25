// Dosya: src/rabbitmq.rs

use anyhow::Result;
use lapin::{
    options::*, types::FieldTable, BasicProperties,
    Channel, Connection, ConnectionProperties,
};
use futures::StreamExt;
use tracing::{info, debug, warn, error};
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct RabbitMqClient {
    // [ARCH-COMPLIANCE] constraints.yaml message_broker_reconnect kuralı:
    // Channel drop senaryosunda yeniden oluşturulabilmesi için
    // connection ve channel ayrı tutulur, channel Mutex ile korunur.
    connection: Arc<Mutex<Connection>>,
    channel: Arc<Mutex<Channel>>,
    url: String,
}

impl RabbitMqClient {
    pub async fn new(url: &str) -> Result<Self> {
        let connection = Self::connect_with_retry(url).await?;
        let channel = connection.create_channel().await?;
        info!("✅ [MQ] RabbitMQ bağlantısı ve channel sağlandı.");

        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            channel: Arc::new(Mutex::new(channel)),
            url: url.to_string(),
        })
    }

    /// Connection seviyesinde retry (10 deneme, 5s aralık)
    async fn connect_with_retry(url: &str) -> Result<Connection> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            match Connection::connect(url, ConnectionProperties::default()).await {
                Ok(conn) => return Ok(conn),
                Err(e) => {
                    if attempt >= 10 {
                        anyhow::bail!("RabbitMQ'ya bağlanılamadı (10 deneme): {}", e);
                    }
                    warn!("RabbitMQ bağlantısı bekleniyor... ({}/10): {}", attempt, e);
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                }
            }
        }
    }

    // [ARCH-COMPLIANCE] constraints.yaml message_broker_reconnect kuralı:
    // Channel drop tespit edildiğinde önce channel yeniden oluşturulur,
    // connection da ölmüşse önce connection yenilenir.
    async fn ensure_healthy_channel(&self) -> Result<()> {
        let channel = self.channel.lock().await;

        if channel.status().connected() {
            return Ok(());
        }

        drop(channel); // lock'u bırak, yeniden oluşturacağız

        warn!("⚠️ [MQ] Channel koptu, yeniden oluşturuluyor...");

        let conn = self.connection.lock().await;
        let new_channel = if conn.status().connected() {
            // Connection sağlıklı, sadece channel yenile
            conn.create_channel().await?
        } else {
            // Connection da ölmüş — her ikisini de yenile
            drop(conn);
            warn!("⚠️ [MQ] Connection da koptu, yeniden bağlanılıyor...");
            let new_conn = Self::connect_with_retry(&self.url).await?;
            let ch = new_conn.create_channel().await?;
            *self.connection.lock().await = new_conn;
            ch
        };

        *self.channel.lock().await = new_channel;
        info!("✅ [MQ] Channel yeniden oluşturuldu.");
        Ok(())
    }

    pub async fn publish_event_bytes(&self, routing_key: &str, payload: &[u8]) -> Result<()> {
        const MAX_RETRIES: u32 = 3;

        for attempt in 0..MAX_RETRIES {
            // [ARCH-COMPLIANCE] Her publish öncesi channel sağlığı kontrol edilir
            if let Err(e) = self.ensure_healthy_channel().await {
                error!("❌ [MQ] Channel sağlıklı hale getirilemedi (deneme {}/{}): {}", attempt + 1, MAX_RETRIES, e);
                tokio::time::sleep(tokio::time::Duration::from_millis(200 * (attempt + 1) as u64)).await;
                continue;
            }

            let channel = self.channel.lock().await;
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
                    if attempt > 0 {
                        info!("✅ [MQ] Event {} deneme sonrası yayınlandı: {}", attempt, routing_key);
                    } else {
                        debug!("📨 [MQ] Event yayınlandı: {} ({} bytes)", routing_key, payload.len());
                    }
                    return Ok(());
                }
                Err(e) => {
                    error!("❌ [MQ] Publish başarısız ({}/{}): {}", attempt + 1, MAX_RETRIES, e);
                    if attempt < MAX_RETRIES - 1 {
                        tokio::time::sleep(
                            tokio::time::Duration::from_millis(100 * (attempt + 1) as u64),
                        )
                        .await;
                    }
                }
            }
        }

        anyhow::bail!("RabbitMQ publish {} denemeden sonra başarısız oldu", MAX_RETRIES)
    }

    pub async fn start_termination_consumer(
        &self,
        engine: Arc<crate::sip::engine::B2BuaEngine>,
    ) {
        // [ARCH-COMPLIANCE] Consumer için ayrı channel — publish channel'ını bloklamaz
        let conn = self.connection.lock().await;
        let consumer_channel = match conn.create_channel().await {
            Ok(ch) => Arc::new(ch),
            Err(e) => {
                error!("❌ [MQ] Consumer channel oluşturulamadı: {}", e);
                return;
            }
        };
        drop(conn);

        tokio::spawn(async move {
            let queue_name = "sentiric.b2bua_service.commands";
            let _ = consumer_channel
                .queue_declare(
                    queue_name,
                    QueueDeclareOptions { durable: true, ..Default::default() },
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
                        if let Ok(json) =
                            serde_json::from_slice::<serde_json::Value>(&delivery.data)
                        {
                            if let Some(call_id) = json["callId"].as_str() {
                                info!(
                                    event = "TERMINATION_REQUEST",
                                    call_id = %call_id,
                                    "🛑 [MQ] Termination isteği alındı."
                                );
                                engine.terminate_session(call_id).await;
                            }
                        }
                        let _ = delivery.ack(BasicAckOptions::default()).await;
                    }
                }
            }
        });
    }
}