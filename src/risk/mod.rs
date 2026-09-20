use rust_decimal::Decimal;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::config::GlobalRiskConfig;
use crate::types::{Qty, StateSnapshot, VenueId};

#[derive(Debug)]
pub struct GlobalRisk {
    pub kill: AtomicBool,
    cfg: GlobalRiskConfig,
}

#[derive(Clone, Debug, Serialize)]
pub struct GlobalRiskView {
    pub kill: bool,
    pub total_abs_qty: String,
    pub max_notional: String,
    pub max_notional_per_venue: String,
    pub max_daily_loss: String,
}

impl GlobalRisk {
    pub fn new(cfg: GlobalRiskConfig) -> Self {
        Self {
            kill: AtomicBool::new(false),
            cfg,
        }
    }

    pub fn is_killed(&self) -> bool {
        self.kill.load(Ordering::SeqCst)
    }

    pub fn set_kill(&self, v: bool) {
        self.kill.store(v, Ordering::SeqCst);
    }

    pub fn view(&self, snaps: &[StateSnapshot]) -> GlobalRiskView {
        let total: Decimal = snaps.iter().map(|s| s.net_qty.abs().0).sum();
        GlobalRiskView {
            kill: self.is_killed(),
            total_abs_qty: total.to_string(),
            max_notional: self.cfg.max_notional.to_string(),
            max_notional_per_venue: self.cfg.max_notional_per_venue.to_string(),
            max_daily_loss: self.cfg.max_daily_loss.to_string(),
        }
    }

    pub fn venue_notional_breach(&self, snaps: &[StateSnapshot], venue: VenueId) -> bool {
        let q: Decimal = snaps
            .iter()
            .filter(|s| s.maker_venue == venue)
            .map(|s| {
                let px = s.fair_price.map(|p| p.0).unwrap_or(Decimal::ONE);
                s.net_qty.abs().0 * px
            })
            .sum();
        q > self.cfg.max_notional_per_venue
    }

    pub fn _unused_qty(_q: Qty) {}
}
