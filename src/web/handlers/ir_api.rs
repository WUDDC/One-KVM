use super::*;
use std::sync::Arc;

use crate::ir::store::{self, IrRemoteRecord};

#[derive(Serialize)]
pub struct IrRemotesResponse {
    pub remotes: Vec<IrRemoteRecord>,
}

#[derive(Serialize)]
pub struct IrCreateRemoteResponse {
    pub id: i64,
    pub name: String,
}

#[derive(Deserialize)]
pub struct IrCreateRemoteRequest {
    pub name: String,
}

#[derive(Deserialize)]
pub struct IrRenameRemoteRequest {
    pub name: String,
}

#[derive(Deserialize)]
pub struct IrLearnRequest {
    pub remote_id: i64,
    pub name: String,
}

#[derive(Deserialize)]
pub struct IrButtonUpdateRequest {
    pub name: Option<String>,
    /// `null` unbinds the button from its quick slot.
    pub slot: Option<Option<i64>>,
}

#[derive(Serialize)]
pub struct IrImportResult {
    pub remotes_imported: u32,
    pub remotes_merged: u32,
    pub buttons_imported: u32,
    pub buttons_skipped: u32,
}

/// Shared remote pack: `{ format: "one-kvm-ir-pack", version: 1, remotes: [...] }`
#[derive(Debug, Deserialize, Serialize)]
pub struct IrRemotePack {
    pub format: String,
    pub version: u32,
    pub remotes: Vec<IrPackRemote>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct IrPackRemote {
    pub name: String,
    pub buttons: Vec<IrPackButton>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct IrPackButton {
    pub name: String,
    pub protocol: String,
    #[serde(default)]
    pub scancode: Option<i64>,
    #[serde(default)]
    pub raw: Option<Vec<u32>>,
    #[serde(default)]
    pub carrier: Option<i64>,
}

const IR_PACK_FORMAT: &str = "one-kvm-ir-pack";

pub async fn ir_remotes(State(state): State<Arc<AppState>>) -> Result<Json<IrRemotesResponse>> {
    let remotes = store::list_remotes(state.db.pool()).await?;
    Ok(Json(IrRemotesResponse { remotes }))
}

pub async fn ir_create_remote(
    State(state): State<Arc<AppState>>,
    Json(req): Json<IrCreateRemoteRequest>,
) -> Result<Json<IrCreateRemoteResponse>> {
    let name = req.name.trim().to_string();
    let id = store::create_remote(state.db.pool(), &name).await?;
    Ok(Json(IrCreateRemoteResponse { id, name }))
}

pub async fn ir_rename_remote(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<i64>,
    Json(req): Json<IrRenameRemoteRequest>,
) -> Result<Json<LoginResponse>> {
    store::rename_remote(state.db.pool(), id, &req.name).await?;
    Ok(Json(LoginResponse {
        success: true,
        message: Some("remote renamed".to_string()),
    }))
}

pub async fn ir_delete_remote(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Result<Json<LoginResponse>> {
    store::delete_remote(state.db.pool(), id).await?;
    Ok(Json(LoginResponse {
        success: true,
        message: Some("remote deleted".to_string()),
    }))
}

/// Mark this remote as the single active KVM-switch remote.
pub async fn ir_set_kvm_remote(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Result<Json<LoginResponse>> {
    store::set_kvm_remote(state.db.pool(), id).await?;
    Ok(Json(LoginResponse {
        success: true,
        message: Some("KVM switch remote updated".to_string()),
    }))
}

pub async fn ir_learn(
    State(state): State<Arc<AppState>>,
    Json(req): Json<IrLearnRequest>,
) -> Result<Json<LoginResponse>> {
    let name = req.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::BadRequest(
            "button name must not be empty".to_string(),
        ));
    }
    let config = state.config.get();
    if !config.ir.enabled {
        return Err(AppError::BadRequest("IR remote is disabled".to_string()));
    }
    drop(config);

    state.ir.start_learn(req.remote_id, name).await?;
    Ok(Json(LoginResponse {
        success: true,
        message: Some("learning started".to_string()),
    }))
}

pub async fn ir_learn_cancel(State(state): State<Arc<AppState>>) -> Result<Json<LoginResponse>> {
    state.ir.cancel_learn().await?;
    Ok(Json(LoginResponse {
        success: true,
        message: Some("learning cancelled".to_string()),
    }))
}

pub async fn ir_send(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Result<Json<LoginResponse>> {
    let config = state.config.get();
    if !config.ir.enabled {
        return Err(AppError::BadRequest("IR remote is disabled".to_string()));
    }
    drop(config);

    state.ir.send_button(id).await?;
    Ok(Json(LoginResponse {
        success: true,
        message: Some("IR signal sent".to_string()),
    }))
}

pub async fn ir_update_button(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<i64>,
    Json(req): Json<IrButtonUpdateRequest>,
) -> Result<Json<LoginResponse>> {
    if req.name.is_none() && req.slot.is_none() {
        return Err(AppError::BadRequest("nothing to update".to_string()));
    }
    if let Some(name) = req.name.as_deref() {
        if name.trim().is_empty() {
            return Err(AppError::BadRequest(
                "button name must not be empty".to_string(),
            ));
        }
    }
    store::update_button(state.db.pool(), id, req.name.as_deref(), req.slot).await?;
    Ok(Json(LoginResponse {
        success: true,
        message: Some("button updated".to_string()),
    }))
}

pub async fn ir_delete_button(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Result<Json<LoginResponse>> {
    store::delete_button(state.db.pool(), id).await?;
    Ok(Json(LoginResponse {
        success: true,
        message: Some("button deleted".to_string()),
    }))
}

pub async fn ir_import(
    State(state): State<Arc<AppState>>,
    Json(pack): Json<serde_json::Value>,
) -> Result<Json<IrImportResult>> {
    let pack: IrRemotePack = serde_json::from_value(pack)
        .map_err(|e| AppError::BadRequest(format!("invalid remote pack: {e}")))?;
    if pack.format != IR_PACK_FORMAT {
        return Err(AppError::BadRequest(format!(
            "unsupported pack format '{}' (expected '{IR_PACK_FORMAT}')",
            pack.format
        )));
    }
    if pack.version != 1 {
        return Err(AppError::BadRequest(format!(
            "unsupported pack version {}",
            pack.version
        )));
    }

    let pool = state.db.pool();
    let mut result = IrImportResult {
        remotes_imported: 0,
        remotes_merged: 0,
        buttons_imported: 0,
        buttons_skipped: 0,
    };

    for remote in pack.remotes {
        let remote_name = remote.name.trim();
        if remote_name.is_empty() {
            continue;
        }
        let remote_id = match store::get_remote_by_name(pool, remote_name).await? {
            Some(id) => {
                result.remotes_merged += 1;
                id
            }
            None => {
                result.remotes_imported += 1;
                store::create_remote(pool, remote_name).await?
            }
        };

        for button in remote.buttons {
            let button_name = button.name.trim();
            if button_name.is_empty() {
                continue;
            }
            if store::button_name_exists(pool, remote_id, button_name).await? {
                result.buttons_skipped += 1;
                continue;
            }
            let raw = button
                .raw
                .as_ref()
                .map(|r| serde_json::to_string(r).unwrap_or_default());
            store::insert_button(
                pool,
                remote_id,
                button_name,
                button.protocol.trim(),
                button.scancode,
                raw.as_deref(),
                button.carrier.unwrap_or(38000),
            )
            .await?;
            result.buttons_imported += 1;
        }
    }

    Ok(Json(result))
}

pub async fn ir_export_remote(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Result<axum::response::Response> {
    use axum::response::IntoResponse;

    let remotes = store::list_remotes(state.db.pool()).await?;
    let remote = remotes
        .into_iter()
        .find(|r| r.id == id)
        .ok_or_else(|| AppError::NotFound(format!("IR remote {id} not found")))?;

    let pool = state.db.pool();
    let mut pack_buttons = Vec::with_capacity(remote.buttons.len());
    for button in &remote.buttons {
        let raw = store::get_button_raw(pool, button.id).await?;
        pack_buttons.push(IrPackButton {
            name: button.name.clone(),
            protocol: button.proto.clone(),
            scancode: button.scancode,
            raw: raw.and_then(|r| serde_json::from_str::<Vec<u32>>(&r).ok()),
            carrier: Some(button.carrier),
        });
    }

    let pack = IrRemotePack {
        format: IR_PACK_FORMAT.to_string(),
        version: 1,
        remotes: vec![IrPackRemote {
            name: remote.name.clone(),
            buttons: pack_buttons,
        }],
    };

    let body = serde_json::to_vec_pretty(&pack)
        .map_err(|e| AppError::Internal(format!("serialize pack failed: {e}")))?;

    let safe_name: String = remote
        .name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let filename = format!("{safe_name}.onekvm-ir.json");

    Ok((
        [
            (
                axum::http::header::CONTENT_TYPE,
                "application/json".to_string(),
            ),
            (
                axum::http::header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        body,
    )
        .into_response())
}

pub async fn ir_hardware(
    State(state): State<Arc<AppState>>,
) -> Result<Json<crate::ir::IrHardwareStatus>> {
    let status = state.ir.hardware_status().await;
    Ok(Json(status))
}
