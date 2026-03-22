# 🔄 Sentiric Sip B2BUA Service

[![Status](https://img.shields.io/badge/status-active-success.svg)]()
[![Language](https://img.shields.io/badge/language-Rust-orange.svg)]()
[![Layer](https://img.shields.io/badge/layer-Telecom_Core-blueviolet.svg)]()

**Sentiric B2BUA (Back-to-Back User Agent) Service**, platformun omurgasını oluşturan SIP oturum yöneticisidir. Sentiric'in "Soft-Defined Telecom" (Yazılım Tanımlı Telekom) vizyonunun kalbidir. 

Kendi içinde hiçbir iş mantığı (AI, Echo, Kayıt) barındırmaz. Sadece ağ bağlantılarını kurar ve platformu olaylardan (Events) haberdar eder.

## 🎯 Temel Sorumluluklar

1.  **Aptal Boru (Dumb Pipe):** Harici dünyadan gelen çağrıları (Inbound) ve platformun başlattığı çağrıları (Outbound) standart SIP RFC'lerine göre yönetir.
2.  **Medya Temini:** Her çağrı için `media-service` üzerinden izole bir RTP portu tahsis eder ve çağrı bitiminde bu portu iade eder.
3.  **Olay Üreticisi (Event Producer):** Sistemdeki her kritik aşamayı (`call.started`, `call.answered`, `call.ended`) RabbitMQ üzerinden yayınlar. Böylece `Workflow` ve `CDR` gibi servisler ne yapacaklarına karar verirler.

## 🛠️ Teknoloji Yığını

*   **Dil:** Rust (Yüksek performanslı telkom protokol işleme için)
*   **Servisler Arası İletişim:** gRPC (Tonic, mTLS destekli)
*   **Olay Yolu:** RabbitMQ (`lapin` kütüphanesi)
*   **Durum:** Redis (Çağrı durumlarını saklamak için)

## 🔌 API ve Olay Etkileşimleri

*   **Gelen Ağ (SIP):** `sbc-service`'ten gelen UDP sinyalleri.
*   **Gelen (gRPC):** `agent-service` veya `workflow-service`'ten gelen `InitiateCall` (Dış Arama) emirleri.
*   **Giden Olaylar (RabbitMQ):**
    *   `call.started`: Çağrı 200 OK aldığında fırlatılır.
    *   `call.answered`: Çağrı ACK aldığında (faturalama için) fırlatılır.
    *   `call.ended`: Çağrı BYE/CANCEL ile kapandığında fırlatılır.

---
## 🏛️ Anayasal Konum

Bu servis,[Sentiric Anayasası'nın](https://github.com/sentiric/sentiric-governance) **Telecom Core Layer**'ında yer alır. **Kesin kural:** Bu repoya hiçbir zaman spesifik bir ürünün (Oyun, IVR, AI) iş mantığı kodlanamaz.

---
