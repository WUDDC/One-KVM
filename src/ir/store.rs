//! SQLite persistence for the IR code library.

use sqlx::{Pool, Row, Sqlite};
use std::collections::BTreeMap;

use crate::error::{AppError, Result};

#[derive(Debug, Clone, serde::Serialize)]
pub struct IrButtonRecord {
    pub id: i64,
    pub remote_id: i64,
    pub name: String,
    pub proto: String,
    pub scancode: Option<i64>,
    pub has_raw: bool,
    pub carrier: i64,
    pub slot: Option<i64>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct IrRemoteRecord {
    pub id: i64,
    pub name: String,
    pub is_kvm: bool,
    pub buttons: Vec<IrButtonRecord>,
}

pub async fn ensure_schema(pool: &Pool<Sqlite>) -> Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS ir_remotes (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL UNIQUE,
            is_kvm INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL
        )
        "#,
    )
    .execute(pool)
    .await?;

    // Migration for tables created before the is_kvm column existed.
    if let Err(e) = sqlx::query(
        "ALTER TABLE ir_remotes ADD COLUMN is_kvm INTEGER NOT NULL DEFAULT 0",
    )
    .execute(pool)
    .await
    {
        let msg = e.to_string();
        if !msg.contains("duplicate column name") {
            return Err(AppError::Persistence(msg));
        }
    }

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS ir_buttons (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            remote_id INTEGER NOT NULL,
            name TEXT NOT NULL,
            proto TEXT NOT NULL,
            scancode INTEGER,
            raw TEXT,
            carrier INTEGER NOT NULL DEFAULT 38000,
            slot INTEGER,
            created_at INTEGER NOT NULL,
            FOREIGN KEY (remote_id) REFERENCES ir_remotes(id) ON DELETE CASCADE
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS idx_ir_buttons_remote
        ON ir_buttons(remote_id)
        "#,
    )
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn ensure_remote_exists(pool: &Pool<Sqlite>, remote_id: i64) -> Result<()> {
    let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM ir_remotes WHERE id = ?")
        .bind(remote_id)
        .fetch_optional(pool)
        .await?;
    if exists.is_none() {
        return Err(AppError::NotFound(format!("IR remote {remote_id} not found")));
    }
    Ok(())
}

pub async fn list_remotes(pool: &Pool<Sqlite>) -> Result<Vec<IrRemoteRecord>> {
    let remote_rows = sqlx::query("SELECT id, name, is_kvm FROM ir_remotes ORDER BY id")
        .fetch_all(pool)
        .await?;

    let button_rows = sqlx::query(
        "SELECT id, remote_id, name, proto, scancode, raw IS NOT NULL AS has_raw, carrier, slot \
         FROM ir_buttons ORDER BY id",
    )
    .fetch_all(pool)
    .await?;

    let mut buttons_by_remote: BTreeMap<i64, Vec<IrButtonRecord>> = BTreeMap::new();
    for row in button_rows {
        let remote_id: i64 = row.try_get("remote_id")?;
        buttons_by_remote.entry(remote_id).or_default().push(IrButtonRecord {
            id: row.try_get("id")?,
            remote_id,
            name: row.try_get("name")?,
            proto: row.try_get("proto")?,
            scancode: row.try_get("scancode")?,
            has_raw: row.try_get::<i64, _>("has_raw")? != 0,
            carrier: row.try_get("carrier")?,
            slot: row.try_get("slot")?,
        });
    }

    let remotes = remote_rows
        .into_iter()
        .map(|row| {
            let id: i64 = row.try_get("id")?;
            Ok(IrRemoteRecord {
                id,
                name: row.try_get("name")?,
                is_kvm: row.try_get::<i64, _>("is_kvm")? != 0,
                buttons: buttons_by_remote.remove(&id).unwrap_or_default(),
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(remotes)
}

/// Mark exactly one remote as the KVM-switch remote; clears the flag on all
/// others so only one remote can be active at a time. Slot bindings of other
/// remotes are kept — they simply stop driving the KVM-switch popover until
/// their remote is made active again.
pub async fn set_kvm_remote(pool: &Pool<Sqlite>, id: i64) -> Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE ir_remotes SET is_kvm = 0 WHERE is_kvm = 1")
        .execute(&mut *tx)
        .await?;
    let result = sqlx::query("UPDATE ir_remotes SET is_kvm = 1 WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    if result.rows_affected() == 0 {
        tx.rollback().await.ok();
        return Err(AppError::NotFound(format!("IR remote {id} not found")));
    }
    tx.commit().await?;
    Ok(())
}

pub async fn create_remote(pool: &Pool<Sqlite>, name: &str) -> Result<i64> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::BadRequest("remote name must not be empty".to_string()));
    }
    let result = sqlx::query("INSERT INTO ir_remotes (name, created_at) VALUES (?, ?)")
        .bind(name)
        .bind(now())
        .execute(pool)
        .await
        .map_err(|e| match e.as_database_error().map(|d| d.is_unique_violation()) {
            Some(true) => AppError::Conflict(format!("remote '{name}' already exists")),
            _ => AppError::Persistence(e.to_string()),
        })?;
    Ok(result.last_insert_rowid())
}

pub async fn rename_remote(pool: &Pool<Sqlite>, id: i64, name: &str) -> Result<()> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::BadRequest("remote name must not be empty".to_string()));
    }
    let result = sqlx::query("UPDATE ir_remotes SET name = ? WHERE id = ?")
        .bind(name)
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| match e.as_database_error().map(|d| d.is_unique_violation()) {
            Some(true) => AppError::Conflict(format!("remote '{name}' already exists")),
            _ => AppError::Persistence(e.to_string()),
        })?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("IR remote {id} not found")));
    }
    Ok(())
}

pub async fn delete_remote(pool: &Pool<Sqlite>, id: i64) -> Result<()> {
    // Delete the buttons and the remote atomically: if the remote delete
    // fails we must not leave the buttons already removed.
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM ir_buttons WHERE remote_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let result = sqlx::query("DELETE FROM ir_remotes WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    if result.rows_affected() == 0 {
        tx.rollback().await.ok();
        return Err(AppError::NotFound(format!("IR remote {id} not found")));
    }
    tx.commit().await?;
    Ok(())
}

pub async fn insert_button(
    pool: &Pool<Sqlite>,
    remote_id: i64,
    name: &str,
    proto: &str,
    scancode: Option<i64>,
    raw: Option<&str>,
    carrier: i64,
) -> Result<i64> {
    let result = sqlx::query(
        "INSERT INTO ir_buttons (remote_id, name, proto, scancode, raw, carrier, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(remote_id)
    .bind(name.trim())
    .bind(proto)
    .bind(scancode)
    .bind(raw)
    .bind(carrier)
    .bind(now())
    .execute(pool)
    .await
    .map_err(|e| AppError::Persistence(e.to_string()))?;
    Ok(result.last_insert_rowid())
}

pub async fn get_button(pool: &Pool<Sqlite>, id: i64) -> Result<Option<IrButtonRecord>> {
    let row = sqlx::query(
        "SELECT id, remote_id, name, proto, scancode, raw IS NOT NULL AS has_raw, carrier, slot \
         FROM ir_buttons WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    row.map(|row| {
        Ok(IrButtonRecord {
            id: row.try_get("id")?,
            remote_id: row.try_get("remote_id")?,
            name: row.try_get("name")?,
            proto: row.try_get("proto")?,
            scancode: row.try_get("scancode")?,
            has_raw: row.try_get::<i64, _>("has_raw")? != 0,
            carrier: row.try_get("carrier")?,
            slot: row.try_get("slot")?,
        })
    })
    .transpose()
}

pub async fn get_button_raw(pool: &Pool<Sqlite>, id: i64) -> Result<Option<String>> {
    let raw: Option<Option<String>> = sqlx::query_scalar("SELECT raw FROM ir_buttons WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(raw.flatten())
}

pub async fn update_button(
    pool: &Pool<Sqlite>,
    id: i64,
    name: Option<&str>,
    slot: Option<Option<i64>>,
) -> Result<()> {
    if let Some(name) = name {
        sqlx::query("UPDATE ir_buttons SET name = ? WHERE id = ?")
            .bind(name.trim())
            .bind(id)
            .execute(pool)
            .await?;
    }
    if let Some(slot) = slot {
        // Only buttons of the current KVM-switch remote may bind slots.
        let kvm: Option<i64> = sqlx::query_scalar(
            "SELECT r.id FROM ir_buttons b JOIN ir_remotes r ON r.id = b.remote_id \
             WHERE b.id = ? AND r.is_kvm = 1",
        )
        .bind(id)
        .fetch_optional(pool)
        .await?;
        if kvm.is_none() {
            return Err(AppError::BadRequest(
                "only the KVM-switch remote can bind slots".to_string(),
            ));
        }
        if let Some(slot) = slot {
            if !(1..=8).contains(&slot) {
                return Err(AppError::BadRequest(format!(
                    "slot must be between 1 and 8, got {slot}"
                )));
            }
            // Slots are unique within a remote: clear any other button
            // already bound to this slot before assigning it.
            sqlx::query(
                "UPDATE ir_buttons SET slot = NULL \
                 WHERE remote_id = (SELECT remote_id FROM ir_buttons WHERE id = ?) \
                 AND slot = ? AND id != ?",
            )
            .bind(id)
            .bind(slot)
            .bind(id)
            .execute(pool)
            .await?;
        }
        sqlx::query("UPDATE ir_buttons SET slot = ? WHERE id = ?")
            .bind(slot)
            .bind(id)
            .execute(pool)
            .await?;
    }
    Ok(())
}

pub async fn delete_button(pool: &Pool<Sqlite>, id: i64) -> Result<()> {
    let result = sqlx::query("DELETE FROM ir_buttons WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("IR button {id} not found")));
    }
    Ok(())
}

pub async fn get_remote_by_name(pool: &Pool<Sqlite>, name: &str) -> Result<Option<i64>> {
    let id: Option<i64> = sqlx::query_scalar("SELECT id FROM ir_remotes WHERE name = ?")
        .bind(name)
        .fetch_optional(pool)
        .await?;
    Ok(id)
}

pub async fn button_name_exists(
    pool: &Pool<Sqlite>,
    remote_id: i64,
    name: &str,
) -> Result<bool> {
    let id: Option<i64> =
        sqlx::query_scalar("SELECT id FROM ir_buttons WHERE remote_id = ? AND name = ?")
            .bind(remote_id)
            .bind(name)
            .fetch_optional(pool)
            .await?;
    Ok(id.is_some())
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
