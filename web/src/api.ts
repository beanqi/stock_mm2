export type Venue = "binance" | "gate";
export type RunMode = "observe" | "live";

export interface GridLevel {
  distance: string;
  size: string;
}

export interface StrategyConfig {
  id: string;
  name: string;
  enabled: boolean;
  mode: RunMode;
  market: {
    maker_venue: Venue;
    maker_symbol: string;
    ref_venue: Venue;
    ref_symbol: string;
    fx: string;
    multiplier: string;
  };
  pricing: {
    spread_window_ms: number;
    min_samples: number;
    bucket_ms: number;
    grid: GridLevel[];
    requote_threshold: string;
    jump_threshold: string;
  };
  exit: {
    take_profit: string;
    fast_exit_timeout_ms: number;
    force_exit_timeout_ms: number;
    max_loss: string;
  };
  risk: {
    max_position: string;
    max_long: string;
    max_short: string;
    max_order_size: string;
    max_book_age_ms: number;
    max_spread_dev_k: string;
    max_abs_spread_dev: string | null;
  };
}

export interface Snapshot {
  strategy_id: string;
  name: string;
  lifecycle: string;
  run_mode: RunMode;
  maker_venue: Venue;
  maker_symbol: string;
  ref_venue: Venue;
  ref_symbol: string;
  fair_price: string | null;
  natural_spread: number | null;
  spread_vol: number | null;
  spread_t: number | null;
  permission: { reason?: string | null; allow_new_buy: boolean; allow_new_sell: boolean };
  slots: Array<{
    side: string;
    state: string;
    px?: string | null;
    qty?: string | null;
    desired_px?: string | null;
  }>;
  lots: Array<{ id: number; side: string; remaining: string; phase: string; entry_px: string }>;
  net_qty: string;
  links: { ref_md: boolean; maker_md: boolean; private: boolean; trading: boolean };
  warmup_samples: number;
  warmup_needed: number;
}

export interface StrategyRow {
  config: StrategyConfig;
  snapshot?: Snapshot | null;
}

export interface VenueHealth {
  venue: Venue;
  md: boolean;
  private: boolean;
  trading: boolean;
  has_keys: boolean;
}

export interface Instrument {
  venue: Venue;
  symbol: string;
  native_symbol: string;
  kind: string;
  recommended: boolean;
  volume_24h: string;
}

export interface Order {
  coid: string;
  strategy_id: string;
  venue: Venue;
  symbol: string;
  side: string;
  purpose: string;
  px: string;
  qty: string;
  filled_qty: string;
  status: string;
}

export interface Fill {
  id: number;
  strategy_id: string;
  coid: string;
  venue: Venue;
  symbol: string;
  side: string;
  px: string;
  qty: string;
  ts: number;
}

export interface RiskView {
  kill: boolean;
  total_abs_qty: string;
  max_notional: string;
}

export function defaultConfig(id = "demo"): StrategyConfig {
  return {
    id,
    name: "Demo AAPL",
    enabled: true,
    mode: "observe",
    market: {
      maker_venue: "gate",
      maker_symbol: "AAPLUSDT",
      ref_venue: "binance",
      ref_symbol: "AAPLUSDT",
      fx: "1",
      multiplier: "1",
    },
    pricing: {
      spread_window_ms: 120000,
      min_samples: 30,
      bucket_ms: 100,
      grid: [
        { distance: "0.001", size: "100" },
        { distance: "0.002", size: "200" },
        { distance: "0.0035", size: "300" },
      ],
      requote_threshold: "0.0005",
      jump_threshold: "0.004",
    },
    exit: {
      take_profit: "0.0012",
      fast_exit_timeout_ms: 5000,
      force_exit_timeout_ms: 12000,
      max_loss: "0.004",
    },
    risk: {
      max_position: "20000",
      max_long: "10000",
      max_short: "10000",
      max_order_size: "2000",
      max_book_age_ms: 15000,
      max_spread_dev_k: "6",
      max_abs_spread_dev: "0.01",
    },
  };
}

async function req<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(path, {
    headers: { "Content-Type": "application/json" },
    ...init,
  });
  if (!res.ok) {
    let msg = res.statusText;
    try {
      const j = await res.json();
      msg = j.error || msg;
    } catch {
      /* ignore */
    }
    throw new Error(msg);
  }
  if (res.status === 204) return undefined as T;
  return res.json();
}

export const api = {
  strategies: () => req<StrategyRow[]>("/api/strategies"),
  strategy: (id: string) => req<{ config: StrategyConfig; snapshot?: Snapshot }>("/api/strategies/" + id),
  save: (cfg: StrategyConfig, update = false) =>
    req<void>(update ? "/api/strategies/" + cfg.id : "/api/strategies", {
      method: update ? "PUT" : "POST",
      body: JSON.stringify(cfg),
    }),
  remove: (id: string) => req<void>("/api/strategies/" + id, { method: "DELETE" }),
  start: (id: string) => req<void>("/api/strategies/" + id + "/start", { method: "POST" }),
  stop: (id: string) => req<void>("/api/strategies/" + id + "/stop", { method: "POST" }),
  flatten: (id: string) => req<void>("/api/strategies/" + id + "/flatten", { method: "POST" }),
  venues: () => req<VenueHealth[]>("/api/venues"),
  instruments: () => req<Instrument[]>("/api/instruments"),
  orders: (strategyId?: string) =>
    req<Order[]>("/api/orders" + (strategyId ? `?strategy_id=${strategyId}` : "")),
  fills: (strategyId?: string) =>
    req<Fill[]>("/api/fills" + (strategyId ? `?strategy_id=${strategyId}` : "")),
  risk: () => req<RiskView>("/api/risk"),
  kill: () => req<void>("/api/risk/kill", { method: "POST" }),
  resume: () => req<void>("/api/risk/resume", { method: "POST" }),
};

export function wsUrl() {
  const proto = location.protocol === "https:" ? "wss" : "ws";
  return `${proto}://${location.host}/ws`;
}

export function fmtBps(x: number | null | undefined) {
  if (x == null || Number.isNaN(x)) return "—";
  return (x * 10000).toFixed(1) + " bp";
}

export function fmtPx(x: string | number | null | undefined) {
  if (x == null || x === "") return "—";
  const n = typeof x === "number" ? x : Number(x);
  if (Number.isNaN(n)) return String(x);
  return n.toFixed(4);
}
