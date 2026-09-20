use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::Result;
use parking_lot::RwLock;
use tokio::sync::{broadcast, mpsc};
use tracing::{info, warn};

use crate::config::{AppConfig, StrategyConfig};
use crate::exchange::binance::BinanceVenue;
use crate::exchange::gate::GateVenue;
use crate::exchange::{PrivateMsg, ReqOutcome, VenueApi, VenueHealth};
use crate::marketdata::{Catalog, rank_recommended};
use crate::risk::GlobalRisk;
use crate::store::Store;
use crate::strategy::{StrategyCore, StrategyCoreApi};
use crate::types::{
    Action, BookUpdate, ControlCmd, Event, FillRecord, Instrument, LinkKind, OrderRecord, Qty,
    RunMode, StateSnapshot, StrategyId, StrategyMode, SymbolId, TelemetryEvent, TimerId, Ts,
    VenueId,
};

#[derive(Clone, Debug, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsMsg {
    Snapshot { snapshot: StateSnapshot },
    Health { venues: Vec<VenueHealth> },
    Risk { risk: crate::risk::GlobalRiskView },
    Log { strategy_id: String, msg: String },
}

pub struct Engine {
    pub cfg: AppConfig,
    pub store: Store,
    pub catalog: RwLock<Catalog>,
    pub snapshots: RwLock<HashMap<String, StateSnapshot>>,
    pub health: RwLock<HashMap<VenueId, VenueHealth>>,
    pub risk: GlobalRisk,
    pub ws: broadcast::Sender<WsMsg>,
    latest_books: RwLock<HashMap<(VenueId, String), BookUpdate>>,
    books: HashMap<VenueId, broadcast::Sender<BookUpdate>>,
    private: HashMap<VenueId, broadcast::Sender<PrivateMsg>>,
    outcomes: HashMap<VenueId, broadcast::Sender<ReqOutcome>>,
    trade: HashMap<VenueId, mpsc::UnboundedSender<Action>>,
    apis: HashMap<VenueId, Arc<dyn VenueApi>>,
    actors: tokio::sync::Mutex<HashMap<String, ActorHandle>>,
    md_symbols: RwLock<HashSet<String>>,
}

struct ActorHandle {
    ctrl: mpsc::UnboundedSender<ControlCmd>,
    cancel: tokio::sync::watch::Sender<bool>,
}

impl Engine {
    pub async fn start(cfg: AppConfig, store: Store) -> Result<Arc<Self>> {
        let (ws, _) = broadcast::channel(256);
        let mut books = HashMap::new();
        let mut private = HashMap::new();
        let mut outcomes = HashMap::new();
        let mut trade = HashMap::new();
        let mut apis: HashMap<VenueId, Arc<dyn VenueApi>> = HashMap::new();

        let bn = Arc::new(BinanceVenue::new(cfg.trading_env, &cfg.venues)?);
        let gt = Arc::new(GateVenue::new(cfg.trading_env, &cfg.venues)?);

        for venue in VenueId::all() {
            let (btx, _) = broadcast::channel(1024);
            books.insert(venue, btx);
            let (ptx, _) = broadcast::channel(1024);
            private.insert(venue, ptx);
            let (otx, _) = broadcast::channel(256);
            outcomes.insert(venue, otx);
        }

        let (bn_trade_tx, bn_trade_rx) = mpsc::unbounded_channel();
        let (gt_trade_tx, gt_trade_rx) = mpsc::unbounded_channel();
        trade.insert(VenueId::Binance, bn_trade_tx);
        trade.insert(VenueId::Gate, gt_trade_tx);

        apis.insert(VenueId::Binance, bn.clone());
        apis.insert(VenueId::Gate, gt.clone());

        let engine = Arc::new(Self {
            risk: GlobalRisk::new(cfg.global_risk.clone()),
            cfg,
            store,
            catalog: RwLock::new(Catalog::default()),
            snapshots: RwLock::new(HashMap::new()),
            health: RwLock::new(HashMap::new()),
            ws,
            latest_books: RwLock::new(HashMap::new()),
            books,
            private,
            outcomes,
            trade,
            apis,
            actors: tokio::sync::Mutex::new(HashMap::new()),
            md_symbols: RwLock::new(HashSet::new()),
        });

        engine.refresh_catalog().await;
        engine.spawn_book_cache();
        engine.spawn_venue_links(&bn, &gt, bn_trade_rx, gt_trade_rx);
        engine.spawn_md_for_universe().await;
        Ok(engine)
    }

    fn spawn_book_cache(self: &Arc<Self>) {
        for venue in VenueId::all() {
            let mut rx = self.books[&venue].subscribe();
            let this = Arc::clone(self);
            tokio::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(book) => {
                            this.latest_books.write().insert(
                                (book.book.venue, book.book.symbol.as_str().to_string()),
                                book,
                            );
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(_) => break,
                    }
                }
            });
        }
    }

    fn spawn_venue_links(
        self: &Arc<Self>,
        bn: &Arc<BinanceVenue>,
        gt: &Arc<GateVenue>,
        bn_trade_rx: mpsc::UnboundedReceiver<Action>,
        gt_trade_rx: mpsc::UnboundedReceiver<Action>,
    ) {
        let (bn_md_l, mut bn_md_r) = mpsc::unbounded_channel();
        let (gt_md_l, mut gt_md_r) = mpsc::unbounded_channel();
        let (bn_pv_l, mut bn_pv_r) = mpsc::unbounded_channel();
        let (gt_pv_l, mut gt_pv_r) = mpsc::unbounded_channel();
        let (bn_tr_l, mut bn_tr_r) = mpsc::unbounded_channel();
        let (gt_tr_l, mut gt_tr_r) = mpsc::unbounded_channel();

        bn.spawn_private(self.private[&VenueId::Binance].clone(), bn_pv_l);
        gt.spawn_private(self.private[&VenueId::Gate].clone(), gt_pv_l);
        bn.spawn_trading(
            bn_trade_rx,
            self.outcomes[&VenueId::Binance].clone(),
            bn_tr_l,
        );
        gt.spawn_trading(gt_trade_rx, self.outcomes[&VenueId::Gate].clone(), gt_tr_l);

        let this = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    Some(up) = bn_md_r.recv() => this.set_health(VenueId::Binance, LinkKind::MarketData, up),
                    Some(up) = gt_md_r.recv() => this.set_health(VenueId::Gate, LinkKind::MarketData, up),
                    Some(up) = bn_pv_r.recv() => this.set_health(VenueId::Binance, LinkKind::Private, up),
                    Some(up) = gt_pv_r.recv() => this.set_health(VenueId::Gate, LinkKind::Private, up),
                    Some(up) = bn_tr_r.recv() => this.set_health(VenueId::Binance, LinkKind::Trading, up),
                    Some(up) = gt_tr_r.recv() => this.set_health(VenueId::Gate, LinkKind::Trading, up),
                    else => break,
                }
            }
        });

        // keep md link senders alive via engine-owned tasks that re-spawn md
        let _ = (bn_md_l, gt_md_l);
        let this = Arc::clone(self);
        let bn = Arc::clone(bn);
        let gt = Arc::clone(gt);
        tokio::spawn(async move {
            // dummy keep: health defaults
            this.set_health(VenueId::Binance, LinkKind::MarketData, false);
            this.set_health(VenueId::Gate, LinkKind::MarketData, false);
            this.set_health(VenueId::Binance, LinkKind::Private, bn.has_keys());
            this.set_health(VenueId::Gate, LinkKind::Private, gt.has_keys());
            this.set_health(VenueId::Binance, LinkKind::Trading, bn.has_keys());
            this.set_health(VenueId::Gate, LinkKind::Trading, gt.has_keys());
        });
    }

    async fn spawn_md_for_universe(self: &Arc<Self>) {
        let rec = self.catalog.read().recommended.clone();
        let mut symbols: Vec<SymbolId> = rec;
        if let Ok(strats) = self.store.list_strategies().await {
            for s in strats {
                symbols.push(s.market.maker_symbol.clone());
                symbols.push(s.market.ref_symbol.clone());
            }
        }
        symbols.sort_by(|a, b| a.0.cmp(&b.0));
        symbols.dedup();
        if symbols.is_empty() {
            symbols = crate::config::default_underlyings()
                .into_iter()
                .map(|u| SymbolId::new(format!("{u}USDT")))
                .collect();
        }
        self.subscribe_md(symbols);
    }

    pub fn subscribe_md(self: &Arc<Self>, symbols: Vec<SymbolId>) {
        let mut guard = self.md_symbols.write();
        let mut fresh = Vec::new();
        for s in symbols {
            if guard.insert(s.0.clone()) {
                fresh.push(s);
            }
        }
        drop(guard);
        if fresh.is_empty() {
            return;
        }
        let all: Vec<SymbolId> = self
            .md_symbols
            .read()
            .iter()
            .cloned()
            .map(SymbolId)
            .collect();
        info!(n = all.len(), "subscribing market data");
        if let Some(bn) = self.apis.get(&VenueId::Binance) {
            let _ = bn;
        }
        // spawn dedicated connections for the union
        if let Ok(bn) = BinanceVenue::new(self.cfg.trading_env, &self.cfg.venues) {
            let (l, mut r) = mpsc::unbounded_channel();
            bn.spawn_market_data(all.clone(), self.books[&VenueId::Binance].clone(), l);
            let this = Arc::clone(self);
            tokio::spawn(async move {
                while let Some(up) = r.recv().await {
                    this.set_health(VenueId::Binance, LinkKind::MarketData, up);
                }
            });
        }
        if let Ok(gt) = GateVenue::new(self.cfg.trading_env, &self.cfg.venues) {
            let (l, mut r) = mpsc::unbounded_channel();
            gt.spawn_market_data(all, self.books[&VenueId::Gate].clone(), l);
            let this = Arc::clone(self);
            tokio::spawn(async move {
                while let Some(up) = r.recv().await {
                    this.set_health(VenueId::Gate, LinkKind::MarketData, up);
                }
            });
        }
    }

    pub async fn refresh_catalog(&self) {
        let mut by_venue = HashMap::new();
        let mut volumes = HashMap::new();
        for (venue, api) in &self.apis {
            match api.list_instruments().await {
                Ok(list) => {
                    info!(%venue, n = list.len(), "loaded instruments");
                    by_venue.insert(*venue, list);
                }
                Err(e) => warn!(%venue, error = %e, "instrument catalog failed"),
            }
            match api.volumes().await {
                Ok(v) => {
                    volumes.insert(*venue, v);
                }
                Err(e) => warn!(%venue, error = %e, "volume fetch failed"),
            }
            if let Err(e) = api.ensure_oneway().await {
                warn!(%venue, error = %e, "ensure oneway / account mode failed");
            }
        }
        let recommended = rank_recommended(&self.cfg, &by_venue, &volumes);
        *self.catalog.write() = Catalog {
            by_venue,
            recommended,
        };
    }

    fn set_health(&self, venue: VenueId, kind: LinkKind, up: bool) {
        let mut h = self.health.write();
        let e = h.entry(venue).or_insert(VenueHealth {
            venue,
            md: false,
            private: false,
            trading: false,
            has_keys: match venue {
                VenueId::Binance => self.cfg.venues.has_binance(),
                VenueId::Gate => self.cfg.venues.has_gate(),
            },
        });
        match kind {
            LinkKind::MarketData => e.md = up,
            LinkKind::Private => e.private = up,
            LinkKind::Trading => e.trading = up,
        }
        let venues: Vec<_> = h.values().cloned().collect();
        drop(h);
        let _ = self.ws.send(WsMsg::Health { venues });
    }

    pub fn health_list(&self) -> Vec<VenueHealth> {
        self.health.read().values().cloned().collect()
    }

    pub fn snapshot_list(&self) -> Vec<StateSnapshot> {
        self.snapshots.read().values().cloned().collect()
    }

    pub fn snapshot(&self, id: &str) -> Option<StateSnapshot> {
        self.snapshots.read().get(id).cloned()
    }

    pub async fn upsert_strategy(self: &Arc<Self>, cfg: StrategyConfig) -> Result<()> {
        cfg.validate()?;
        self.store.upsert_strategy(&cfg).await?;
        self.subscribe_md(vec![
            cfg.market.maker_symbol.clone(),
            cfg.market.ref_symbol.clone(),
        ]);
        self.snapshots.write().insert(
            cfg.id.as_str().to_string(),
            idle_snapshot(&cfg, self.instrument_for(&cfg)),
        );
        Ok(())
    }

    pub async fn delete_strategy(&self, id: &StrategyId) -> Result<bool> {
        self.stop_strategy(id).await;
        self.snapshots.write().remove(id.as_str());
        self.store.delete_strategy(id).await
    }

    pub async fn start_strategy(self: &Arc<Self>, id: &StrategyId) -> Result<()> {
        let cfg = self
            .store
            .get_strategy(id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("strategy not found"))?;
        self.spawn_actor(cfg).await
    }

    pub async fn stop_strategy(&self, id: &StrategyId) {
        let mut actors = self.actors.lock().await;
        if let Some(h) = actors.remove(id.as_str()) {
            let _ = h.ctrl.send(ControlCmd::Stop);
            let _ = h.cancel.send(true);
        }
    }

    pub async fn flatten_strategy(&self, id: &StrategyId) {
        let actors = self.actors.lock().await;
        if let Some(h) = actors.get(id.as_str()) {
            let _ = h.ctrl.send(ControlCmd::Flatten);
        }
    }

    pub async fn kill_all(&self) {
        self.risk.set_kill(true);
        let actors = self.actors.lock().await;
        for h in actors.values() {
            let _ = h.ctrl.send(ControlCmd::Flatten);
        }
        let snaps = self.snapshot_list();
        let _ = self.ws.send(WsMsg::Risk {
            risk: self.risk.view(&snaps),
        });
    }

    pub async fn clear_kill(&self) {
        self.risk.set_kill(false);
    }

    fn instrument_for(&self, cfg: &StrategyConfig) -> Instrument {
        self.catalog
            .read()
            .find(cfg.market.maker_venue, &cfg.market.maker_symbol)
            .unwrap_or_else(|| dummy_instrument(cfg.market.maker_venue, &cfg.market.maker_symbol))
    }

    async fn spawn_actor(self: &Arc<Self>, cfg: StrategyConfig) -> Result<()> {
        let id = cfg.id.as_str().to_string();
        {
            let mut actors = self.actors.lock().await;
            if actors.contains_key(&id) {
                return Ok(());
            }
            let (ctrl_tx, ctrl_rx) = mpsc::unbounded_channel();
            let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
            actors.insert(
                id.clone(),
                ActorHandle {
                    ctrl: ctrl_tx,
                    cancel: cancel_tx,
                },
            );
            let inst = self.instrument_for(&cfg);
            let this = Arc::clone(self);
            tokio::spawn(async move {
                run_actor(this, cfg, inst, ctrl_rx, cancel_rx).await;
            });
        }
        Ok(())
    }
}

fn dummy_instrument(venue: VenueId, symbol: &SymbolId) -> Instrument {
    Instrument {
        venue,
        symbol: symbol.clone(),
        native_symbol: crate::exchange::native_symbol(venue, symbol.as_str()),
        tick_size: rust_decimal::Decimal::new(1, 2),
        lot_size: rust_decimal::Decimal::new(1, 2),
        contract_size: rust_decimal::Decimal::ONE,
        min_qty: rust_decimal::Decimal::new(1, 2),
        min_notional: rust_decimal::Decimal::ZERO,
        quote_ccy: "USDT".into(),
        maker_fee: rust_decimal::Decimal::new(2, 4),
        taker_fee: rust_decimal::Decimal::new(5, 4),
        kind: crate::types::ContractKind::Stock,
        status: crate::types::InstrumentStatus::Trading,
        volume_24h: rust_decimal::Decimal::ZERO,
    }
}

fn idle_snapshot(cfg: &StrategyConfig, _inst: Instrument) -> StateSnapshot {
    StateSnapshot {
        strategy_id: cfg.id.clone(),
        name: cfg.name.clone(),
        lifecycle: StrategyMode::Init,
        run_mode: cfg.mode,
        maker_venue: cfg.market.maker_venue,
        maker_symbol: cfg.market.maker_symbol.clone(),
        ref_venue: cfg.market.ref_venue,
        ref_symbol: cfg.market.ref_symbol.clone(),
        fair_price: None,
        natural_spread: None,
        spread_vol: None,
        spread_t: None,
        ref_book: None,
        maker_book: None,
        permission: crate::types::QuotePermission::reduce_only("idle"),
        slots: vec![],
        lots: vec![],
        net_qty: Qty::default(),
        links: crate::types::LinkStatus::default(),
        warmup_samples: 0,
        warmup_needed: cfg.pricing.min_samples,
        enabled: cfg.enabled,
    }
}

async fn run_actor(
    engine: Arc<Engine>,
    cfg: StrategyConfig,
    inst: Instrument,
    mut ctrl: mpsc::UnboundedReceiver<ControlCmd>,
    mut cancel: tokio::sync::watch::Receiver<bool>,
) {
    let sid = cfg.id.clone();
    let maker_venue = cfg.market.maker_venue;
    let ref_venue = cfg.market.ref_venue;
    let maker_sym = cfg.market.maker_symbol.clone();
    let ref_sym = cfg.market.ref_symbol.clone();
    let live = cfg.mode == RunMode::Live && !engine.risk.is_killed();

    let mut core = StrategyCore::new(cfg.clone(), inst);
    {
        let health = engine.health.read();
        if let Some(h) = health.get(&ref_venue) {
            core.set_md_link(true, h.md);
        }
        if let Some(h) = health.get(&maker_venue) {
            core.set_md_link(false, h.md);
        }
    }
    let mut out = Vec::new();
    core.on_event(Ts::now_system(), &Event::Control(ControlCmd::Start), &mut out);
    out.clear();

    if let Some(api) = engine.apis.get(&maker_venue) {
        if let Ok(pos) = api.positions().await {
            for p in pos {
                if p.symbol == maker_sym {
                    core.on_event(Ts::now_system(), &Event::PositionSync(p), &mut out);
                    out.clear();
                }
            }
        }
        if let Ok(orders) = api.open_orders(&maker_sym).await {
            for o in orders {
                if o.coid.strategy_prefix() != Some(sid.as_str()) {
                    if live {
                        if let Some(tx) = engine.trade.get(&maker_venue) {
                            let _ = tx.send(Action::Cancel {
                                coid: o.coid,
                                venue: maker_venue,
                                symbol: maker_sym.clone(),
                            });
                        }
                    }
                }
            }
        }
    }

    let mut books_r = engine.books[&ref_venue].subscribe();
    let mut books_m = engine.books[&maker_venue].subscribe();
    let mut priv_r = engine.private[&maker_venue].subscribe();
    let mut out_r = engine.outcomes[&maker_venue].subscribe();
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(250));

    info!(strategy = %sid, "actor started");
    loop {
        if *cancel.borrow() {
            break;
        }
        tokio::select! {
            _ = cancel.changed() => {
                if *cancel.borrow() { break; }
            }
            cmd = ctrl.recv() => {
                let Some(cmd) = cmd else { break; };
                dispatch(&mut core, Event::Control(cmd), &engine, live, &mut out).await;
                if matches!(core.state.lifecycle, StrategyMode::Stopped) {
                    break;
                }
            }
            Ok(book) = books_r.recv() => {
                if book.book.symbol == ref_sym {
                    core.set_md_link(true, true);
                    dispatch(&mut core, Event::RefBook(book), &engine, live, &mut out).await;
                }
            }
            Ok(book) = books_m.recv() => {
                if book.book.symbol == maker_sym {
                    core.set_md_link(false, true);
                    dispatch(&mut core, Event::MakerBook(book), &engine, live, &mut out).await;
                }
            }
            Ok(msg) = priv_r.recv() => {
                match msg {
                    PrivateMsg::Exec(e) => {
                        persist_exec(&engine, &sid, &e).await;
                        dispatch(&mut core, Event::Exec(e), &engine, live, &mut out).await;
                    }
                    PrivateMsg::Position(p) => {
                        if p.symbol == maker_sym {
                            dispatch(&mut core, Event::PositionSync(p), &engine, live, &mut out).await;
                        }
                    }
                    PrivateMsg::Link { kind, up } => {
                        dispatch(&mut core, Event::Link { venue: maker_venue, kind, up }, &engine, live, &mut out).await;
                    }
                }
            }
            Ok(oc) = out_r.recv() => {
                dispatch(&mut core, Event::ReqOutcome { coid: oc.coid, result: oc.result, reason: oc.reason }, &engine, live, &mut out).await;
            }
            _ = tick.tick() => {
                let ref_book = engine
                    .latest_books
                    .read()
                    .get(&(ref_venue, ref_sym.as_str().to_string()))
                    .cloned();
                let maker_book = engine
                    .latest_books
                    .read()
                    .get(&(maker_venue, maker_sym.as_str().to_string()))
                    .cloned();
                if let Some(book) = ref_book {
                    core.set_md_link(true, true);
                    dispatch(&mut core, Event::RefBook(book), &engine, live, &mut out).await;
                }
                if let Some(book) = maker_book {
                    core.set_md_link(false, true);
                    dispatch(&mut core, Event::MakerBook(book), &engine, live, &mut out).await;
                }
                dispatch(&mut core, Event::Timer(TimerId::EXIT_SCAN), &engine, live, &mut out).await;
                if engine.risk.is_killed() && !core.state.flatten_requested {
                    dispatch(&mut core, Event::Control(ControlCmd::Flatten), &engine, live, &mut out).await;
                }
            }
        }
    }
    publish_snap(&engine, &core);
    info!(strategy = %sid, "actor stopped");
}

async fn dispatch(
    core: &mut StrategyCore,
    ev: Event,
    engine: &Arc<Engine>,
    live: bool,
    out: &mut Vec<Action>,
) {
    out.clear();
    core.on_event(Ts::now_system(), &ev, out);
    let actions = std::mem::take(out);
    for a in actions {
        match a {
            Action::Place(_) | Action::Cancel { .. } | Action::Amend { .. } | Action::CancelAll { .. }
                if live =>
            {
                if let Some(tx) = engine.trade.get(&core.cfg.market.maker_venue) {
                    persist_action(engine, &core.cfg.id, &a).await;
                    let _ = tx.send(a);
                }
            }
            Action::Emit(TelemetryEvent::Guard { reason }) => {
                let _ = engine.ws.send(WsMsg::Log {
                    strategy_id: core.cfg.id.as_str().into(),
                    msg: reason,
                });
            }
            Action::Emit(other) => {
                let _ = engine.store.append_journal(
                    Some(core.cfg.id.as_str()),
                    "telemetry",
                    &serde_json::to_value(other).unwrap_or_default(),
                );
            }
            _ => {}
        }
    }
    publish_snap(engine, core);
}

fn publish_snap(engine: &Engine, core: &StrategyCore) {
    let snap = core.snapshot();
    engine
        .snapshots
        .write()
        .insert(snap.strategy_id.as_str().to_string(), snap.clone());
    let _ = engine.ws.send(WsMsg::Snapshot { snapshot: snap });
}

async fn persist_action(engine: &Engine, sid: &StrategyId, action: &Action) {
    if let Action::Place(req) = action {
        let rec = OrderRecord {
            coid: req.coid.clone(),
            strategy_id: sid.as_str().into(),
            venue: req.venue,
            symbol: req.symbol.clone(),
            side: req.side,
            purpose: if req.reduce_only { "exit" } else { "grid" }.into(),
            px: req.px,
            qty: req.qty,
            filled_qty: Qty::default(),
            status: "pending".into(),
            exchange_id: None,
            reduce_only: req.reduce_only,
            created_at: Ts::now_system(),
            updated_at: Ts::now_system(),
        };
        let _ = engine.store.upsert_order(&rec).await;
    }
}

async fn persist_exec(engine: &Engine, sid: &StrategyId, exec: &crate::types::ExecReport) {
    let status = match exec.kind {
        crate::types::ExecKind::Ack => "live",
        crate::types::ExecKind::PartialFill => "partial",
        crate::types::ExecKind::Fill => "filled",
        crate::types::ExecKind::Canceled => "canceled",
        crate::types::ExecKind::Expired => "expired",
        crate::types::ExecKind::Reject | crate::types::ExecKind::AmendReject => "rejected",
        crate::types::ExecKind::AmendAck => "live",
    };
    if let Some(px) = exec.px {
        let rec = OrderRecord {
            coid: exec.coid.clone(),
            strategy_id: sid.as_str().into(),
            venue: exec.venue,
            symbol: exec.symbol.clone(),
            side: exec.side,
            purpose: String::new(),
            px,
            qty: exec.qty.unwrap_or_default(),
            filled_qty: exec.filled_qty,
            status: status.into(),
            exchange_id: exec.exchange_id.clone(),
            reduce_only: false,
            created_at: exec.ts,
            updated_at: exec.ts,
        };
        let _ = engine.store.upsert_order(&rec).await;
    }
    if let (Some(px), Some(qty)) = (exec.last_px, exec.last_qty) {
        if qty.is_positive() {
            let fill = FillRecord {
                id: 0,
                strategy_id: sid.as_str().into(),
                coid: exec.coid.clone(),
                venue: exec.venue,
                symbol: exec.symbol.clone(),
                side: exec.side,
                px,
                qty,
                fee: None,
                ts: exec.ts,
            };
            let _ = engine.store.insert_fill(&fill).await;
        }
    }
}
