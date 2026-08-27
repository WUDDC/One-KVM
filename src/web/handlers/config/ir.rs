use axum::{extract::State, Json};
use std::sync::Arc;

use crate::config::IrConfig;
use crate::error::Result;
use crate::state::AppState;

use super::apply::apply_ir_config;
use super::types::IrConfigUpdate;

pub async fn get_ir_config(State(state): State<Arc<AppState>>) -> Json<IrConfig> {
    Json(state.config.get().ir.clone())
}

pub async fn update_ir_config(
    State(state): State<Arc<AppState>>,
    Json(req): Json<IrConfigUpdate>,
) -> Result<Json<IrConfig>> {
    let current_config = state.config.get();
    let old_ir_config = current_config.ir.clone();

    req.validate_with_current(&old_ir_config)?;

    let _apply_guard = super::apply::try_apply_lock(&state.config_apply_locks.ir, "ir")?;
    state
        .config
        .update(|config| {
            req.apply_to(&mut config.ir);
            config.ir.normalize();
        })
        .await?;

    let new_ir_config = state.config.get().ir.clone();
    apply_ir_config(&state, &new_ir_config).await?;

    Ok(Json(new_ir_config))
}
