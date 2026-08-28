//! Infrared remote learning and transmission.
//!
//! Learning reads decoded `(protocol, scancode)` pairs from an rc-core LIRC
//! device (`/dev/lirc*` in `LIRC_MODE_SCANCODE`, typically backed by meson-ir
//! on the IR_DEC hardware decoder). Transmission prefers a kernel TX-capable
//! LIRC device (e.g. gpio-ir-tx, which encodes every protocol rc-core knows)
//! and falls back to userspace NEC encoding with 38 kHz carrier bit-banging
//! on a raw GPIO line.

mod encoder;
mod led;
mod rx;
pub(crate) mod store;
mod tx;

pub use store::{IrButtonRecord, IrRemoteRecord};

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use crate::config::IrConfig;
use crate::error::{AppError, Result};
use crate::events::{EventBus, SystemEvent};

pub use led::{LedColor, LedPattern};

/// Decoded keypress captured from the receiver.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LearnedCode {
    pub proto: String,
    pub scancode: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct IrHardwareStatus {
    pub rx_available: bool,
    pub rx_device: Option<String>,
    pub tx_available: bool,
    pub tx_mode: String,
    pub tx_device: Option<String>,
    pub led_ready: bool,
    pub learn_active: bool,
}

#[derive(Clone)]
pub struct IrManager {
    config: IrConfig,
    db: crate::db::DatabasePool,
    events: Arc<EventBus>,
    led: Arc<led::Ws2812Led>,
    learn_lock: Arc<Mutex<()>>,
    session: Arc<Mutex<Option<LearnSessionHandle>>>,
}

struct LearnSessionHandle {
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl IrManager {
    pub fn new(config: IrConfig, db: crate::db::DatabasePool, events: Arc<EventBus>) -> Self {
        let led = Arc::new(led::Ws2812Led::new(&config));
        Self {
            config,
            db,
            events,
            led,
            learn_lock: Arc::new(Mutex::new(())),
            session: Arc::new(Mutex::new(None)),
        }
    }

    /// Re-apply hardware settings after a config change.
    pub fn apply_config(&self, config: &IrConfig) {
        self.led.apply_config(config);
    }

    // ---------------------------------------------------------------- learn

    pub async fn learn_active(&self) -> bool {
        self.session.lock().await.is_some()
    }

    pub async fn start_learn(&self, remote_id: i64, button_name: String) -> Result<()> {
        let _guard = self.learn_lock.try_lock().map_err(|_| {
            AppError::Conflict("another learn session is already running".to_string())
        })?;

        if self.session.lock().await.is_some() {
            return Err(AppError::Conflict(
                "a learn session is already running".to_string(),
            ));
        }

        store::ensure_remote_exists(self.db.pool(), remote_id).await?;

        let rx = rx::LircReceiver::open(self.config.rx_device.as_str())
            .map_err(|e| {
                self.led.set(LedPattern::solid(LedColor::RED, 2000));
                self.publish_learn("failed", Some(remote_id), None, None, None, None);
                e
            })?;
        drop(rx);

        let rx_device = self.config.rx_device.clone();
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        *self.session.lock().await = Some(LearnSessionHandle {
            cancelled: cancelled.clone(),
        });

        let manager = self.clone();
        let timeout = Duration::from_millis(self.config.learn_timeout_ms.max(1000));
        info!("IR learn session started for remote {remote_id} ({button_name})");
        tokio::spawn(async move {
            let result = rx::capture(rx_device, cancelled, timeout).await;
            manager.finish_learn(remote_id, button_name, result).await;
        });

        self.led.set(LedPattern::blink(LedColor::GREEN));
        self.publish_learn("waiting", Some(remote_id), None, None, None, None);
        Ok(())
    }

    pub async fn cancel_learn(&self) -> Result<()> {
        let handle = self.session.lock().await.take();
        match handle {
            Some(handle) => {
                handle
                    .cancelled
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                self.led.set(LedPattern::Off);
                self.publish_learn("cancelled", None, None, None, None, None);
                info!("IR learn session cancelled");
                Ok(())
            }
            None => Err(AppError::BadRequest("no learn session running".to_string())),
        }
    }

    async fn finish_learn(
        &self,
        remote_id: i64,
        button_name: String,
        result: Result<LearnedCode>,
    ) {
        let _ = self.session.lock().await.take();

        let code = match result {
            Ok(code) => code,
            Err(AppError::Conflict(_)) => {
                // Cancelled by the user; LED already handled.
                return;
            }
            Err(e) => {
                warn!("IR learn failed: {}", e);
                self.led.set(LedPattern::solid(LedColor::RED, 2000));
                self.publish_learn("failed", Some(remote_id), None, None, None, None);
                return;
            }
        };

        match store::insert_button(
            self.db.pool(),
            remote_id,
            &button_name,
            &code.proto,
            Some(code.scancode as i64),
            None,
            self.config.carrier as i64,
        )
        .await
        {
            Ok(button_id) => {
                info!(
                    "IR code saved: button {button_id} ({button_name}) {} 0x{:x}",
                    code.proto, code.scancode
                );
                self.led.set(LedPattern::solid(LedColor::BLUE, 2000));
                self.publish_learn(
                    "saved",
                    Some(remote_id),
                    Some(button_id),
                    Some(code.proto),
                    Some(code.scancode),
                    None,
                );
            }
            Err(e) => {
                error!("Failed to save IR code: {}", e);
                self.led.set(LedPattern::solid(LedColor::RED, 2000));
                self.publish_learn("failed", Some(remote_id), None, None, None, None);
            }
        }
    }

    // -------------------------------------------------------------- transmit

    pub async fn send_button(&self, button_id: i64) -> Result<()> {
        let button = store::get_button(self.db.pool(), button_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("IR button {button_id} not found")))?;

        let raw = store::get_button_raw(self.db.pool(), button_id)
            .await?
            .map(|s| serde_json::from_str::<Vec<u32>>(&s))
            .transpose()
            .map_err(|e| AppError::Internal(format!("corrupt raw IR data: {e}")))?;

        // Bit-bang transmission blocks for the whole frame (~100 ms), so run
        // it on a blocking thread.
        let config = self.config.clone();
        let proto = button.proto.clone();
        let scancode = button.scancode;
        let carrier = i64::from(self.config.carrier);
        let result = tokio::task::spawn_blocking(move || {
            let mut tx = tx::Transmitter::open(&config)?;
            tx.transmit(&proto, scancode, raw.as_deref(), carrier)
        })
        .await
        .map_err(|e| AppError::Internal(format!("IR send task failed: {e}")))?;

        match result {
            Ok(()) => {
                self.led.set(LedPattern::solid(LedColor::BLUE, 400));
                self.publish_learn(
                    "sent",
                    Some(button.remote_id),
                    Some(button.id),
                    Some(button.proto.clone()),
                    button.scancode.map(|s| s as u64),
                    None,
                );
                Ok(())
            }
            Err(e) => {
                self.led.set(LedPattern::solid(LedColor::RED, 800));
                Err(e)
            }
        }
    }

    // ------------------------------------------------------------- hardware

    pub async fn hardware_status(&self) -> IrHardwareStatus {
        let rx = rx::LircReceiver::probe(self.config.rx_device.as_str());
        let tx = tx::Transmitter::probe(&self.config);
        IrHardwareStatus {
            rx_available: rx.is_some(),
            rx_device: rx,
            tx_available: tx.is_some(),
            tx_mode: tx
                .as_ref()
                .map(|(mode, _)| mode.clone())
                .unwrap_or_else(|| "none".to_string()),
            tx_device: tx.and_then(|(_, dev)| dev),
            led_ready: self.led.is_ready(),
            learn_active: self.learn_active().await,
        }
    }

    // ---------------------------------------------------------------- events

    #[allow(clippy::too_many_arguments)]
    fn publish_learn(
        &self,
        state: &str,
        remote_id: Option<i64>,
        button_id: Option<i64>,
        proto: Option<String>,
        scancode: Option<u64>,
        message: Option<String>,
    ) {
        let _ = self.events.publish(SystemEvent::IrLearn {
            state: state.to_string(),
            message,
            remote_id,
            button_id,
            proto,
            scancode,
        });
    }
}
