use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet};

use crate::config::AppConfig;
use crate::types::{ContractKind, Instrument, SymbolId, VenueId};

#[derive(Clone, Debug, serde::Serialize)]
pub struct ListedInstrument {
    pub venue: VenueId,
    pub symbol: SymbolId,
    pub native_symbol: String,
    pub kind: ContractKind,
    pub tick_size: String,
    pub lot_size: String,
    pub contract_size: String,
    pub status: crate::types::InstrumentStatus,
    pub volume_24h: String,
    pub recommended: bool,
}

impl From<Instrument> for ListedInstrument {
    fn from(i: Instrument) -> Self {
        Self {
            venue: i.venue,
            symbol: i.symbol.clone(),
            native_symbol: i.native_symbol,
            kind: i.kind,
            tick_size: i.tick_size.to_string(),
            lot_size: i.lot_size.to_string(),
            contract_size: i.contract_size.to_string(),
            status: i.status,
            volume_24h: i.volume_24h.to_string(),
            recommended: false,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Catalog {
    pub by_venue: HashMap<VenueId, Vec<Instrument>>,
    pub recommended: Vec<SymbolId>,
}

impl Catalog {
    pub fn find(&self, venue: VenueId, symbol: &SymbolId) -> Option<Instrument> {
        self.by_venue.get(&venue)?.iter().find(|i| i.symbol == *symbol).cloned()
    }

    pub fn listed(&self) -> Vec<ListedInstrument> {
        let rec: HashSet<String> = self
            .recommended
            .iter()
            .map(|s| s.as_str().to_string())
            .collect();
        let mut out = Vec::new();
        for list in self.by_venue.values() {
            for i in list {
                let mut row = ListedInstrument::from(i.clone());
                row.recommended = rec.contains(i.symbol.as_str())
                    || rec.iter().any(|u| i.symbol.as_str().starts_with(u));
                out.push(row);
            }
        }
        out.sort_by(|a, b| b.recommended.cmp(&a.recommended).then(a.symbol.0.cmp(&b.symbol.0)));
        out
    }
}

pub fn rank_recommended(
    cfg: &AppConfig,
    catalogs: &HashMap<VenueId, Vec<Instrument>>,
    volumes: &HashMap<VenueId, Vec<(SymbolId, Decimal)>>,
) -> Vec<SymbolId> {
    let fallback: Vec<SymbolId> = cfg
        .recommended_underlyings
        .iter()
        .map(|u| SymbolId::new(format!("{u}USDT")))
        .collect();

    let binance = catalogs.get(&VenueId::Binance).cloned().unwrap_or_default();
    let gate = catalogs.get(&VenueId::Gate).cloned().unwrap_or_default();
    let bn: HashSet<String> = binance
        .iter()
        .filter(|i| i.kind == ContractKind::Stock || is_named_stock(&i.symbol, cfg))
        .map(|i| i.symbol.0.clone())
        .collect();
    let gt: HashSet<String> = gate
        .iter()
        .filter(|i| i.kind == ContractKind::Stock || is_named_stock(&i.symbol, cfg))
        .map(|i| i.symbol.0.clone())
        .collect();
    let mut both: Vec<String> = bn.intersection(&gt).cloned().collect();
    if both.is_empty() {
        // allow intersection of any USDT perp that appears on both
        let bn_all: HashSet<_> = binance.iter().map(|i| i.symbol.0.clone()).collect();
        let gt_all: HashSet<_> = gate.iter().map(|i| i.symbol.0.clone()).collect();
        both = bn_all.intersection(&gt_all).cloned().collect();
    }

    let vol_map: HashMap<String, Decimal> = volumes
        .values()
        .flatten()
        .fold(HashMap::new(), |mut acc, (s, v)| {
            *acc.entry(s.0.clone()).or_insert(Decimal::ZERO) += *v;
            acc
        });
    both.sort_by(|a, b| {
        vol_map
            .get(b)
            .unwrap_or(&Decimal::ZERO)
            .cmp(vol_map.get(a).unwrap_or(&Decimal::ZERO))
    });
    let mut ranked: Vec<SymbolId> = both.into_iter().map(SymbolId).take(20).collect();
    if ranked.len() < 20 {
        for s in fallback {
            if !ranked.iter().any(|x| x == &s) {
                ranked.push(s);
            }
            if ranked.len() >= 20 {
                break;
            }
        }
    }
    ranked
}

fn is_named_stock(symbol: &SymbolId, cfg: &AppConfig) -> bool {
    cfg.recommended_underlyings
        .iter()
        .any(|u| symbol.as_str().starts_with(u) && symbol.as_str().ends_with("USDT"))
}
