// sentiric-b2bua-service/src/main.rs
use anyhow::{Context, Result};
use sentiric_sip_b2bua_service::app::App;
use std::process;
use std::io::Write; // [ARCH-COMPLIANCE] SUTS uyumlu raw çıktı için

fn main() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("Tokio runtime oluşturulamadı")?;

    runtime.block_on(async {
        match App::bootstrap().await {
            Ok(app) => {
                if let Err(e) = app.run().await {
                    tracing::error!(event="APP_RUN_ERROR", error=%e, "Uygulama çalışırken hata oluştu.");
                    process::exit(1);
                }
            },
            Err(e) => {
                // [ARCH-COMPLIANCE] ARCH-005: eprintln! kullanımı yasaktır. 
                // Tracing başlatılamadığında bile SUTS formatında stdout/stderr basılmalıdır.
                let _ = writeln!(
                    std::io::stderr(), 
                    "{{\"schema_v\":\"1.0.0\",\"severity\":\"FATAL\",\"event\":\"BOOTSTRAP_FAILED\",\"message\":\"{}\"}}", 
                    e
                );
                process::exit(1);
            }
        }
    });
    Ok(())
}