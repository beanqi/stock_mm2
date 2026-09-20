use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;

use crate::types::{
    Action, DesiredOrder, Instrument, OrderType, PlaceReq, Px, Qty, Side, SlotKey, SlotStateKind,
    SymbolId, TimeInForce, Ts, VenueId, make_client_order_id, round_buy_px, round_exit_px,
    round_qty_down, round_sell_px,
};
use crate::types::{ClientOrderId, StrategyId};

use super::state::CoreState;

#[derive(Clone, Debug)]
pub struct DesiredSlot {
    pub key: SlotKey,
    pub order: DesiredOrder,
}

pub fn grid_targets(
    fair: Px,
    levels: &[crate::config::GridLevel],
    inst: &Instrument,
    allow_buy: bool,
    allow_sell: bool,
    maker_book: Option<&crate::types::TopOfBook>,
) -> Vec<DesiredSlot> {
    let mut out = Vec::new();
    for (i, lvl) in levels.iter().enumerate() {
        let idx = i as u8;
        let qty_raw = crate::types::notional_to_qty(lvl.size, fair, inst.contract_size);
        let mut qty = round_qty_down(qty_raw, inst.lot_size);
        if qty <= rust_decimal::Decimal::ZERO {
            qty = inst.min_qty.max(inst.lot_size);
        }
        let qty = Qty(qty);
        if !qty.is_positive() {
            continue;
        }
        if allow_buy {
            let raw = fair.0 * (Decimal::ONE - lvl.distance);
            let mut px = Px(round_buy_px(raw, inst.tick_size));
            if let Some(book) = maker_book {
                let cap = book.ask_px.0 - inst.tick_size;
                if px.0 > cap {
                    px = Px(round_buy_px(cap, inst.tick_size));
                }
            }
            if px.is_positive() {
                out.push(DesiredSlot {
                    key: SlotKey::grid(Side::Buy, idx),
                    order: DesiredOrder {
                        px,
                        qty,
                        reduce_only: false,
                        tif: TimeInForce::PostOnly,
                        order_type: OrderType::Limit,
                    },
                });
            }
        }
        if allow_sell {
            let raw = fair.0 * (Decimal::ONE + lvl.distance);
            let mut px = Px(round_sell_px(raw, inst.tick_size));
            if let Some(book) = maker_book {
                let floor = book.bid_px.0 + inst.tick_size;
                if px.0 < floor {
                    px = Px(round_sell_px(floor, inst.tick_size));
                }
            }
            if px.is_positive() {
                out.push(DesiredSlot {
                    key: SlotKey::grid(Side::Sell, idx),
                    order: DesiredOrder {
                        px,
                        qty,
                        reduce_only: false,
                        tif: TimeInForce::PostOnly,
                        order_type: OrderType::Limit,
                    },
                });
            }
        }
    }
    out
}

/// Drop the lower-priority side (grid) if the combined book would self-cross.
pub fn prevent_self_cross(desired: &mut Vec<DesiredSlot>) {
    loop {
        let max_buy = desired
            .iter()
            .filter(|d| d.key.side == Side::Buy)
            .map(|d| d.order.px)
            .max();
        let min_sell = desired
            .iter()
            .filter(|d| d.key.side == Side::Sell)
            .map(|d| d.order.px)
            .min();
        let (Some(b), Some(s)) = (max_buy, min_sell) else {
            break;
        };
        if b < s {
            break;
        }
        // Prefer keeping exits over grid.
        let drop_idx = desired.iter().position(|d| {
            d.key.purpose == crate::types::Purpose::GridEntry
                && ((d.key.side == Side::Buy && d.order.px == b)
                    || (d.key.side == Side::Sell && d.order.px == s))
        });
        if let Some(i) = drop_idx {
            desired.remove(i);
        } else {
            // last resort: drop a buy
            if let Some(i) = desired.iter().position(|d| d.key.side == Side::Buy && d.order.px == b)
            {
                desired.remove(i);
            } else {
                break;
            }
        }
    }
}

pub fn deviation(target: Px, live: Px, fair: Px) -> f64 {
    if fair.0.is_zero() {
        return f64::MAX;
    }
    ((target.0 - live.0).abs() / fair.0)
        .to_f64()
        .unwrap_or(f64::MAX)
}

pub fn reconcile(
    state: &mut CoreState,
    strategy_id: &StrategyId,
    venue: VenueId,
    symbol: &SymbolId,
    desired: &[DesiredSlot],
    requote: Decimal,
    fair: Option<Px>,
    supports_amend: bool,
    now: Ts,
    out: &mut Vec<Action>,
) {
    let desired_keys: std::collections::HashSet<SlotKey> = desired.iter().map(|d| d.key).collect();
    for d in desired {
        let slot = state.slot(d.key);
        slot.desired = Some(d.order.clone());
        if slot.is_blocked(now) {
            continue;
        }
        match slot.state {
            SlotStateKind::Empty => {
                let seq = state.alloc_seq();
                let coid = make_client_order_id(strategy_id, d.key, seq);
                place(state, d.key, &coid, venue, symbol, &d.order, now, out);
            }
            SlotStateKind::Live => {
                let live = slot.live.clone().expect("live slot");
                let qty_changed = live.qty != d.order.qty;
                let px_dev = fair
                    .map(|f| deviation(d.order.px, live.px, f))
                    .unwrap_or(f64::MAX);
                let need = qty_changed
                    || px_dev > requote.to_f64().unwrap_or(0.0);
                if !need {
                    continue;
                }
                if supports_amend {
                    slot.state = SlotStateKind::PendingAmend;
                    slot.pending_since = Some(now);
                    slot.amend_from = Some((live.px, live.qty));
                    if let Some(l) = slot.live.as_mut() {
                        l.px = d.order.px;
                        l.qty = d.order.qty;
                    }
                    out.push(Action::Amend {
                        coid: live.coid,
                        venue,
                        symbol: symbol.clone(),
                        px: d.order.px,
                        qty: d.order.qty,
                    });
                } else {
                    slot.state = SlotStateKind::PendingCancel;
                    slot.pending_since = Some(now);
                    out.push(Action::Cancel {
                        coid: live.coid,
                        venue,
                        symbol: symbol.clone(),
                    });
                }
            }
            SlotStateKind::PendingNew
            | SlotStateKind::PendingAmend
            | SlotStateKind::PendingCancel
            | SlotStateKind::Unknown => {}
        }
    }

    let stale: Vec<(SlotKey, ClientOrderId)> = state
        .slots
        .iter()
        .filter(|(k, s)| {
            !desired_keys.contains(k)
                && s.state == SlotStateKind::Live
                && s.live.is_some()
        })
        .map(|(k, s)| (*k, s.live.as_ref().unwrap().coid.clone()))
        .collect();
    for (key, coid) in stale {
        let slot = state.slot(key);
        slot.desired = None;
        slot.state = SlotStateKind::PendingCancel;
        slot.pending_since = Some(now);
        out.push(Action::Cancel {
            coid,
            venue,
            symbol: symbol.clone(),
        });
    }
}

fn place(
    state: &mut CoreState,
    key: SlotKey,
    coid: &ClientOrderId,
    venue: VenueId,
    symbol: &SymbolId,
    order: &DesiredOrder,
    now: Ts,
    out: &mut Vec<Action>,
) {
    let slot = state.slot(key);
    slot.state = SlotStateKind::PendingNew;
    slot.pending_since = Some(now);
    slot.amend_from = None;
    slot.live = Some(crate::types::LiveOrder {
        coid: coid.clone(),
        exchange_id: None,
        px: order.px,
        qty: order.qty,
        filled_qty: Qty::default(),
        reduce_only: order.reduce_only,
    });
    out.push(Action::Place(PlaceReq {
        coid: coid.clone(),
        venue,
        symbol: symbol.clone(),
        side: key.side,
        px: order.px,
        qty: order.qty,
        tif: order.tif,
        reduce_only: order.reduce_only,
        order_type: order.order_type,
    }));
}

pub fn mark_cancel_all(state: &mut CoreState, now: Ts) {
    for slot in state.slots.values_mut() {
        if matches!(
            slot.state,
            SlotStateKind::Live | SlotStateKind::PendingNew | SlotStateKind::PendingAmend
        ) {
            slot.state = SlotStateKind::PendingCancel;
            slot.pending_since = Some(now);
            slot.desired = None;
        }
    }
}

pub fn exit_px_maker(entry: Px, side: Side, take_profit: Decimal, tick: Decimal) -> Px {
    let target = match side {
        Side::Buy => entry.0 * (Decimal::ONE + take_profit),
        Side::Sell => entry.0 * (Decimal::ONE - take_profit),
    };
    Px(round_exit_px(side.opposite(), target, tick))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::GridLevel;
    use crate::types::{
        ContractKind, InstrumentStatus, SymbolId, VenueId,
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

    #[test]
    fn grid_around_fair() {
        let levels = vec![
            GridLevel {
                distance: dec!(0.001),
                size: dec!(100),
            },
            GridLevel {
                distance: dec!(0.002),
                size: dec!(200),
            },
        ];
        let g = grid_targets(Px::new(dec!(100)), &levels, &inst(), true, true, None);
        assert_eq!(g.len(), 4);
        let buys: Vec<_> = g.iter().filter(|d| d.key.side == Side::Buy).collect();
        assert_eq!(buys[0].order.px.0, dec!(99.90));
        assert_eq!(buys[1].order.px.0, dec!(99.80));
        let sells: Vec<_> = g.iter().filter(|d| d.key.side == Side::Sell).collect();
        assert_eq!(sells[0].order.px.0, dec!(100.10));
        assert_eq!(sells[1].order.px.0, dec!(100.20));
    }

    #[test]
    fn self_cross_drops_grid() {
        let mut desired = vec![
            DesiredSlot {
                key: SlotKey::exit(Side::Sell, 0),
                order: DesiredOrder {
                    px: Px::new(dec!(100)),
                    qty: Qty::new(dec!(1)),
                    reduce_only: true,
                    tif: TimeInForce::PostOnly,
                    order_type: OrderType::Limit,
                },
            },
            DesiredSlot {
                key: SlotKey::grid(Side::Buy, 0),
                order: DesiredOrder {
                    px: Px::new(dec!(100.10)),
                    qty: Qty::new(dec!(1)),
                    reduce_only: false,
                    tif: TimeInForce::PostOnly,
                    order_type: OrderType::Limit,
                },
            },
        ];
        prevent_self_cross(&mut desired);
        assert_eq!(desired.len(), 1);
        assert_eq!(desired[0].key.purpose, crate::types::Purpose::Exit);
    }
}
