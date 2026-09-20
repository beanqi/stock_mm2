use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;

use crate::config::StrategyConfig;
use crate::types::{
    Instrument, InstrumentStatus, LinkStatus, Px, QuotePermission, SlotStateKind, TopOfBook, Ts,
};

use super::state::CoreState;

pub fn evaluate(
    cfg: &StrategyConfig,
    state: &CoreState,
    inst: &Instrument,
    now: Ts,
) -> QuotePermission {
    if !state.started || state.lifecycle == crate::types::StrategyMode::Stopped {
        return QuotePermission::reduce_only("stopped");
    }
    if state.flatten_requested {
        return QuotePermission {
            allow_new_buy: false,
            allow_new_sell: false,
            force_cancel_all: true,
            force_flatten: true,
            reason: Some("flatten".into()),
        };
    }
    if inst.status != InstrumentStatus::Trading {
        return QuotePermission::reduce_only("instrument_halt");
    }
    if let Some(reason) = link_reason(&state.links, cfg.mode) {
        return QuotePermission {
            allow_new_buy: false,
            allow_new_sell: false,
            force_cancel_all: true,
            force_flatten: false,
            reason: Some(reason),
        };
    }
    if state.unknown_slots() > 0 {
        return QuotePermission::reduce_only("unknown_slots");
    }
    if let Some(until) = state.cooldown_until {
        if now < until {
            return QuotePermission {
                allow_new_buy: false,
                allow_new_sell: false,
                force_cancel_all: true,
                force_flatten: false,
                reason: Some("jump_cooldown".into()),
            };
        }
    }
    let Some(ref_book) = state.ref_book.as_ref() else {
        return QuotePermission::reduce_only("no_ref_book");
    };
    let Some(maker_book) = state.maker_book.as_ref() else {
        return QuotePermission::reduce_only("no_maker_book");
    };
    if !ref_book.is_valid() {
        return QuotePermission::reduce_only(if ref_book.crossed() {
            "ref_crossed"
        } else {
            "ref_invalid"
        });
    }
    if !maker_book.is_valid() {
        return QuotePermission::reduce_only(if maker_book.crossed() {
            "maker_crossed"
        } else {
            "maker_invalid"
        });
    }
    if age_ms(now, ref_book) > cfg.risk.max_book_age_ms as i64 {
        return QuotePermission::reduce_only("ref_stale");
    }
    if age_ms(now, maker_book) > cfg.risk.max_book_age_ms as i64 {
        return QuotePermission::reduce_only("maker_stale");
    }
    if state.window.len() < cfg.pricing.min_samples {
        return QuotePermission::reduce_only("warmup");
    }
    if let (Some(spread), Some(nat), Some(vol)) =
        (state.last_spread, state.last_natural, state.last_vol)
    {
        let k = cfg.risk.max_spread_dev_k.to_f64().unwrap_or(6.0);
        if vol > 0.0 && (spread - nat).abs() > k * vol {
            return QuotePermission::reduce_only("spread_regime_break");
        }
        if let Some(abs) = cfg.risk.max_abs_spread_dev {
            if (spread - nat).abs() > abs.to_f64().unwrap_or(f64::MAX) {
                return QuotePermission::reduce_only("spread_abs_break");
            }
        }
    }

    let mut perm = QuotePermission::open();
    let px = state.last_fair.map(|p| p.0).unwrap_or(Decimal::ONE);
    let cs = inst.contract_size.max(Decimal::new(1, 8));
    let notion = |q: rust_decimal::Decimal| q.abs() * px * cs;
    let long_n = if state.net_qty.0 > rust_decimal::Decimal::ZERO {
        notion(state.net_qty.0)
    } else {
        rust_decimal::Decimal::ZERO
    };
    let short_n = if state.net_qty.0 < rust_decimal::Decimal::ZERO {
        notion(state.net_qty.0)
    } else {
        rust_decimal::Decimal::ZERO
    };
    let abs_n = notion(state.net_qty.0);
    if long_n >= cfg.risk.max_long || abs_n >= cfg.risk.max_position {
        perm.allow_new_buy = false;
        perm.reason = Some("max_long".into());
    }
    if short_n >= cfg.risk.max_short || abs_n >= cfg.risk.max_position {
        perm.allow_new_sell = false;
        perm.reason = Some(match perm.reason {
            Some(r) => format!("{r},max_short"),
            None => "max_short".into(),
        });
    }
    if !perm.allow_new_buy && !perm.allow_new_sell {
        perm.reason = Some("position_cap".into());
    }
    perm
}

fn age_ms(now: Ts, book: &TopOfBook) -> i64 {
    now.duration_since_ms(book.exchange_ts)
}

fn link_reason(links: &LinkStatus, mode: crate::types::RunMode) -> Option<String> {
    if !links.ref_md {
        return Some("ref_md_down".into());
    }
    if !links.maker_md {
        return Some("maker_md_down".into());
    }
    if mode == crate::types::RunMode::Live {
        if !links.private {
            return Some("private_down".into());
        }
        if !links.trading {
            return Some("trading_down".into());
        }
    }
    None
}

pub fn float_loss(entry: Px, fair: Px, side: crate::types::Side) -> f64 {
    if entry.0.is_zero() {
        return 0.0;
    }
    let raw = match side {
        crate::types::Side::Buy => (entry.0 - fair.0) / entry.0,
        crate::types::Side::Sell => (fair.0 - entry.0) / entry.0,
    };
    raw.to_f64().unwrap_or(0.0)
}

pub fn pending_unknown_reason(state: &CoreState) -> bool {
    state
        .slots
        .values()
        .any(|s| s.state == SlotStateKind::Unknown)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::StrategyConfig;
    use crate::types::{
        BookUpdate, ContractKind, InstrumentStatus, Qty, Side, SymbolId, TopOfBook, VenueId,
    };
    use rust_decimal_macros::dec;

    fn inst() -> Instrument {
        Instrument {
            venue: VenueId::Gate,
            symbol: SymbolId::new("AAPLUSDT"),
            native_symbol: "AAPL_USDT".into(),
            tick_size: dec!(0.01),
            lot_size: dec!(0.01),
            contract_size: dec!(1),
            min_qty: dec!(0.01),
            min_notional: dec!(5),
            quote_ccy: "USDT".into(),
            maker_fee: dec!(0.0002),
            taker_fee: dec!(0.0005),
            kind: ContractKind::Stock,
            status: InstrumentStatus::Trading,
            volume_24h: dec!(0),
        }
    }

    fn book(px: rust_decimal::Decimal, ts: Ts) -> TopOfBook {
        TopOfBook {
            venue: VenueId::Binance,
            symbol: SymbolId::new("AAPLUSDT"),
            bid_px: Px::new(px),
            bid_qty: Qty::new(dec!(10)),
            ask_px: Px::new(px + dec!(0.02)),
            ask_qty: Qty::new(dec!(10)),
            exchange_ts: ts,
            recv_ts: ts,
        }
    }

    #[test]
    fn crossed_book_blocks() {
        let cfg = StrategyConfig::default();
        let mut st = CoreState::new(120_000, 100, dec!(1));
        st.started = true;
        st.lifecycle = crate::types::StrategyMode::Running;
        st.links = LinkStatus {
            ref_md: true,
            maker_md: true,
            private: true,
            trading: true,
        };
        let ts = Ts::from_millis(1_000);
        let mut b = book(dec!(100), ts);
        b.bid_px = Px::new(dec!(101));
        b.ask_px = Px::new(dec!(100));
        st.ref_book = Some(b.clone());
        st.maker_book = Some(book(dec!(100), ts));
        let p = evaluate(&cfg, &st, &inst(), ts);
        assert!(!p.allow_new_buy);
        assert_eq!(p.reason.as_deref(), Some("ref_crossed"));
        let _ = BookUpdate {
            book: b,
            seq: None,
        };
    }

    #[test]
    fn long_cap_blocks_buys() {
        let cfg = StrategyConfig::default();
        let mut st = CoreState::new(120_000, 100, dec!(1));
        st.started = true;
        st.lifecycle = crate::types::StrategyMode::Running;
        st.links = LinkStatus {
            ref_md: true,
            maker_md: true,
            private: true,
            trading: true,
        };
        let ts = Ts::from_millis(1_000);
        st.ref_book = Some(book(dec!(100), ts));
        st.maker_book = Some(book(dec!(100), ts));
        for i in 0..cfg.pricing.min_samples {
            st.window.push(ts.saturating_add_millis(i as i64 * 100), 0.0);
        }
        st.last_spread = Some(0.0);
        st.last_natural = Some(0.0);
        st.last_vol = Some(0.0001);
        st.net_qty = Qty::new(cfg.risk.max_long);
        let p = evaluate(&cfg, &st, &inst(), ts);
        assert!(!p.allow_new_buy);
        assert!(p.allow_new_sell);
        let _ = Side::Buy;
    }
}
