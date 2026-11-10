//! SQLite storage for local API tokens and the usage log.
//!
//! Tokens are high-entropy random strings, so a single SHA-256 is an
//! adequate at-rest hash (no password-style KDF needed). Plaintext tokens are
//! returned exactly once, from [`Db::mint_token`].

use std::path::Path;

use chrono::{DateTime, Datelike, TimeZone, Utc};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    Row, SqlitePool,
};
use x3_core::{Task, UsageRecord};

pub const TOKEN_PREFIX: &str = "x3_live_";

#[derive(Clone)]
pub struct Db {
    pool: SqlitePool,
}

/// Token metadata (never includes the secret).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TokenInfo {
    pub id: String,
    pub label: String,
    /// First characters of the token, for recognising it in the UI.
    pub display_prefix: String,
    pub allowed_providers: Option<Vec<String>>,
    pub allowed_tasks: Option<Vec<Task>>,
    pub spend_cap_usd: Option<f64>,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub revoked_at: Option<String>,
}

/// Parameters for minting a token.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NewToken {
    pub label: String,
    #[serde(default)]
    pub allowed_providers: Option<Vec<String>>,
    #[serde(default)]
    pub allowed_tasks: Option<Vec<Task>>,
    #[serde(default)]
    pub spend_cap_usd: Option<f64>,
}

pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

fn generate_token() -> String {
    let mut bytes = [0u8; 24];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    format!("{TOKEN_PREFIX}{}", hex::encode(bytes))
}

fn now() -> String {
    Utc::now().to_rfc3339()
}

/// Start of the current calendar month, UTC.
pub fn month_start(now: DateTime<Utc>) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
        .single()
        .unwrap_or(now)
}

impl Db {
    pub async fn open(path: &Path) -> anyhow::Result<Self> {
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);
        Self::connect(opts).await
    }

    /// In-memory database for tests.
    pub async fn in_memory() -> anyhow::Result<Self> {
        let opts = SqliteConnectOptions::new().in_memory(true);
        Self::connect(opts).await
    }

    async fn connect(opts: SqliteConnectOptions) -> anyhow::Result<Self> {
        // One connection: in-memory DBs are per-connection, and our write
        // volume is tiny.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Self { pool })
    }

    /// Mint a token. Returns metadata and the plaintext token (shown once).
    pub async fn mint_token(&self, new: &NewToken) -> anyhow::Result<(TokenInfo, String)> {
        let label = new.label.trim();
        anyhow::ensure!(!label.is_empty(), "token label is required");
        if let Some(cap) = new.spend_cap_usd {
            anyhow::ensure!(cap >= 0.0, "spend cap must be >= 0");
        }
        let token = generate_token();
        let mut id_bytes = [0u8; 8];
        rand::rngs::OsRng.fill_bytes(&mut id_bytes);
        let info = TokenInfo {
            id: format!("tok_{}", hex::encode(id_bytes)),
            label: label.to_string(),
            display_prefix: token[..TOKEN_PREFIX.len() + 4].to_string(),
            allowed_providers: new.allowed_providers.clone(),
            allowed_tasks: new.allowed_tasks.clone(),
            spend_cap_usd: new.spend_cap_usd,
            created_at: now(),
            last_used_at: None,
            revoked_at: None,
        };
        sqlx::query(
            "INSERT INTO tokens (id, label, token_hash, display_prefix, allowed_providers, \
             allowed_tasks, spend_cap_usd, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&info.id)
        .bind(&info.label)
        .bind(hash_token(&token))
        .bind(&info.display_prefix)
        .bind(
            info.allowed_providers
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?,
        )
        .bind(
            info.allowed_tasks
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?,
        )
        .bind(info.spend_cap_usd)
        .bind(&info.created_at)
        .execute(&self.pool)
        .await?;
        Ok((info, token))
    }

    fn row_to_info(row: &sqlx::sqlite::SqliteRow) -> anyhow::Result<TokenInfo> {
        let providers: Option<String> = row.try_get("allowed_providers")?;
        let tasks: Option<String> = row.try_get("allowed_tasks")?;
        Ok(TokenInfo {
            id: row.try_get("id")?,
            label: row.try_get("label")?,
            display_prefix: row.try_get("display_prefix")?,
            allowed_providers: providers.map(|s| serde_json::from_str(&s)).transpose()?,
            allowed_tasks: tasks.map(|s| serde_json::from_str(&s)).transpose()?,
            spend_cap_usd: row.try_get("spend_cap_usd")?,
            created_at: row.try_get("created_at")?,
            last_used_at: row.try_get("last_used_at")?,
            revoked_at: row.try_get("revoked_at")?,
        })
    }

    pub async fn list_tokens(&self) -> anyhow::Result<Vec<TokenInfo>> {
        let rows = sqlx::query("SELECT * FROM tokens ORDER BY created_at DESC")
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(Self::row_to_info).collect()
    }

    /// Look up an active (non-revoked) token by its plaintext value.
    pub async fn authenticate(&self, token: &str) -> anyhow::Result<Option<TokenInfo>> {
        if !token.starts_with(TOKEN_PREFIX) {
            return Ok(None);
        }
        let row = sqlx::query("SELECT * FROM tokens WHERE token_hash = ? AND revoked_at IS NULL")
            .bind(hash_token(token))
            .fetch_optional(&self.pool)
            .await?;
        let Some(row) = row else { return Ok(None) };
        let info = Self::row_to_info(&row)?;
        sqlx::query("UPDATE tokens SET last_used_at = ? WHERE id = ?")
            .bind(now())
            .bind(&info.id)
            .execute(&self.pool)
            .await?;
        Ok(Some(info))
    }

    /// Revoke a token. Returns false if it did not exist or was revoked.
    pub async fn revoke_token(&self, id: &str) -> anyhow::Result<bool> {
        let r = sqlx::query("UPDATE tokens SET revoked_at = ? WHERE id = ? AND revoked_at IS NULL")
            .bind(now())
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() == 1)
    }

    pub async fn record_usage(&self, r: &UsageRecord) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO usage (ts, token_id, token_label, model, provider, task, units, unit, \
             est_cost_usd, status) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(r.ts.to_rfc3339())
        .bind(&r.token_id)
        .bind(&r.token_label)
        .bind(&r.model)
        .bind(&r.provider)
        .bind(r.task.as_str())
        .bind(r.units)
        .bind(r.unit.as_str())
        .bind(r.est_cost_usd)
        .bind(r.status.as_str())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Estimated spend for `provider` since `since`.
    pub async fn provider_spend_since(
        &self,
        provider: &str,
        since: DateTime<Utc>,
    ) -> anyhow::Result<f64> {
        let v: Option<f64> = sqlx::query_scalar(
            "SELECT SUM(est_cost_usd) FROM usage WHERE provider = ? AND ts >= ?",
        )
        .bind(provider)
        .bind(since.to_rfc3339())
        .fetch_one(&self.pool)
        .await?;
        Ok(v.unwrap_or(0.0))
    }

    /// Lifetime estimated spend of a token.
    pub async fn token_spend(&self, token_id: &str) -> anyhow::Result<f64> {
        let v: Option<f64> =
            sqlx::query_scalar("SELECT SUM(est_cost_usd) FROM usage WHERE token_id = ?")
                .bind(token_id)
                .fetch_one(&self.pool)
                .await?;
        Ok(v.unwrap_or(0.0))
    }

    /// Most recent usage rows, newest first.
    pub async fn recent_usage(&self, limit: i64) -> anyhow::Result<Vec<serde_json::Value>> {
        let rows = sqlx::query(
            "SELECT ts, token_label, model, provider, task, units, unit, est_cost_usd, status \
             FROM usage ORDER BY id DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(serde_json::json!({
                    "ts": r.try_get::<String, _>("ts")?,
                    "token_label": r.try_get::<String, _>("token_label")?,
                    "model": r.try_get::<String, _>("model")?,
                    "provider": r.try_get::<String, _>("provider")?,
                    "task": r.try_get::<String, _>("task")?,
                    "units": r.try_get::<f64, _>("units")?,
                    "unit": r.try_get::<String, _>("unit")?,
                    "est_cost_usd": r.try_get::<f64, _>("est_cost_usd")?,
                    "status": r.try_get::<String, _>("status")?,
                }))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use x3_core::{PricingUnit, UsageStatus};

    use super::*;

    #[tokio::test]
    async fn token_lifecycle() {
        let db = Db::in_memory().await.unwrap();
        let (info, token) = db
            .mint_token(&NewToken {
                label: "claude".into(),
                allowed_providers: Some(vec!["apple".into()]),
                allowed_tasks: Some(vec![Task::Tts]),
                spend_cap_usd: Some(1.0),
            })
            .await
            .unwrap();
        assert!(token.starts_with(TOKEN_PREFIX));
        assert_eq!(token.len(), TOKEN_PREFIX.len() + 48);
        assert!(token.starts_with(&info.display_prefix));

        let got = db.authenticate(&token).await.unwrap().unwrap();
        assert_eq!(got.id, info.id);
        assert_eq!(got.allowed_tasks, Some(vec![Task::Tts]));
        assert!(db.authenticate("x3_live_nope").await.unwrap().is_none());
        assert!(db.authenticate("garbage").await.unwrap().is_none());

        assert_eq!(db.list_tokens().await.unwrap().len(), 1);
        assert!(db.revoke_token(&info.id).await.unwrap());
        assert!(!db.revoke_token(&info.id).await.unwrap());
        assert!(db.authenticate(&token).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn plaintext_token_not_stored() {
        let db = Db::in_memory().await.unwrap();
        let (_, token) = db
            .mint_token(&NewToken {
                label: "t".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let rows: Vec<String> = sqlx::query_scalar("SELECT token_hash FROM tokens")
            .fetch_all(&db.pool)
            .await
            .unwrap();
        assert_eq!(rows, vec![hash_token(&token)]);
        assert!(!rows[0].contains(&token[TOKEN_PREFIX.len()..]));
    }

    #[tokio::test]
    async fn spend_queries() {
        let db = Db::in_memory().await.unwrap();
        let rec = |provider: &str, token: &str, cost: f64, ts: DateTime<Utc>| UsageRecord {
            ts,
            token_id: token.into(),
            token_label: "l".into(),
            model: format!("{provider}/m"),
            provider: provider.into(),
            task: Task::Tts,
            units: 10.0,
            unit: PricingUnit::Characters,
            est_cost_usd: cost,
            status: UsageStatus::Ok,
        };
        let now = Utc::now();
        let last_month = month_start(now) - chrono::Duration::days(2);
        db.record_usage(&rec("elevenlabs", "a", 1.0, now))
            .await
            .unwrap();
        db.record_usage(&rec("elevenlabs", "b", 2.0, now))
            .await
            .unwrap();
        db.record_usage(&rec("elevenlabs", "a", 4.0, last_month))
            .await
            .unwrap();
        db.record_usage(&rec("cartesia", "a", 8.0, now))
            .await
            .unwrap();
        let spent = db
            .provider_spend_since("elevenlabs", month_start(now))
            .await
            .unwrap();
        assert!((spent - 3.0).abs() < 1e-9);
        assert!((db.token_spend("a").await.unwrap() - 13.0).abs() < 1e-9);
        assert_eq!(db.recent_usage(10).await.unwrap().len(), 4);
    }
}
