//! Procesos de fondo. Pueden correr en todas las instancias a la vez.

use std::time::Duration;

use remates_store::Store;
use tokio_util::sync::CancellationToken;

/// Cierra subastas vencidas cada `every` (RF-13). Varias instancias pueden correrlo
/// en paralelo: `close_due` reparte las subastas con `SKIP LOCKED`.
///
/// La exactitud no depende de este intervalo: desde la hora de cierre el motor ya
/// rechaza pujas; el proceso sólo registra el resultado y emite el certificado.
pub async fn run_closer(store: Store, every: Duration, shutdown: CancellationToken) {
    let mut tick = tokio::time::interval(every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = tick.tick() => {}
        }
        match store.close_due(100).await {
            Ok(closed) => {
                for c in closed {
                    tracing::info!(
                        auction = %c.auction_id,
                        outcome = ?c.outcome,
                        certificate = ?c.certificate_number,
                        "subasta cerrada"
                    );
                }
            }
            Err(e) => tracing::warn!(error = %e, "closer: error cerrando subastas"),
        }
    }
}
