import { useEffect, useState } from "react";
import { useNavigate, useParams } from "react-router-dom";
import { api, defaultConfig, type Instrument, type StrategyConfig, type Venue } from "../api";

export default function StrategyForm() {
  const { id } = useParams();
  const nav = useNavigate();
  const [cfg, setCfg] = useState<StrategyConfig>(defaultConfig(id || "s1"));
  const [inst, setInst] = useState<Instrument[]>([]);
  const [err, setErr] = useState("");

  useEffect(() => {
    api.instruments().then(setInst).catch(() => {});
    if (id) {
      api.strategy(id).then((r) => setCfg(normalize(r.config))).catch(() => {});
    }
  }, [id]);

  const rec = inst.filter((i) => i.recommended);
  const symbols = (rec.length ? rec : inst).map((i) => i.symbol);
  const uniq = Array.from(new Set(symbols)).slice(0, 40);

  const save = async () => {
    setErr("");
    try {
      await api.save(cfg, Boolean(id));
      nav("/");
    } catch (e) {
      setErr(String(e));
    }
  };

  return (
    <div>
      <h1>{id ? "编辑策略" : "新建策略"}</h1>
      {err && <div className="err">{err}</div>}
      <div className="form">
        <section className="card">
          <h3>基础</h3>
          <div className="fields">
            <label>
              ID（不可含 -）
              <input
                value={cfg.id}
                disabled={Boolean(id)}
                onChange={(e) => setCfg({ ...cfg, id: e.target.value })}
              />
            </label>
            <label>
              名称
              <input value={cfg.name} onChange={(e) => setCfg({ ...cfg, name: e.target.value })} />
            </label>
            <label>
              模式
              <select
                value={cfg.mode}
                onChange={(e) => setCfg({ ...cfg, mode: e.target.value as StrategyConfig["mode"] })}
              >
                <option value="observe">observe（只观察）</option>
                <option value="live">live（实盘）</option>
              </select>
            </label>
          </div>
        </section>
        <section className="card">
          <h3>市场</h3>
          <div className="fields">
            <VenueField
              label="Maker 交易所"
              value={cfg.market.maker_venue}
              onChange={(v) => setCfg({ ...cfg, market: { ...cfg.market, maker_venue: v } })}
            />
            <SymbolField
              label="Maker 标的"
              value={cfg.market.maker_symbol}
              options={uniq}
              onChange={(v) => setCfg({ ...cfg, market: { ...cfg.market, maker_symbol: v } })}
            />
            <VenueField
              label="Reference 交易所"
              value={cfg.market.ref_venue}
              onChange={(v) => setCfg({ ...cfg, market: { ...cfg.market, ref_venue: v } })}
            />
            <SymbolField
              label="Reference 标的"
              value={cfg.market.ref_symbol}
              options={uniq}
              onChange={(v) => setCfg({ ...cfg, market: { ...cfg.market, ref_symbol: v } })}
            />
            <label>
              FX
              <input
                value={cfg.market.fx}
                onChange={(e) => setCfg({ ...cfg, market: { ...cfg.market, fx: e.target.value } })}
              />
            </label>
            <label>
              Multiplier
              <input
                value={cfg.market.multiplier}
                onChange={(e) =>
                  setCfg({ ...cfg, market: { ...cfg.market, multiplier: e.target.value } })
                }
              />
            </label>
          </div>
        </section>
        <section className="card">
          <h3>价格 / 网格</h3>
          <div className="fields">
            <Num label="Spread 窗口 ms" value={cfg.pricing.spread_window_ms} onChange={(v) => setCfg({ ...cfg, pricing: { ...cfg.pricing, spread_window_ms: v } })} />
            <Num label="最少样本" value={cfg.pricing.min_samples} onChange={(v) => setCfg({ ...cfg, pricing: { ...cfg.pricing, min_samples: v } })} />
            <label>
              Requote
              <input value={cfg.pricing.requote_threshold} onChange={(e) => setCfg({ ...cfg, pricing: { ...cfg.pricing, requote_threshold: e.target.value } })} />
            </label>
            <label>
              Jump
              <input value={cfg.pricing.jump_threshold} onChange={(e) => setCfg({ ...cfg, pricing: { ...cfg.pricing, jump_threshold: e.target.value } })} />
            </label>
          </div>
          {cfg.pricing.grid.map((g, i) => (
            <div className="fields" key={i}>
              <label>
                档{i + 1} 距离
                <input
                  value={g.distance}
                  onChange={(e) => {
                    const grid = cfg.pricing.grid.slice();
                    grid[i] = { ...g, distance: e.target.value };
                    setCfg({ ...cfg, pricing: { ...cfg.pricing, grid } });
                  }}
                />
              </label>
              <label>
                档{i + 1} 数量(U)
                <input
                  value={g.size}
                  onChange={(e) => {
                    const grid = cfg.pricing.grid.slice();
                    grid[i] = { ...g, size: e.target.value };
                    setCfg({ ...cfg, pricing: { ...cfg.pricing, grid } });
                  }}
                />
              </label>
            </div>
          ))}
        </section>
        <section className="card">
          <h3>退出</h3>
          <div className="fields">
            <label>
              Take Profit
              <input value={cfg.exit.take_profit} onChange={(e) => setCfg({ ...cfg, exit: { ...cfg.exit, take_profit: e.target.value } })} />
            </label>
            <Num label="Fast Exit ms" value={cfg.exit.fast_exit_timeout_ms} onChange={(v) => setCfg({ ...cfg, exit: { ...cfg.exit, fast_exit_timeout_ms: v } })} />
            <Num label="Force Exit ms" value={cfg.exit.force_exit_timeout_ms} onChange={(v) => setCfg({ ...cfg, exit: { ...cfg.exit, force_exit_timeout_ms: v } })} />
            <label>
              Max Loss
              <input value={cfg.exit.max_loss} onChange={(e) => setCfg({ ...cfg, exit: { ...cfg.exit, max_loss: e.target.value } })} />
            </label>
          </div>
        </section>
        <section className="card">
          <h3>风险</h3>
          <div className="fields">
            <label>Max Position (USDT)<input value={cfg.risk.max_position} onChange={(e) => setCfg({ ...cfg, risk: { ...cfg.risk, max_position: e.target.value } })} /></label>
            <label>Max Long (USDT)<input value={cfg.risk.max_long} onChange={(e) => setCfg({ ...cfg, risk: { ...cfg.risk, max_long: e.target.value } })} /></label>
            <label>Max Short (USDT)<input value={cfg.risk.max_short} onChange={(e) => setCfg({ ...cfg, risk: { ...cfg.risk, max_short: e.target.value } })} /></label>
            <label>Max Order Size (USDT)<input value={cfg.risk.max_order_size} onChange={(e) => setCfg({ ...cfg, risk: { ...cfg.risk, max_order_size: e.target.value } })} /></label>
            <Num label="最大行情延迟 ms" value={cfg.risk.max_book_age_ms} onChange={(v) => setCfg({ ...cfg, risk: { ...cfg.risk, max_book_age_ms: v } })} />
            <label>K<input value={cfg.risk.max_spread_dev_k} onChange={(e) => setCfg({ ...cfg, risk: { ...cfg.risk, max_spread_dev_k: e.target.value } })} /></label>
          </div>
        </section>
        <div className="row">
          <button className="primary" onClick={save}>
            保存
          </button>
          <button onClick={() => nav(-1)}>取消</button>
        </div>
      </div>
    </div>
  );
}

function VenueField({
  label,
  value,
  onChange,
}: {
  label: string;
  value: Venue;
  onChange: (v: Venue) => void;
}) {
  return (
    <label>
      {label}
      <select value={value} onChange={(e) => onChange(e.target.value as Venue)}>
        <option value="gate">gate</option>
        <option value="binance">binance</option>
      </select>
    </label>
  );
}

function SymbolField({
  label,
  value,
  options,
  onChange,
}: {
  label: string;
  value: string;
  options: string[];
  onChange: (v: string) => void;
}) {
  return (
    <label>
      {label}
      <input list={label} value={value} onChange={(e) => onChange(e.target.value)} />
      <datalist id={label}>
        {options.map((s) => (
          <option key={s} value={s} />
        ))}
      </datalist>
    </label>
  );
}

function Num({
  label,
  value,
  onChange,
}: {
  label: string;
  value: number;
  onChange: (v: number) => void;
}) {
  return (
    <label>
      {label}
      <input type="number" value={value} onChange={(e) => onChange(Number(e.target.value))} />
    </label>
  );
}

function normalize(cfg: StrategyConfig): StrategyConfig {
  const str = (v: unknown, d: string) => (v == null ? d : String(v));
  return {
    ...cfg,
    market: {
      ...cfg.market,
      fx: str(cfg.market.fx, "1"),
      multiplier: str(cfg.market.multiplier, "1"),
    },
    pricing: {
      ...cfg.pricing,
      requote_threshold: str(cfg.pricing.requote_threshold, "0.0005"),
      jump_threshold: str(cfg.pricing.jump_threshold, "0.004"),
      grid: (cfg.pricing.grid || []).map((g) => ({
        distance: str(g.distance, "0.001"),
        size: str(g.size, "100"),
      })),
    },
    exit: {
      ...cfg.exit,
      take_profit: str(cfg.exit.take_profit, "0.0012"),
      max_loss: str(cfg.exit.max_loss, "0.004"),
    },
    risk: {
      ...cfg.risk,
      max_position: str(cfg.risk.max_position, "20"),
      max_long: str(cfg.risk.max_long, "10"),
      max_short: str(cfg.risk.max_short, "10"),
      max_order_size: str(cfg.risk.max_order_size, "500"),
      max_spread_dev_k: str(cfg.risk.max_spread_dev_k, "6"),
      max_abs_spread_dev: cfg.risk.max_abs_spread_dev == null ? null : String(cfg.risk.max_abs_spread_dev),
    },
  };
}
