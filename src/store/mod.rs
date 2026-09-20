use anyhow::{Context, Result};
use rust_decimal::Decimal;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use std::str::FromStr;

use crate::config::StrategyConfig;
use crate::types::{ClientOrderId, FillRecord, Px, Qty, Side, StrategyId, SymbolId, Ts};

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}

impl Store {
    pub async fn open(database_url: &str) -> Result<Self> {
        if let Some(path) = database_url
            .strip_prefix("sqlite://")
            .or_else(|| database_url.strip_prefix("sqlite:"))
        {
            if let Some(parent) = std::path::Path::new(path).parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("create {}", parent.display()))?;
            }
        }
        let opts = SqliteConnectOptions::from_str(database_url)?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(opts)
            .await
            .context("open sqlite")?;
        let store = Self { pool };
        store.migrate().await?;
        Ok(store)
    }

    async fn migrate(&self) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS strategies (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                enabled INTEGER NOT NULL,
                mode TEXT NOT NULL,
                config_json TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS fills (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                strategy_id TEXT NOT NULL,
                coid TEXT NOT NULL,
                venue TEXT NOT NULL,
                symbol TEXT NOT NULL,
                side TEXT NOT NULL,
                px TEXT NOT NULL,
                qty TEXT NOT NULL,
                fee TEXT,
                ts INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS journal (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                strategy_id TEXT,
                kind TEXT NOT NULL,
                payload TEXT NOT NULL,
                ts INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS positions (
                strategy_id TEXT PRIMARY KEY,
                net_qty TEXT NOT NULL,
                avg_px TEXT,
                updated_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_fills_strategy ON fills(strategy_id, ts);
            CREATE INDEX IF NOT EXISTS idx_journal_ts ON journal(ts);
            DROP TABLE IF EXISTS orders;
            "#,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn upsert_strategy(&self, cfg: &StrategyConfig) -> Result<()> {
        cfg.validate()?;
        let now = Ts::now_system().millis();
        let json = serde_json::to_string(cfg)?;
        sqlx::query(
            r#"
            INSERT INTO strategies (id, name, enabled, mode, config_json, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET
                name=excluded.name,
                enabled=excluded.enabled,
                mode=excluded.mode,
                config_json=excluded.config_json,
                updated_at=excluded.updated_at
            "#,
        )
        .bind(cfg.id.as_str())
        .bind(&cfg.name)
        .bind(if cfg.enabled { 1 } else { 0 })
        .bind(serde_json::to_string(&cfg.mode)?.trim_matches('"'))
        .bind(json)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete_strategy(&self, id: &StrategyId) -> Result<bool> {
        let res = sqlx::query("DELETE FROM strategies WHERE id = ?")
            .bind(id.as_str())
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    pub async fn get_strategy(&self, id: &StrategyId) -> Result<Option<StrategyConfig>> {
        let row = sqlx::query("SELECT config_json FROM strategies WHERE id = ?")
            .bind(id.as_str())
            .fetch_optional(&self.pool)
            .await?;
        match row {
            Some(r) => {
                let json: String = r.try_get("config_json")?;
                Ok(Some(serde_json::from_str(&json)?))
            }
            None => Ok(None),
        }
    }

    pub async fn list_strategies(&self) -> Result<Vec<StrategyConfig>> {
        let rows = sqlx::query("SELECT config_json FROM strategies ORDER BY updated_at DESC")
            .fetch_all(&self.pool)
            .await?;
        let mut out = Vec::new();
        for r in rows {
            let json: String = r.try_get("config_json")?;
            out.push(serde_json::from_str(&json)?);
        }
        Ok(out)
    }

    pub async fn insert_fill(&self, fill: &FillRecord) -> Result<i64> {
        let res = sqlx::query(
            r#"
            INSERT INTO fills (strategy_id, coid, venue, symbol, side, px, qty, fee, ts)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&fill.strategy_id)
        .bind(fill.coid.as_str())
        .bind(fill.venue.as_str())
        .bind(fill.symbol.as_str())
        .bind(fill.side.as_str())
        .bind(fill.px.0.to_string())
        .bind(fill.qty.0.to_string())
        .bind(fill.fee.map(|f| f.to_string()))
        .bind(fill.ts.millis())
        .execute(&self.pool)
        .await?;
        Ok(res.last_insert_rowid())
    }

    pub async fn list_fills(&self, strategy_id: Option<&str>, limit: i64) -> Result<Vec<FillRecord>> {
        let rows = if let Some(id) = strategy_id {
            sqlx::query("SELECT * FROM fills WHERE strategy_id = ? ORDER BY ts DESC LIMIT ?")
                .bind(id)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
        } else {
            sqlx::query("SELECT * FROM fills ORDER BY ts DESC LIMIT ?")
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
        };
        rows.into_iter().map(row_to_fill).collect()
    }

    pub async fn upsert_position(
        &self,
        strategy_id: &str,
        net_qty: Qty,
        avg_px: Option<Px>,
    ) -> Result<()> {
        let now = Ts::now_system().millis();
        sqlx::query(
            r#"
            INSERT INTO positions (strategy_id, net_qty, avg_px, updated_at)
            VALUES (?, ?, ?, ?)
            ON CONFLICT(strategy_id) DO UPDATE SET
                net_qty=excluded.net_qty,
                avg_px=excluded.avg_px,
                updated_at=excluded.updated_at
            "#,
        )
        .bind(strategy_id)
        .bind(net_qty.0.to_string())
        .bind(avg_px.map(|p| p.0.to_string()))
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_positions(&self) -> Result<Vec<PositionRow>> {
        let rows = sqlx::query("SELECT * FROM positions")
            .fetch_all(&self.pool)
            .await?;
        let mut out = Vec::new();
        for r in rows {
            out.push(PositionRow {
                strategy_id: r.try_get("strategy_id")?,
                net_qty: parse_qty(&r.try_get::<String, _>("net_qty")?)?,
                avg_px: match r.try_get::<Option<String>, _>("avg_px")? {
                    Some(s) => Some(parse_px(&s)?),
                    None => None,
                },
                updated_at: Ts::from_millis(r.try_get("updated_at")?),
            });
        }
        Ok(out)
    }

    pub async fn append_journal(
        &self,
        strategy_id: Option<&str>,
        kind: &str,
        payload: &serde_json::Value,
    ) -> Result<()> {
        sqlx::query("INSERT INTO journal (strategy_id, kind, payload, ts) VALUES (?, ?, ?, ?)")
            .bind(strategy_id)
            .bind(kind)
            .bind(payload.to_string())
            .bind(Ts::now_system().millis())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn recent_journal(
        &self,
        strategy_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<JournalRow>> {
        let rows = if let Some(id) = strategy_id {
            sqlx::query("SELECT * FROM journal WHERE strategy_id = ? ORDER BY id DESC LIMIT ?")
                .bind(id)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
        } else {
            sqlx::query("SELECT * FROM journal ORDER BY id DESC LIMIT ?")
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
        };
        let mut out = Vec::new();
        for r in rows {
            out.push(JournalRow {
                id: r.try_get("id")?,
                strategy_id: r.try_get("strategy_id")?,
                kind: r.try_get("kind")?,
                payload: r.try_get("payload")?,
                ts: Ts::from_millis(r.try_get("ts")?),
            });
        }
        Ok(out)
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct PositionRow {
    pub strategy_id: String,
    pub net_qty: Qty,
    pub avg_px: Option<Px>,
    pub updated_at: Ts,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct JournalRow {
    pub id: i64,
    pub strategy_id: Option<String>,
    pub kind: String,
    pub payload: String,
    pub ts: Ts,
}

fn parse_px(s: &str) -> Result<Px> {
    Ok(Px(s.parse::<Decimal>()?))
}

fn parse_qty(s: &str) -> Result<Qty> {
    Ok(Qty(s.parse::<Decimal>()?))
}

fn row_to_fill(r: sqlx::sqlite::SqliteRow) -> Result<FillRecord> {
    Ok(FillRecord {
        id: r.try_get("id")?,
        strategy_id: r.try_get("strategy_id")?,
        coid: ClientOrderId(r.try_get("coid")?),
        venue: r.try_get::<String, _>("venue")?.parse().map_err(anyhow::Error::msg)?,
        symbol: SymbolId::new(r.try_get::<String, _>("symbol")?),
        side: match r.try_get::<String, _>("side")?.as_str() {
            "sell" => Side::Sell,
            _ => Side::Buy,
        },
        px: parse_px(&r.try_get::<String, _>("px")?)?,
        qty: parse_qty(&r.try_get::<String, _>("qty")?)?,
        fee: r
            .try_get::<Option<String>, _>("fee")?
            .map(|s| s.parse())
            .transpose()?,
        ts: Ts::from_millis(r.try_get("ts")?),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::StrategyConfig;

    #[tokio::test]
    async fn strategy_roundtrip() {
        let dir = std::env::temp_dir().join(format!("mm-test-{}.db", uuid::Uuid::new_v4()));
        let url = format!("sqlite://{}", dir.display());
        let store = Store::open(&url).await.unwrap();
        let cfg = StrategyConfig::default();
        store.upsert_strategy(&cfg).await.unwrap();
        let got = store.get_strategy(&cfg.id).await.unwrap().unwrap();
        assert_eq!(got.name, cfg.name);
        assert_eq!(store.list_strategies().await.unwrap().len(), 1);
        store.delete_strategy(&cfg.id).await.unwrap();
        assert!(store.get_strategy(&cfg.id).await.unwrap().is_none());
        let _ = std::fs::remove_file(dir);
    }

    /// Order lifecycle rows are no longer kept, so an existing database must shed the table.
    #[tokio::test]
    async fn migration_drops_legacy_orders_table() {
        let path = std::env::temp_dir().join(format!("mm-test-{}.db", uuid::Uuid::new_v4()));
        let url = format!("sqlite://{}", path.display());
        let store = Store::open(&url).await.unwrap();
        sqlx::query("CREATE TABLE orders (coid TEXT PRIMARY KEY)")
            .execute(&store.pool)
            .await
            .unwrap();
        drop(store);

        let store = Store::open(&url).await.unwrap();
        let found: i64 =
            sqlx::query_scalar("SELECT count(*) FROM sqlite_master WHERE name = 'orders'")
                .fetch_one(&store.pool)
                .await
                .unwrap();
        assert_eq!(found, 0);
        let _ = std::fs::remove_file(path);
    }
}
