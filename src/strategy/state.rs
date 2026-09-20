use std::collections::HashMap;

use rust_decimal::Decimal;

use crate::types::{
    ClientOrderId, DesiredOrder, LinkStatus, LiveOrder, Lot, LotId, Px, Qty, QuotePermission,
    SlotKey, SlotStateKind, StrategyMode, TopOfBook, Ts,
};

use super::rolling::RollingMedian;

#[derive(Clone, Debug)]
pub struct Slot {
    pub state: SlotStateKind,
    pub live: Option<LiveOrder>,
    pub desired: Option<DesiredOrder>,
    pub pending_since: Option<Ts>,
}

impl Slot {
    pub fn empty() -> Self {
        Self {
            state: SlotStateKind::Empty,
            live: None,
            desired: None,
            pending_since: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct CoreState {
    pub lifecycle: StrategyMode,
    pub started: bool,
    pub ref_book: Option<TopOfBook>,
    pub maker_book: Option<TopOfBook>,
    pub fx: Decimal,
    pub window: RollingMedian,
    pub last_fair: Option<Px>,
    pub last_ref_mid: Option<Px>,
    pub last_spread: Option<f64>,
    pub last_natural: Option<f64>,
    pub last_vol: Option<f64>,
    pub cooldown_until: Option<Ts>,
    pub slots: HashMap<SlotKey, Slot>,
    pub lots: Vec<Lot>,
    pub next_lot: u64,
    pub next_seq: u64,
    pub net_qty: Qty,
    pub links: LinkStatus,
    pub permission: QuotePermission,
    pub flatten_requested: bool,
    pub last_unknown_count: usize,
}

impl CoreState {
    pub fn new(window_ms: u64, bucket_ms: u64, fx: Decimal) -> Self {
        Self {
            lifecycle: StrategyMode::Init,
            started: false,
            ref_book: None,
            maker_book: None,
            fx,
            window: RollingMedian::new(window_ms, bucket_ms),
            last_fair: None,
            last_ref_mid: None,
            last_spread: None,
            last_natural: None,
            last_vol: None,
            cooldown_until: None,
            slots: HashMap::new(),
            lots: Vec::new(),
            next_lot: 1,
            next_seq: 1,
            net_qty: Qty::default(),
            links: LinkStatus::default(),
            permission: QuotePermission::reduce_only("init"),
            flatten_requested: false,
            last_unknown_count: 0,
        }
    }

    pub fn alloc_seq(&mut self) -> u64 {
        let s = self.next_seq;
        self.next_seq += 1;
        s
    }

    pub fn alloc_lot_id(&mut self) -> LotId {
        let id = LotId(self.next_lot);
        self.next_lot += 1;
        id
    }

    pub fn slot(&mut self, key: SlotKey) -> &mut Slot {
        self.slots.entry(key).or_insert_with(Slot::empty)
    }

    pub fn find_slot_by_coid(&self, coid: &ClientOrderId) -> Option<SlotKey> {
        self.slots.iter().find_map(|(k, s)| {
            s.live
                .as_ref()
                .filter(|l| l.coid == *coid)
                .map(|_| *k)
        })
    }

    pub fn unknown_slots(&self) -> usize {
        self.slots
            .values()
            .filter(|s| s.state == SlotStateKind::Unknown)
            .count()
    }

    pub fn pending_slots(&self) -> usize {
        self.slots.iter().filter(|(_, s)| {
            matches!(
                s.state,
                SlotStateKind::PendingNew
                    | SlotStateKind::PendingAmend
                    | SlotStateKind::PendingCancel
                    | SlotStateKind::Unknown
            )
        }).count()
    }
}
