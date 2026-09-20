use anyhow::{Context, Result, bail};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::types::{RunMode, StrategyId, SymbolId, VenueId};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppConfig {
    pub listen: String,
    pub database_url: String,
    pub trading_env: TradingEnv,
    pub log_filter: String,
    pub web_dir: PathBuf,
    pub recommended_underlyings: Vec<String>,
    pub global_risk: GlobalRiskConfig,
    pub venues: VenueSecrets,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TradingEnv {
    Testnet,
    Mainnet,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GlobalRiskConfig {
    pub max_notional: Decimal,
    pub max_notional_per_venue: Decimal,
    pub max_daily_loss: Decimal,
}

impl Default for GlobalRiskConfig {
    fn default() -> Self {
        Self {
            max_notional: Decimal::from(50_000),
            max_notional_per_venue: Decimal::from(30_000),
            max_daily_loss: Decimal::from(1_000),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct VenueSecrets {
    pub binance_key: Option<String>,
    pub binance_secret: Option<String>,
    pub gate_key: Option<String>,
    pub gate_secret: Option<String>,
}

impl VenueSecrets {
    pub fn has_binance(&self) -> bool {
        self.binance_key.as_ref().is_some_and(|s| !s.is_empty())
            && self.binance_secret.as_ref().is_some_and(|s| !s.is_empty())
    }

    pub fn has_gate(&self) -> bool {
        self.gate_key.as_ref().is_some_and(|s| !s.is_empty())
            && self.gate_secret.as_ref().is_some_and(|s| !s.is_empty())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileAppConfig {
    #[serde(default = "default_listen")]
    pub listen: String,
    #[serde(default)]
    pub recommended_underlyings: Vec<String>,
    #[serde(default)]
    pub global_risk: GlobalRiskConfig,
}

fn default_listen() -> String {
    "127.0.0.1:8080".into()
}

impl Default for FileAppConfig {
    fn default() -> Self {
        Self {
            listen: default_listen(),
            recommended_underlyings: default_underlyings(),
            global_risk: GlobalRiskConfig::default(),
        }
    }
}

pub fn default_underlyings() -> Vec<String> {
    [
        "AAPL", "TSLA", "NVDA", "MSFT", "AMZN", "GOOGL", "META", "NFLX", "AMD", "BABA", "COIN",
        "MSTR", "PLTR", "AVGO", "INTC", "CRM", "ORCL", "QCOM", "HOOD", "SMCI",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StrategyConfig {
    pub id: StrategyId,
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub mode: RunMode,
    pub market: MarketConfig,
    pub pricing: PricingConfig,
    pub exit: ExitConfig,
    pub risk: RiskLimitsConfig,
}

fn default_true() -> bool {
    true
}

impl Default for RunMode {
    fn default() -> Self {
        Self::Observe
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MarketConfig {
    pub maker_venue: VenueId,
    pub maker_symbol: SymbolId,
    pub ref_venue: VenueId,
    pub ref_symbol: SymbolId,
    #[serde(default = "one")]
    pub fx: Decimal,
    #[serde(default = "one")]
    pub multiplier: Decimal,
}

fn one() -> Decimal {
    Decimal::ONE
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GridLevel {
    /// Fractional distance from fair, e.g. 0.001 = 10 bps.
    pub distance: Decimal,
    /// Quote notional in USDT.
    pub size: Decimal,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PricingConfig {
    #[serde(default = "spread_window_default")]
    pub spread_window_ms: u64,
    #[serde(default = "min_samples_default")]
    pub min_samples: usize,
    #[serde(default = "bucket_ms_default")]
    pub bucket_ms: u64,
    pub grid: Vec<GridLevel>,
    pub requote_threshold: Decimal,
    pub jump_threshold: Decimal,
}

fn spread_window_default() -> u64 {
    120_000
}
fn min_samples_default() -> usize {
    30
}
fn bucket_ms_default() -> u64 {
    100
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExitConfig {
    pub take_profit: Decimal,
    pub fast_exit_timeout_ms: u64,
    pub force_exit_timeout_ms: u64,
    pub max_loss: Decimal,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RiskLimitsConfig {
    pub max_position: Decimal,
    pub max_long: Decimal,
    pub max_short: Decimal,
    pub max_order_size: Decimal,
    pub max_book_age_ms: u64,
    #[serde(default = "k_default")]
    pub max_spread_dev_k: Decimal,
    pub max_abs_spread_dev: Option<Decimal>,
}

fn k_default() -> Decimal {
    Decimal::from(6)
}

impl StrategyConfig {
    pub fn validate(&self) -> Result<()> {
        if self.id.as_str().is_empty() {
            bail!("strategy id is required");
        }
        if self.id.as_str().contains('-') {
            bail!("strategy id must not contain '-' (used in client_order_id)");
        }
        if self.name.trim().is_empty() {
            bail!("strategy name is required");
        }
        if self.market.fx <= Decimal::ZERO {
            bail!("fx must be > 0");
        }
        if self.market.multiplier <= Decimal::ZERO {
            bail!("multiplier must be > 0");
        }
        if self.pricing.grid.is_empty() {
            bail!("at least one grid level is required");
        }
        let mut min_grid = self.pricing.grid[0].distance;
        for (i, lvl) in self.pricing.grid.iter().enumerate() {
            if lvl.distance <= Decimal::ZERO {
                bail!("grid[{i}].distance must be > 0");
            }
            if lvl.size <= Decimal::ZERO {
                bail!("grid[{i}].size must be > 0");
            }
            if lvl.size > self.risk.max_order_size {
                bail!("grid[{i}].size exceeds max_order_size");
            }
            if lvl.distance < min_grid {
                min_grid = lvl.distance;
            }
        }
        if self.pricing.requote_threshold <= Decimal::ZERO {
            bail!("requote_threshold must be > 0");
        }
        if self.pricing.requote_threshold >= min_grid {
            bail!("requote_threshold must be < smallest grid distance");
        }
        if self.pricing.jump_threshold <= Decimal::ZERO {
            bail!("jump_threshold must be > 0");
        }
        if self.pricing.spread_window_ms < 1_000 {
            bail!("spread_window_ms must be >= 1000");
        }
        if self.exit.take_profit <= Decimal::ZERO {
            bail!("take_profit must be > 0");
        }
        if self.exit.max_loss <= Decimal::ZERO {
            bail!("max_loss must be > 0");
        }
        if self.exit.fast_exit_timeout_ms == 0 {
            bail!("fast_exit_timeout_ms must be > 0");
        }
        if self.exit.force_exit_timeout_ms <= self.exit.fast_exit_timeout_ms {
            bail!("force_exit_timeout_ms must be > fast_exit_timeout_ms");
        }
        if self.risk.max_position < Decimal::ZERO
            || self.risk.max_long < Decimal::ZERO
            || self.risk.max_short < Decimal::ZERO
            || self.risk.max_order_size <= Decimal::ZERO
        {
            bail!("position/order limits must be non-negative; max_order_size > 0");
        }
        if self.risk.max_long > self.risk.max_position || self.risk.max_short > self.risk.max_position
        {
            bail!("max_long/max_short cannot exceed max_position");
        }
        if self.risk.max_book_age_ms == 0 {
            bail!("max_book_age_ms must be > 0");
        }
        Ok(())
    }

    pub fn validate_fees(&self, maker_fee: Decimal, taker_fee: Decimal) -> Result<()> {
        let round_trip = maker_fee.abs() + maker_fee.abs();
        let _ = taker_fee;
        if self.exit.take_profit <= round_trip && maker_fee > Decimal::ZERO {
            bail!(
                "take_profit {} must exceed two-way maker fee {}",
                self.exit.take_profit,
                round_trip
            );
        }
        Ok(())
    }
}

impl Default for StrategyConfig {
    fn default() -> Self {
        Self {
            id: StrategyId::new("demo"),
            name: "Demo".into(),
            enabled: true,
            mode: RunMode::Observe,
            market: MarketConfig {
                maker_venue: VenueId::Gate,
                maker_symbol: SymbolId::new("AAPLUSDT"),
                ref_venue: VenueId::Binance,
                ref_symbol: SymbolId::new("AAPLUSDT"),
                fx: Decimal::ONE,
                multiplier: Decimal::ONE,
            },
            pricing: PricingConfig {
                spread_window_ms: 120_000,
                min_samples: 30,
                bucket_ms: 100,
                grid: vec![
                    GridLevel {
                        distance: dec_str("0.001"),
                        size: Decimal::from(100),
                    },
                    GridLevel {
                        distance: dec_str("0.002"),
                        size: Decimal::from(200),
                    },
                    GridLevel {
                        distance: dec_str("0.0035"),
                        size: Decimal::from(300),
                    },
                ],
                requote_threshold: dec_str("0.0005"),
                jump_threshold: dec_str("0.004"),
            },
            exit: ExitConfig {
                take_profit: dec_str("0.0012"),
                fast_exit_timeout_ms: 5_000,
                force_exit_timeout_ms: 12_000,
                max_loss: dec_str("0.004"),
            },
            risk: RiskLimitsConfig {
                max_position: Decimal::from(20_000),
                max_long: Decimal::from(10_000),
                max_short: Decimal::from(10_000),
                max_order_size: Decimal::from(2_000),
                max_book_age_ms: 15_000,
                max_spread_dev_k: Decimal::from(6),
                max_abs_spread_dev: Some(dec_str("0.01")),
            },
        }
    }
}

fn dec_str(s: &str) -> Decimal {
    s.parse().expect("literal decimal")
}

pub fn load_app_config() -> Result<AppConfig> {
    let _ = dotenvy::dotenv();
    let file = load_file_config(Path::new("config/app.toml"))?;
    let trading_env = match std::env::var("TRADING_ENV")
        .unwrap_or_else(|_| "testnet".into())
        .to_ascii_lowercase()
        .as_str()
    {
        "mainnet" | "prod" | "live" => TradingEnv::Mainnet,
        _ => TradingEnv::Testnet,
    };
    let listen = std::env::var("API_LISTEN").unwrap_or(file.listen);
    let database_url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite://data/mm.db".into());
    Ok(AppConfig {
        listen,
        database_url,
        trading_env,
        log_filter: std::env::var("RUST_LOG").unwrap_or_else(|_| "stock_mm=info,warn".into()),
        web_dir: PathBuf::from(std::env::var("WEB_DIR").unwrap_or_else(|_| "web/dist".into())),
        recommended_underlyings: if file.recommended_underlyings.is_empty() {
            default_underlyings()
        } else {
            file.recommended_underlyings
        },
        global_risk: file.global_risk,
        venues: VenueSecrets {
            binance_key: std::env::var("BINANCE_API_KEY").ok().filter(|s| !s.is_empty()),
            binance_secret: std::env::var("BINANCE_API_SECRET")
                .ok()
                .filter(|s| !s.is_empty()),
            gate_key: std::env::var("GATE_API_KEY").ok().filter(|s| !s.is_empty()),
            gate_secret: std::env::var("GATE_API_SECRET").ok().filter(|s| !s.is_empty()),
        },
    })
}

fn load_file_config(path: &Path) -> Result<FileAppConfig> {
    if !path.exists() {
        return Ok(FileAppConfig::default());
    }
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parse {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_strategy_is_valid() {
        StrategyConfig::default().validate().unwrap();
    }

    #[test]
    fn requote_must_be_inside_grid() {
        let mut cfg = StrategyConfig::default();
        cfg.pricing.requote_threshold = dec_str("0.002");
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn force_exit_must_follow_fast() {
        let mut cfg = StrategyConfig::default();
        cfg.exit.force_exit_timeout_ms = 1_000;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn strategy_id_rejects_dash() {
        let mut cfg = StrategyConfig::default();
        cfg.id = StrategyId::new("bad-id");
        assert!(cfg.validate().is_err());
    }
}
