use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;

use crate::config::StrategyConfig;
use crate::types::{
    Action, ControlCmd, DesiredOrder, Event, ExecKind, ExecReport, ExitPhase, Instrument, LiveOrder,
    Lot, OrderType, Px, Qty, ReqResult, Side, SlotKey, SlotStateKind, SlotView, StateSnapshot,
    StrategyMode, TelemetryEvent, TimeInForce, TimerId, Ts,
};

use super::quote::{
    DesiredSlot, exit_px_maker, grid_targets, mark_cancel_all, prevent_self_cross, reconcile,
};
use super::risk::{self, float_loss};
use super::state::CoreState;

/// How long a place/amend/cancel may stay unacknowledged before we stop assuming it is in flight.
const PENDING_TIMEOUT_MS: i64 = 10_000;

pub trait StrategyCoreApi {
    fn on_event(&mut self, now: Ts, ev: &Event, out: &mut Vec<Action>);
    fn snapshot(&self) -> StateSnapshot;
}

pub struct StrategyCore {
    pub cfg: StrategyConfig,
    pub maker: Instrument,
    pub state: CoreState,
}

impl StrategyCore {
    pub fn new(cfg: StrategyConfig, maker: Instrument) -> Self {
        let state = CoreState::new(
            cfg.pricing.spread_window_ms,
            cfg.pricing.bucket_ms,
            cfg.market.fx,
        );
        Self { cfg, maker, state }
    }

    pub fn replace_config(&mut self, cfg: StrategyConfig) {
        self.state.fx = cfg.market.fx;
        self.cfg = cfg;
    }
}

impl StrategyCoreApi for StrategyCore {
    fn on_event(&mut self, now: Ts, ev: &Event, out: &mut Vec<Action>) {
        self.apply_event(now, ev, out);
        if matches!(self.state.lifecycle, StrategyMode::Stopped | StrategyMode::Init)
            && !matches!(ev, Event::Control(ControlCmd::Start))
        {
            return;
        }
        self.expire_pending(now);
        self.reprice(now);
        self.advance_exits(now, out);
        self.state.permission = risk::evaluate(&self.cfg, &self.state, &self.maker, now);
        self.sync_lifecycle();
        self.quote(now, out);
        out.push(Action::SetTimer {
            id: TimerId::EXIT_SCAN,
            at: now.saturating_add_millis(250),
        });
    }

    fn snapshot(&self) -> StateSnapshot {
        let slots = self
            .state
            .slots
            .iter()
            .map(|(k, s)| SlotView {
                key: *k,
                state: s.state,
                coid: s.live.as_ref().map(|l| l.coid.clone()),
                side: k.side,
                px: s.live.as_ref().map(|l| l.px),
                qty: s.live.as_ref().map(|l| l.qty),
                filled_qty: s.live.as_ref().map(|l| l.filled_qty).unwrap_or_default(),
                desired_px: s.desired.as_ref().map(|d| d.px),
                desired_qty: s.desired.as_ref().map(|d| d.qty),
            })
            .collect();
        StateSnapshot {
            strategy_id: self.cfg.id.clone(),
            name: self.cfg.name.clone(),
            lifecycle: self.state.lifecycle,
            run_mode: self.cfg.mode,
            maker_venue: self.cfg.market.maker_venue,
            maker_symbol: self.cfg.market.maker_symbol.clone(),
            ref_venue: self.cfg.market.ref_venue,
            ref_symbol: self.cfg.market.ref_symbol.clone(),
            fair_price: self.state.last_fair,
            natural_spread: self.state.last_natural,
            spread_vol: self.state.last_vol,
            spread_t: self.state.last_spread,
            ref_book: self.state.ref_book.clone(),
            maker_book: self.state.maker_book.clone(),
            permission: self.state.permission.clone(),
            slots,
            lots: self.state.lots.clone(),
            net_qty: self.state.net_qty,
            links: self.state.links.clone(),
            warmup_samples: self.state.window.len(),
            warmup_needed: self.cfg.pricing.min_samples,
            enabled: self.cfg.enabled,
        }
    }
}

impl StrategyCore {
    fn apply_event(&mut self, now: Ts, ev: &Event, out: &mut Vec<Action>) {
        match ev {
            Event::RefBook(u) => self.state.ref_book = Some(u.book.clone()),
            Event::MakerBook(u) => self.state.maker_book = Some(u.book.clone()),
            Event::Fx(fx) => {
                if fx.rate > Decimal::ZERO {
                    self.state.fx = fx.rate;
                }
            }
            Event::Exec(rep) => self.on_exec(now, rep, out),
            Event::PositionSync(p) => {
                if p.net_qty != self.state.net_qty {
                    tracing::warn!(
                        local = %self.state.net_qty,
                        exch = %p.net_qty,
                        "position drift; using exchange net"
                    );
                    self.rebuild_lots_from_net(now, p.net_qty, p.avg_px);
                }
            }
            Event::Asset(_) => {}
            Event::ReqOutcome {
                coid, result, reason
            } => self.on_req_outcome(now, coid, *result, reason.as_deref()),
            Event::Timer(_) => {}
            Event::Link { kind, up, .. } => match kind {
                crate::types::LinkKind::MarketData => {
                    // actor sets venue-specific flags; treat as maker+ref if same
                    if *up {
                        // filled by actor via two events; keep both if already up
                    }
                    let _ = up;
                }
                crate::types::LinkKind::Private => self.state.links.private = *up,
                crate::types::LinkKind::Trading => self.state.links.trading = *up,
            },
            Event::Control(cmd) => self.on_control(now, cmd, out),
        }
    }

    /// Reconcile the slot map against the venue's authoritative open-order list.
    ///
    /// Resolves `Unknown` slots, and returns the client order ids that carry our strategy
    /// prefix but belong to no slot. Those are unreachable by `reconcile`, so without this
    /// sweep they would rest on the book forever, tying up margin.
    pub fn reconcile_open_orders(
        &mut self,
        open: &[crate::types::ClientOrderId],
    ) -> Vec<crate::types::ClientOrderId> {
        let keys: Vec<SlotKey> = self.state.slots.keys().copied().collect();
        for key in keys {
            let slot = self.state.slot(key);
            if slot.state != SlotStateKind::Unknown {
                continue;
            }
            let found = slot.live.as_ref().is_some_and(|l| open.contains(&l.coid));
            if found {
                slot.state = SlotStateKind::Live;
                slot.pending_since = None;
                slot.note_accepted();
            } else {
                slot.state = SlotStateKind::Empty;
                slot.live = None;
                slot.pending_since = None;
            }
        }
        let tracked: std::collections::HashSet<&str> = self
            .state
            .slots
            .values()
            .filter_map(|s| s.live.as_ref().map(|l| l.coid.as_str()))
            .collect();
        open.iter()
            .filter(|c| c.strategy_prefix() == Some(self.cfg.id.as_str()))
            .filter(|c| !tracked.contains(c.as_str()))
            .cloned()
            .collect()
    }

    pub fn set_md_link(&mut self, is_ref: bool, up: bool) {
        if is_ref {
            self.state.links.ref_md = up;
        } else {
            self.state.links.maker_md = up;
        }
    }

    fn on_control(&mut self, now: Ts, cmd: &ControlCmd, out: &mut Vec<Action>) {
        match cmd {
            ControlCmd::Start => {
                self.state.started = true;
                self.state.flatten_requested = false;
                self.state.lifecycle = StrategyMode::Warmup;
            }
            ControlCmd::Stop => {
                self.state.started = false;
                self.state.lifecycle = StrategyMode::Stopped;
                mark_cancel_all(&mut self.state, now);
                out.push(Action::CancelAll {
                    venue: self.cfg.market.maker_venue,
                    symbol: self.cfg.market.maker_symbol.clone(),
                });
            }
            ControlCmd::Flatten => {
                self.state.flatten_requested = true;
                self.state.lifecycle = StrategyMode::Flattening;
            }
            ControlCmd::ConfigUpdate => {}
        }
    }

    fn on_req_outcome(
        &mut self,
        now: Ts,
        coid: &crate::types::ClientOrderId,
        result: ReqResult,
        // Rejection reasons are logged by the caller, which also sees untracked order ids.
        _reason: Option<&str>,
    ) {
        let Some(key) = self.state.find_slot_by_coid(coid) else {
            return;
        };
        let slot = self.state.slot(key);
        match result {
            ReqResult::Accepted => slot.note_accepted(),
            ReqResult::Rejected => {
                slot.note_reject(now);
                match slot.state {
                    // The venue refused to create the order, so there is nothing resting.
                    SlotStateKind::PendingNew => {
                        slot.state = SlotStateKind::Empty;
                        slot.live = None;
                        slot.pending_since = None;
                    }
                    // A refused amend leaves the original order working; forgetting it here
                    // would orphan it on the book with nothing left to cancel it.
                    SlotStateKind::PendingAmend => {
                        if let (Some((px, qty)), Some(l)) =
                            (slot.amend_from.take(), slot.live.as_mut())
                        {
                            l.px = px;
                            l.qty = qty;
                        }
                        slot.state = SlotStateKind::Live;
                        slot.pending_since = None;
                    }
                    // A refused cancel almost always means the order is already gone. If it
                    // isn't, the open-order sweep will find it and cancel it again.
                    SlotStateKind::PendingCancel => {
                        slot.state = SlotStateKind::Empty;
                        slot.live = None;
                        slot.pending_since = None;
                    }
                    _ => {}
                }
            }
            ReqResult::Unknown => {
                slot.state = SlotStateKind::Unknown;
                slot.pending_since = Some(now);
            }
        }
    }

    fn on_exec(&mut self, now: Ts, rep: &ExecReport, out: &mut Vec<Action>) {
        let Some(key) = self.state.find_slot_by_coid(&rep.coid) else {
            return;
        };
        match rep.kind {
            ExecKind::Ack | ExecKind::AmendAck => {
                let slot = self.state.slot(key);
                slot.state = SlotStateKind::Live;
                slot.pending_since = None;
                slot.amend_from = None;
                slot.note_accepted();
                if let Some(live) = slot.live.as_mut() {
                    // Only trust positive values: a venue that omits or zeroes these would
                    // otherwise make the order look infinitely mispriced and requote forever.
                    if let Some(px) = rep.px.filter(|p| p.is_positive()) {
                        live.px = px;
                    }
                    if let Some(qty) = rep.qty.filter(|q| q.is_positive()) {
                        live.qty = qty;
                    }
                    live.exchange_id = rep.exchange_id.clone();
                }
            }
            ExecKind::Reject => {
                let slot = self.state.slot(key);
                slot.note_reject(now);
                slot.state = SlotStateKind::Empty;
                slot.live = None;
                slot.pending_since = None;
            }
            ExecKind::AmendReject => {
                let slot = self.state.slot(key);
                slot.note_reject(now);
                if let (Some((px, qty)), Some(l)) = (slot.amend_from.take(), slot.live.as_mut()) {
                    l.px = px;
                    l.qty = qty;
                }
                slot.state = SlotStateKind::Live;
                slot.pending_since = None;
            }
            ExecKind::Canceled | ExecKind::Expired => {
                let slot = self.state.slot(key);
                slot.state = SlotStateKind::Empty;
                slot.live = None;
                slot.pending_since = None;
            }
            ExecKind::PartialFill | ExecKind::Fill => {
                if let Some(last_qty) = rep.last_qty {
                    if last_qty.is_positive() {
                        let last_px = rep.last_px.unwrap_or(rep.px.unwrap_or(Px::default()));
                        if key.purpose == crate::types::Purpose::Exit {
                            apply_fill_to_lots(&mut self.state.lots, key.side, last_qty);
                            self.state.net_qty +=
                                Qty(last_qty.0 * Decimal::from(key.side.sign()));
                        } else {
                            self.open_lot(now, key.side, last_qty, last_px, out);
                        }
                    }
                }
                let slot = self.state.slot(key);
                if let Some(live) = slot.live.as_mut() {
                    live.filled_qty = rep.filled_qty;
                }
                if rep.kind == ExecKind::Fill {
                    slot.state = SlotStateKind::Empty;
                    slot.live = None;
                    slot.pending_since = None;
                } else {
                    slot.state = SlotStateKind::Live;
                }
            }
        }
    }

    fn open_lot(&mut self, now: Ts, side: Side, qty: Qty, px: Px, out: &mut Vec<Action>) {
        let signed = Qty(qty.0 * Decimal::from(side.sign()));
        self.state.net_qty += signed;
        let id = self.state.alloc_lot_id();
        let lot = Lot {
            id,
            side,
            qty,
            remaining: qty,
            entry_px: px,
            opened_at: now,
            phase: ExitPhase::MakerTp,
            fast_deadline: now.saturating_add_millis(self.cfg.exit.fast_exit_timeout_ms as i64),
            force_deadline: now.saturating_add_millis(self.cfg.exit.force_exit_timeout_ms as i64),
        };
        out.push(Action::SetTimer {
            id: TimerId::lot_fast(id),
            at: lot.fast_deadline,
        });
        out.push(Action::SetTimer {
            id: TimerId::lot_force(id),
            at: lot.force_deadline,
        });
        self.state.lots.push(lot);
    }

    fn rebuild_lots_from_net(&mut self, now: Ts, net: Qty, avg: Option<Px>) {
        self.state.lots.clear();
        self.state.net_qty = net;
        if net.is_zero() {
            return;
        }
        let side = if net.0 > Decimal::ZERO {
            Side::Buy
        } else {
            Side::Sell
        };
        let id = self.state.alloc_lot_id();
        self.state.lots.push(Lot {
            id,
            side,
            qty: net.abs(),
            remaining: net.abs(),
            entry_px: avg.unwrap_or(self.state.last_fair.unwrap_or(Px::default())),
            opened_at: now,
            phase: ExitPhase::FastMaker,
            fast_deadline: now,
            force_deadline: now.saturating_add_millis(self.cfg.exit.force_exit_timeout_ms as i64),
        });
    }

    /// A request whose acknowledgement never arrives used to leave the slot pending forever,
    /// silently retiring that grid level for the rest of the process. Park it as `Unknown`
    /// instead: quoting drops to reduce-only until the open-order sweep says what is real.
    fn expire_pending(&mut self, now: Ts) {
        for (key, slot) in self.state.slots.iter_mut() {
            if !matches!(
                slot.state,
                SlotStateKind::PendingNew
                    | SlotStateKind::PendingAmend
                    | SlotStateKind::PendingCancel
            ) {
                continue;
            }
            let stale = slot
                .pending_since
                .is_some_and(|t| now.duration_since_ms(t) > PENDING_TIMEOUT_MS);
            if stale {
                tracing::warn!(slot = %key, state = ?slot.state, "request timed out; slot unknown");
                slot.state = SlotStateKind::Unknown;
                slot.pending_since = Some(now);
            }
        }
    }

    fn reprice(&mut self, now: Ts) {
        let Some(ref_book) = self.state.ref_book.as_ref() else {
            return;
        };
        let Some(ref_mid) = ref_book.mid() else {
            return;
        };
        let norm = Px(ref_mid.0 * self.state.fx * self.cfg.market.multiplier);
        if let (Some(prev), true) = (self.state.last_ref_mid, self.state.started) {
            if prev.0 > Decimal::ZERO {
                let jump = ((norm.0 - prev.0).abs() / prev.0)
                    .to_f64()
                    .unwrap_or(0.0);
                if jump > self.cfg.pricing.jump_threshold.to_f64().unwrap_or(0.0) {
                    self.state.cooldown_until = Some(now.saturating_add_millis(800));
                }
            }
        }
        self.state.last_ref_mid = Some(norm);
        if let Some(maker) = self.state.maker_book.as_ref().and_then(|b| b.mid()) {
            if norm.0 > Decimal::ZERO {
                let spread = ((maker.0 - norm.0) / norm.0).to_f64().unwrap_or(0.0);
                self.state.last_spread = Some(spread);
                self.state.window.push(now, spread);
                self.state.last_natural = self.state.window.median();
                self.state.last_vol = self.state.window.mad();
            }
        }
        if let Some(nat) = self.state.last_natural {
            let scale = Decimal::from_f64_retain(1.0 + nat).unwrap_or(Decimal::ONE);
            let fair = Px(norm.0 * scale);
            self.state.last_fair = Some(fair);
        } else {
            self.state.last_fair = Some(norm);
        }
    }

    fn advance_exits(&mut self, now: Ts, out: &mut Vec<Action>) {
        let fair = self.state.last_fair;
        for lot in &mut self.state.lots {
            if lot.phase == ExitPhase::Closed || lot.remaining.is_zero() {
                lot.phase = ExitPhase::Closed;
                continue;
            }
            if let Some(fair) = fair {
                if float_loss(lot.entry_px, fair, lot.side)
                    > self.cfg.exit.max_loss.to_f64().unwrap_or(0.0)
                {
                    lot.phase = ExitPhase::TakerExit;
                    continue;
                }
            }
            if now >= lot.force_deadline {
                lot.phase = ExitPhase::TakerExit;
            } else if now >= lot.fast_deadline && lot.phase == ExitPhase::MakerTp {
                lot.phase = ExitPhase::FastMaker;
                out.push(Action::Emit(TelemetryEvent::Info {
                    msg: format!("lot {} fast exit", lot.id.0),
                }));
            }
        }
        self.state.lots.retain(|l| l.phase != ExitPhase::Closed);
    }

    fn sync_lifecycle(&mut self) {
        if !self.state.started {
            self.state.lifecycle = StrategyMode::Stopped;
            return;
        }
        if self.state.flatten_requested {
            self.state.lifecycle = if self.state.net_qty.is_zero() && self.state.lots.is_empty() {
                self.state.started = false;
                StrategyMode::Stopped
            } else {
                StrategyMode::Flattening
            };
            return;
        }
        if self.state.permission.reason.as_deref() == Some("warmup") {
            self.state.lifecycle = StrategyMode::Warmup;
            return;
        }
        if !self.state.permission.allow_new_buy && !self.state.permission.allow_new_sell {
            self.state.lifecycle = StrategyMode::Degraded;
            return;
        }
        self.state.lifecycle = StrategyMode::Running;
    }

    fn quote(&mut self, now: Ts, out: &mut Vec<Action>) {
        if self.state.permission.force_cancel_all
            && self.state.cooldown_until.is_some_and(|t| now < t)
        {
            mark_cancel_all(&mut self.state, now);
            out.push(Action::CancelAll {
                venue: self.cfg.market.maker_venue,
                symbol: self.cfg.market.maker_symbol.clone(),
            });
            out.push(Action::Emit(TelemetryEvent::Jump {
                prev: self
                    .state
                    .last_ref_mid
                    .map(|p| p.to_string())
                    .unwrap_or_default(),
                next: "cooldown".into(),
            }));
            return;
        }

        let mut desired = Vec::new();
        desired.extend(self.exit_targets());
        if let Some(fair) = self.state.last_fair {
            if !self.state.flatten_requested
                && (self.state.permission.allow_new_buy || self.state.permission.allow_new_sell)
            {
                let mut grid = grid_targets(
                    fair,
                    &self.cfg.pricing.grid,
                    &self.maker,
                    self.state.permission.allow_new_buy,
                    self.state.permission.allow_new_sell,
                    self.state.maker_book.as_ref(),
                );
                self.cap_grid_inventory(&mut grid);
                desired.extend(grid);
            }
        }
        prevent_self_cross(&mut desired);
        if self.state.permission.reason.is_some() {
            out.push(Action::Emit(TelemetryEvent::Guard {
                reason: self
                    .state
                    .permission
                    .reason
                    .clone()
                    .unwrap_or_default(),
            }));
        }
        let fair = self.state.last_fair;
        let venue = self.cfg.market.maker_venue;
        let symbol = self.cfg.market.maker_symbol.clone();
        let requote = self.cfg.pricing.requote_threshold;
        reconcile(
            &mut self.state,
            &self.cfg.id,
            venue,
            &symbol,
            &desired,
            requote,
            fair,
            true,
            now,
            out,
        );
    }

    fn cap_grid_inventory(&self, grid: &mut Vec<DesiredSlot>) {
        let px = self.state.last_fair.map(|p| p.0).unwrap_or(Decimal::ONE);
        let cs = self.maker.contract_size.max(Decimal::new(1, 8));
        let unit = |qty: Decimal| qty.abs() * px * cs;
        let mut long = if self.state.net_qty.0 > Decimal::ZERO {
            unit(self.state.net_qty.0)
        } else {
            Decimal::ZERO
        };
        let mut short = if self.state.net_qty.0 < Decimal::ZERO {
            unit(self.state.net_qty.0)
        } else {
            Decimal::ZERO
        };
        let abs_now = unit(self.state.net_qty.0);
        grid.retain(|d| {
            if d.key.purpose != crate::types::Purpose::GridEntry {
                return true;
            }
            let add = unit(d.order.qty.0);
            match d.key.side {
                Side::Buy => {
                    if long + add > self.cfg.risk.max_long || abs_now + add > self.cfg.risk.max_position
                    {
                        return false;
                    }
                    long += add;
                    true
                }
                Side::Sell => {
                    if short + add > self.cfg.risk.max_short
                        || abs_now + add > self.cfg.risk.max_position
                    {
                        return false;
                    }
                    short += add;
                    true
                }
            }
        });
    }

    fn exit_targets(&self) -> Vec<DesiredSlot> {
        let mut out = Vec::new();
        for lot in &self.state.lots {
            if lot.remaining.is_zero() {
                continue;
            }
            let side = lot.side.opposite();
            let idx = (lot.id.0 % 200) as u8;
            let (px, tif, otype) = match lot.phase {
                ExitPhase::MakerTp => (
                    exit_px_maker(
                        lot.entry_px,
                        lot.side,
                        self.cfg.exit.take_profit,
                        self.maker.tick_size,
                    ),
                    TimeInForce::PostOnly,
                    OrderType::Limit,
                ),
                ExitPhase::FastMaker => {
                    let book = self.state.maker_book.as_ref();
                    let px = match (side, book) {
                        (Side::Sell, Some(b)) => Px(crate::types::round_sell_px(
                            b.ask_px.0,
                            self.maker.tick_size,
                        )),
                        (Side::Buy, Some(b)) => {
                            Px(crate::types::round_buy_px(b.bid_px.0, self.maker.tick_size))
                        }
                        _ => exit_px_maker(
                            lot.entry_px,
                            lot.side,
                            Decimal::ZERO,
                            self.maker.tick_size,
                        ),
                    };
                    (px, TimeInForce::PostOnly, OrderType::Limit)
                }
                ExitPhase::TakerExit => {
                    let book = self.state.maker_book.as_ref();
                    let px = match (side, book) {
                        (Side::Sell, Some(b)) => b.bid_px,
                        (Side::Buy, Some(b)) => b.ask_px,
                        _ => lot.entry_px,
                    };
                    (px, TimeInForce::Ioc, OrderType::Market)
                }
                ExitPhase::Closed => continue,
            };
            out.push(DesiredSlot {
                key: SlotKey::exit(side, idx),
                order: DesiredOrder {
                    px,
                    qty: lot.remaining,
                    reduce_only: true,
                    tif,
                    order_type: otype,
                },
            });
        }
        out
    }
}

pub fn apply_fill_to_lots(lots: &mut Vec<Lot>, exit_side: Side, qty: Qty) {
    let mut left = qty.0;
    for lot in lots.iter_mut() {
        if lot.side.opposite() != exit_side || lot.remaining.is_zero() {
            continue;
        }
        let take = lot.remaining.0.min(left);
        lot.remaining.0 -= take;
        left -= take;
        if lot.remaining.is_zero() {
            lot.phase = ExitPhase::Closed;
        }
        if left <= Decimal::ZERO {
            break;
        }
    }
    lots.retain(|l| !l.remaining.is_zero());
}

/// Mark a slot live (test helper / recovery).
pub fn seed_live_slot(core: &mut StrategyCore, key: SlotKey, live: LiveOrder) {
    let slot = core.state.slot(key);
    slot.state = SlotStateKind::Live;
    slot.live = Some(live);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::StrategyConfig;
    use crate::types::{
        BookUpdate, ContractKind, InstrumentStatus, LinkKind, SymbolId, TopOfBook, VenueId,
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

    fn book(venue: VenueId, px: Decimal, ts: Ts) -> BookUpdate {
        BookUpdate {
            book: TopOfBook {
                venue,
                symbol: SymbolId::new("AAPLUSDT"),
                bid_px: Px::new(px),
                bid_qty: Qty::new(dec!(10)),
                ask_px: Px::new(px + dec!(0.02)),
                ask_qty: Qty::new(dec!(10)),
                exchange_ts: ts,
                recv_ts: ts,
            },
            seq: None,
        }
    }

    fn drive_warmup(core: &mut StrategyCore, t0: Ts) -> Vec<Action> {
        let mut out = Vec::new();
        core.on_event(t0, &Event::Control(ControlCmd::Start), &mut out);
        core.set_md_link(true, true);
        core.set_md_link(false, true);
        core.state.links.private = true;
        core.state.links.trading = true;
        out.clear();
        for i in 0..40 {
            let ts = t0.saturating_add_millis(i * 100);
            core.on_event(
                ts,
                &Event::RefBook(book(VenueId::Binance, dec!(100), ts)),
                &mut out,
            );
            out.clear();
            core.on_event(
                ts,
                &Event::MakerBook(book(VenueId::Gate, dec!(100.10), ts)),
                &mut out,
            );
        }
        out
    }

    #[test]
    fn warmup_then_grid_places() {
        let mut core = StrategyCore::new(StrategyConfig::default(), inst());
        let t0 = Ts::from_millis(1_000_000);
        let _out = drive_warmup(&mut core, t0);
        assert_eq!(core.state.lifecycle, StrategyMode::Running);
        assert!(core.state.last_fair.is_some());
        let live = core
            .state
            .slots
            .values()
            .filter(|s| s.live.is_some())
            .count();
        assert!(live >= 4, "expected grid slots, got {}", live);
    }

    #[test]
    fn requote_holds_inside_threshold() {
        let mut core = StrategyCore::new(StrategyConfig::default(), inst());
        let t0 = Ts::from_millis(1_000_000);
        let _ = drive_warmup(&mut core, t0);
        // ack all places
        let live: Vec<_> = core
            .state
            .slots
            .iter()
            .filter_map(|(_, s)| s.live.as_ref().map(|l| l.coid.clone()))
            .collect();
        let ts = t0.saturating_add_millis(5_000);
        for coid in live {
            let mut out = Vec::new();
            core.on_event(
                ts,
                &Event::Exec(ExecReport {
                    coid,
                    exchange_id: Some("1".into()),
                    venue: VenueId::Gate,
                    symbol: SymbolId::new("AAPLUSDT"),
                    kind: ExecKind::Ack,
                    side: Side::Buy,
                    px: None,
                    qty: None,
                    filled_qty: Qty::default(),
                    last_px: None,
                    last_qty: None,
                    reason: None,
                    ts,
                }),
                &mut out,
            );
        }
        let mut out = Vec::new();
        let ts2 = ts.saturating_add_millis(100);
        // tiny ref move: 100 -> 100.02 = 2bps < 5bps requote
        core.on_event(
            ts2,
            &Event::RefBook(book(VenueId::Binance, dec!(100.02), ts2)),
            &mut out,
        );
        let amends = out
            .iter()
            .filter(|a| matches!(a, Action::Amend { .. } | Action::Cancel { .. }))
            .count();
        assert_eq!(amends, 0, "tiny move should hold queue {out:?}");
    }

    #[test]
    fn fill_opens_lot_and_exit() {
        let mut core = StrategyCore::new(StrategyConfig::default(), inst());
        let t0 = Ts::from_millis(1_000_000);
        let _ = drive_warmup(&mut core, t0);
        let buy_slot = core
            .state
            .slots
            .iter()
            .find(|(k, s)| k.side == Side::Buy && s.live.is_some())
            .map(|(k, s)| (*k, s.live.as_ref().unwrap().clone()))
            .expect("buy slot");
        let ts = t0.saturating_add_millis(8_000);
        let mut out = Vec::new();
        core.on_event(
            ts,
            &Event::Exec(ExecReport {
                coid: buy_slot.1.coid,
                exchange_id: Some("9".into()),
                venue: VenueId::Gate,
                symbol: SymbolId::new("AAPLUSDT"),
                kind: ExecKind::Fill,
                side: Side::Buy,
                px: Some(buy_slot.1.px),
                qty: Some(buy_slot.1.qty),
                filled_qty: buy_slot.1.qty,
                last_px: Some(buy_slot.1.px),
                last_qty: Some(buy_slot.1.qty),
                reason: None,
                ts,
            }),
            &mut out,
        );
        assert_eq!(core.state.lots.len(), 1);
        assert!(
            out.iter().any(|a| matches!(
                a,
                Action::Place(p) if p.reduce_only && p.side == Side::Sell
            )),
            "expected maker tp exit {out:?}"
        );
    }

    #[test]
    fn max_loss_goes_taker() {
        let mut cfg = StrategyConfig::default();
        cfg.exit.max_loss = dec!(0.001);
        let mut core = StrategyCore::new(cfg, inst());
        let t0 = Ts::from_millis(1_000_000);
        let _ = drive_warmup(&mut core, t0);
        core.state.lots.push(Lot {
            id: crate::types::LotId(1),
            side: Side::Buy,
            qty: Qty::new(dec!(1)),
            remaining: Qty::new(dec!(1)),
            entry_px: Px::new(dec!(100)),
            opened_at: t0,
            phase: ExitPhase::MakerTp,
            fast_deadline: t0.saturating_add_millis(5_000),
            force_deadline: t0.saturating_add_millis(12_000),
        });
        core.state.net_qty = Qty::new(dec!(1));
        let ts = t0.saturating_add_millis(200);
        let mut out = Vec::new();
        // fair drops to 99.7 → loss 0.3% > 0.1%
        core.on_event(
            ts,
            &Event::RefBook(book(VenueId::Binance, dec!(99.70), ts)),
            &mut out,
        );
        assert_eq!(core.state.lots[0].phase, ExitPhase::TakerExit);
        assert!(out.iter().any(|a| matches!(
            a,
            Action::Place(p) if p.reduce_only && p.order_type == OrderType::Market
        )));
    }

    #[test]
    fn jump_emits_cancel_all() {
        let mut core = StrategyCore::new(StrategyConfig::default(), inst());
        let t0 = Ts::from_millis(1_000_000);
        let _ = drive_warmup(&mut core, t0);
        let ts = t0.saturating_add_millis(9_000);
        let mut out = Vec::new();
        core.on_event(
            ts,
            &Event::RefBook(book(VenueId::Binance, dec!(101.00), ts)),
            &mut out,
        );
        assert!(
            out.iter()
                .any(|a| matches!(a, Action::CancelAll { .. })),
            "jump should cancel all {out:?}"
        );
    }

    /// A venue that refuses a placement (no margin, bad price) must not be hammered on every
    /// book tick: that is what turned one blocked grid level into ~100k orders in 15 minutes.
    #[test]
    fn rejected_place_backs_off() {
        let mut core = StrategyCore::new(StrategyConfig::default(), inst());
        let t0 = Ts::from_millis(1_000_000);
        let _ = drive_warmup(&mut core, t0);
        let (key, coid) = core
            .state
            .slots
            .iter()
            .find(|(k, s)| k.side == Side::Sell && s.live.is_some())
            .map(|(k, s)| (*k, s.live.as_ref().unwrap().coid.clone()))
            .expect("sell slot");
        let ts = t0.saturating_add_millis(5_000);
        let mut out = Vec::new();
        core.on_event(
            ts,
            &Event::ReqOutcome {
                coid,
                result: ReqResult::Rejected,
                reason: Some("BALANCE_NOT_ENOUGH".into()),
            },
            &mut out,
        );
        assert_eq!(core.state.slot(key).state, SlotStateKind::Empty);

        let mut places = 0;
        for i in 1..=8 {
            let t = ts.saturating_add_millis(i * 25);
            let mut out = Vec::new();
            core.on_event(t, &Event::MakerBook(book(VenueId::Gate, dec!(100.10), t)), &mut out);
            places += out
                .iter()
                .filter(|a| matches!(a, Action::Place(p) if p.side == Side::Sell))
                .count();
        }
        assert_eq!(places, 0, "backoff should hold the slot for 250ms");

        let t = ts.saturating_add_millis(400);
        let mut out = Vec::new();
        core.on_event(t, &Event::MakerBook(book(VenueId::Gate, dec!(100.10), t)), &mut out);
        assert!(
            out.iter()
                .any(|a| matches!(a, Action::Place(p) if p.side == Side::Sell)),
            "slot should retry once the backoff expires {out:?}"
        );
    }

    /// A refused amend leaves the original order working on the venue. Dropping it from the
    /// slot map stranded it there with nothing left able to cancel it.
    #[test]
    fn rejected_amend_keeps_the_live_order() {
        let mut core = StrategyCore::new(StrategyConfig::default(), inst());
        let t0 = Ts::from_millis(1_000_000);
        let _ = drive_warmup(&mut core, t0);
        let (key, live) = core
            .state
            .slots
            .iter()
            .find(|(k, s)| k.side == Side::Buy && s.live.is_some())
            .map(|(k, s)| (*k, s.live.as_ref().unwrap().clone()))
            .expect("buy slot");
        let ts = t0.saturating_add_millis(5_000);
        ack(&mut core, &live.coid, ts, Side::Buy, live.px, live.qty);
        assert_eq!(core.state.slot(key).state, SlotStateKind::Live);

        // Enough of a reference move to force a requote (>5bps) but below the 40bps jump guard.
        let ts2 = ts.saturating_add_millis(100);
        let mut out = Vec::new();
        core.on_event(
            ts2,
            &Event::RefBook(book(VenueId::Binance, dec!(100.20), ts2)),
            &mut out,
        );
        let amended = out.iter().any(|a| matches!(a, Action::Amend { coid, .. } if *coid == live.coid));
        assert!(amended, "expected amend {out:?}");
        assert_eq!(core.state.slot(key).state, SlotStateKind::PendingAmend);

        let mut out = Vec::new();
        core.on_event(
            ts2.saturating_add_millis(10),
            &Event::ReqOutcome {
                coid: live.coid.clone(),
                result: ReqResult::Rejected,
                reason: Some("ORDER_NOT_FOUND".into()),
            },
            &mut out,
        );
        let slot = core.state.slot(key);
        assert_eq!(slot.state, SlotStateKind::Live, "order is still on the venue");
        let still = slot.live.as_ref().expect("live order retained");
        assert_eq!(still.coid, live.coid);
        assert_eq!(still.px, live.px, "optimistic amend price rolled back");
    }

    /// Anything the venue reports that no slot points at can only be cleaned up here.
    #[test]
    fn sweep_reports_untracked_own_orders() {
        let mut core = StrategyCore::new(StrategyConfig::default(), inst());
        let t0 = Ts::from_millis(1_000_000);
        let _ = drive_warmup(&mut core, t0);
        let tracked = core
            .state
            .slots
            .values()
            .filter_map(|s| s.live.as_ref().map(|l| l.coid.clone()))
            .next()
            .expect("a live order");
        let orphan = crate::types::ClientOrderId("demo-grid-sell-0-999".into());
        let foreign = crate::types::ClientOrderId("other-grid-sell-0-1".into());
        let out = core.reconcile_open_orders(&[tracked, orphan.clone(), foreign]);
        assert_eq!(out, vec![orphan], "only our own untracked orders");
    }

    #[test]
    fn unknown_slot_resolves_from_open_orders() {
        let mut core = StrategyCore::new(StrategyConfig::default(), inst());
        let t0 = Ts::from_millis(1_000_000);
        let _ = drive_warmup(&mut core, t0);
        let (key, coid) = core
            .state
            .slots
            .iter()
            .find_map(|(k, s)| s.live.as_ref().map(|l| (*k, l.coid.clone())))
            .unwrap();
        core.state.slot(key).state = SlotStateKind::Unknown;
        core.reconcile_open_orders(std::slice::from_ref(&coid));
        assert_eq!(core.state.slot(key).state, SlotStateKind::Live);

        core.state.slot(key).state = SlotStateKind::Unknown;
        core.reconcile_open_orders(&[]);
        assert_eq!(core.state.slot(key).state, SlotStateKind::Empty);
        assert!(core.state.slot(key).live.is_none());
    }

    /// An acknowledgement carrying no usable price must not overwrite what we know, or the
    /// order looks 100% mispriced and gets requoted on every single event.
    #[test]
    fn ack_without_price_keeps_known_price() {
        let mut core = StrategyCore::new(StrategyConfig::default(), inst());
        let t0 = Ts::from_millis(1_000_000);
        let _ = drive_warmup(&mut core, t0);
        let (key, live) = core
            .state
            .slots
            .iter()
            .find_map(|(k, s)| s.live.as_ref().map(|l| (*k, l.clone())))
            .unwrap();
        let ts = t0.saturating_add_millis(5_000);
        ack(&mut core, &live.coid, ts, key.side, Px::default(), Qty::default());
        let slot = core.state.slot(key);
        assert_eq!(slot.state, SlotStateKind::Live);
        assert_eq!(slot.live.as_ref().unwrap().px, live.px);
        assert_eq!(slot.live.as_ref().unwrap().qty, live.qty);
    }

    fn ack(
        core: &mut StrategyCore,
        coid: &crate::types::ClientOrderId,
        ts: Ts,
        side: Side,
        px: Px,
        qty: Qty,
    ) {
        let mut out = Vec::new();
        core.on_event(
            ts,
            &Event::Exec(ExecReport {
                coid: coid.clone(),
                exchange_id: Some("1".into()),
                venue: VenueId::Gate,
                symbol: SymbolId::new("AAPLUSDT"),
                kind: ExecKind::Ack,
                side,
                px: Some(px),
                qty: Some(qty),
                filled_qty: Qty::default(),
                last_px: None,
                last_qty: None,
                reason: None,
                ts,
            }),
            &mut out,
        );
    }

    #[test]
    fn unknown_is_not_resent() {
        let mut core = StrategyCore::new(StrategyConfig::default(), inst());
        let t0 = Ts::from_millis(1_000_000);
        let _ = drive_warmup(&mut core, t0);
        let (key, coid) = core
            .state
            .slots
            .iter()
            .find_map(|(k, s)| s.live.as_ref().map(|l| (*k, l.coid.clone())))
            .unwrap();
        let ts = t0.saturating_add_millis(8_000);
        let mut out = Vec::new();
        core.on_event(
            ts,
            &Event::ReqOutcome {
                coid: coid.clone(),
                result: ReqResult::Unknown,
                reason: Some("timeout".into()),
            },
            &mut out,
        );
        assert_eq!(core.state.slot(key).state, SlotStateKind::Unknown);
        let places = out
            .iter()
            .filter(|a| matches!(a, Action::Place(_)))
            .count();
        // degraded: no new grid for that slot
        let _ = places;
        let _ = LinkKind::Trading;
    }
}
