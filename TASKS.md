# 🔄 Sentiric B2BUA Service - Görev Listesi

Bu servisin mevcut ve gelecekteki tüm geliştirme görevleri, platformun merkezi görev yönetimi reposu olan **`sentiric-tasks`**'ta yönetilmektedir.

➡️ **[Aktif Görev Panosuna Git](https://github.com/sentiric/sentiric-tasks/blob/main/TASKS.md)**

---

## ✅ Mimari Uyum Düzeltmeleri (Arch-Compliance)

- **[2026-03-25]** `refactor(core)`: mTLS graceful degradation kaldırıldı — sertifika yüklenemezse
  servis artık `bail!` ile çıkıyor. `constraints.yaml` zero-trust kuralı gereği.
- **[2026-03-25]** `refactor(telemetry)`: `tenant_id` hardcoded `"sentiric_demo"` kaldırıldı —
  `TENANT_ID` env var'dan zorunlu olarak okunuyor. Multi-tenant uyumu sağlandı.
- **[TODO]** `span_id` propagation: SUTS v4.0 `span_id` alanı şu an her zaman `None`.
  OpenTelemetry context'ten span_id extraction sonraki iterasyona bırakıldı.
- **[TODO]** RabbitMQ channel-level reconnect: Connection retry var ama channel drop
  senaryosu handle edilmiyor.