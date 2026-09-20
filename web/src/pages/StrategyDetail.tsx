import { useEffect, useState } from "react";
import { Link, useParams } from "react-router-dom";
import { api, fmtBps, fmtPx, isRunning, lifeLabel, reasonLabel, type Snapshot, type StrategyConfig } from "../api";

export default function StrategyDetail({ live }: { live: Record<string, Snapshot> }) {
  const { id = "" } = useParams();
  const [cfg, setCfg] = useState<StrategyConfig | null>(null);
  const [rest, setRest] = useState<Snapshot | null>(null);
  const [err, setErr] = useState("");

  const load = () =>
    api
      .strategy(id)
      .then((r) => {
        setCfg(r.config);
        setRest(r.snapshot || null);
      })
      .catch((e) => setErr(String(e)));

  useEffect(() => {
    load();
  }, [id]);

  const s = live[id] || rest;
  const life = s?.lifecycle || "init";
  const active = isRunning(life);

  return (
    <div>
      <div className="row">
        <h1 style={{ margin: 0 }}>{cfg?.name || id}</h1>
        <span className={`badge ${life}`}>{lifeLabel(life)}</span>
        <div className="spacer" />
        <Link to={`/strategies/${id}/edit`}>
          <button>编辑</button>
        </Link>
        {active ? (
          <button onClick={() => api.stop(id).then(load).catch((e) => setErr(String(e)))}>停止</button>
        ) : (
          <button className="primary" onClick={() => api.start(id).then(load).catch((e) => setErr(String(e)))}>
            启动
          </button>
        )}
        <button onClick={() => api.flatten(id)}>全平</button>
      </div>
      {err && <div className="err">{err}</div>}
      <div className="grid cards" style={{ marginTop: 16 }}>
        <div className="card">
          <h3>定价</h3>
          <div>FairPrice {fmtPx(s?.fair_price)}</div>
          <div>NaturalSpread {fmtBps(s?.natural_spread)}</div>
          <div>SpreadVol {fmtBps(s?.spread_vol)}</div>
          <div>Spread_t {fmtBps(s?.spread_t)}</div>
          <div>
            Warmup {s?.warmup_samples ?? 0}/{s?.warmup_needed ?? 0}
          </div>
          {s?.permission?.reason && <div className="reason">{reasonLabel(s.permission.reason)}</div>}
        </div>
        <div className="card">
          <h3>链路</h3>
          <div>参考行情 {s?.links.ref_md ? "UP" : "DOWN"}</div>
          <div>挂单行情 {s?.links.maker_md ? "UP" : "DOWN"}</div>
          <div>私有 {s?.links.private ? "UP" : "DOWN"}</div>
          <div>交易 {s?.links.trading ? "UP" : "DOWN"}</div>
          <div>净仓 {s?.net_qty ?? "0"}</div>
        </div>
      </div>
      <div className="card" style={{ marginTop: 12 }}>
        <h3>槽位</h3>
        <table className="table">
          <thead>
            <tr>
              <th>边</th>
              <th>状态</th>
              <th>挂单价</th>
              <th>目标价</th>
              <th>数量</th>
            </tr>
          </thead>
          <tbody>
            {(s?.slots || []).map((sl, i) => (
              <tr key={i}>
                <td>{sl.side}</td>
                <td>{sl.state}</td>
                <td>{fmtPx(sl.px)}</td>
                <td>{fmtPx(sl.desired_px)}</td>
                <td>{sl.qty ?? "—"}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      <div className="card" style={{ marginTop: 12 }}>
        <h3>Lots</h3>
        <table className="table">
          <thead>
            <tr>
              <th>ID</th>
              <th>方向</th>
              <th>剩余</th>
              <th>开仓价</th>
              <th>阶段</th>
            </tr>
          </thead>
          <tbody>
            {(s?.lots || []).map((l) => (
              <tr key={l.id}>
                <td>{l.id}</td>
                <td>{l.side}</td>
                <td>{l.remaining}</td>
                <td>{fmtPx(l.entry_px)}</td>
                <td>{l.phase}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}
