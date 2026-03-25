# 🔄 Sentiric B2BUA Service - Görev Listesi

Bu servisin mevcut ve gelecekteki tüm geliştirme görevleri, platformun merkezi görev yönetimi reposu olan **`sentiric-tasks`**'ta yönetilmektedir.

➡️ **[Aktif Görev Panosuna Git](https://github.com/sentiric/sentiric-tasks/blob/main/TASKS.md)**

---

## ✅ Mimari Uyum Düzeltmeleri (Arch-Compliance)

- **[2026-03-25]** `refactor(core)`: mTLS graceful degradation kaldırıldı.
- **[2026-03-25]** `refactor(telemetry)`: tenant_id hardcoded kaldırıldı, TENANT_ID env var'dan okunuyor.
- **[2026-03-25]** `refactor(telemetry)`: span_id artık aktif tracing span'den doldurulur,
  None bırakmak constraints.yaml span_id kuralı gereği yasaktır.
- **[2026-03-25]** `refactor(rabbitmq)`: Channel-level reconnect eklendi. Connection ve channel
  ayrı Mutex'lerde tutulur. ensure_healthy_channel() her publish öncesi çağrılır.
  Consumer için ayrı channel oluşturulur (publish channel'ını bloklamaz).