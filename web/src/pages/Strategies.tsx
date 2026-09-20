import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { api, fmtBps, fmtPx, isRunning, lifeLabel, reasonLabel, type Snapshot, type StrategyRow } from "../api";

export default function Strategies({ live }: { live: Record<string, Snapshot> }) {
  const [rows, setRows] = useState<StrategyRow[]>([]);
  const [err, setErr] = useState("");

  const load = () =>
    api
      .strategies()
      .then(setRows)
      .catch((e) => setErr(String(e)));

  useEffect(() => {
    load();
  }, []);

  return (
    <div>
      <div className="row" style={{ marginBottom: 16 }}>
        <h1 style={{ margin: 0 }}>策略</h1>
        <div className="spacer" />
        <Link to="/new">
          <button className="primary">新建策略</button>
        </Link>
      </div>
      {err && <div className="err">{err}</div>}
      <div className="grid cards">
        {rows.map((r) => {
          const s = live[r.config.id] || r.snapshot;
          const life = s?.lifecycle ?? "init";
          const active = isRunning(life);
          return (
            <div className="card" key={r.config.id}>
              <div className="row">
                <h3>{r.config.name}</h3>
                <span className={`badge ${life}`}>{lifeLabel(life)}</span>
                <span className="badge">{r.config.mode}</span>
              </div>
              <div className="meta">
                <span>
                  {r.config.market.maker_venue}/{r.config.market.maker_symbol}
                </span>
                <span>
                  ref {r.config.market.ref_venue}/{r.config.market.ref_symbol}
                </span>
              </div>
              <div className="row kpi" style={{ marginTop: 10 }}>
                <span>Fair {fmtPx(s?.fair_price)}</span>
                <span>Spread {fmtBps(s?.natural_spread)}</span>
                <span>仓 {s?.net_qty ?? "0"}</span>
              </div>
              {s?.permission?.reason && <div className="reason">未报价：{reasonLabel(s.permission.reason)}</div>}
              <div className="row" style={{ marginTop: 12 }}>
                <Link to={`/strategies/${r.config.id}`}>
                  <button>详情</button>
                </Link>
                {active ? (
                  <button onClick={() => api.stop(r.config.id).then(load).catch((e) => setErr(String(e)))}>
                    停止
                  </button>
                ) : (
                  <button
                    className="primary"
                    onClick={() => api.start(r.config.id).then(load).catch((e) => setErr(String(e)))}
                  >
                    启动
                  </button>
                )}
                <button onClick={() => api.flatten(r.config.id)}>全平</button>
                <button
                  className="danger"
                  onClick={() => {
                    if (confirm("删除策略？")) api.remove(r.config.id).then(load);
                  }}
                >
                  删除
                </button>
              </div>
            </div>
          );
        })}
        {rows.length === 0 && <div className="card">还没有策略。先新建一个 observe 实例。</div>}
      </div>
    </div>
  );
}
