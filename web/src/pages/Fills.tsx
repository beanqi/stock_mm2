import { useEffect, useState } from "react";
import { api, fmtPx, type Fill } from "../api";

export default function Fills() {
  const [fills, setFills] = useState<Fill[]>([]);
  const [err, setErr] = useState("");

  useEffect(() => {
    api.fills().then(setFills).catch((e) => setErr(String(e)));
  }, []);

  return (
    <div>
      <h1>成交</h1>
      {err && <div className="err">{err}</div>}
      <div className="card">
        <table className="table">
          <thead>
            <tr>
              <th>策略</th>
              <th>交易所</th>
              <th>标的</th>
              <th>方向</th>
              <th>价格</th>
              <th>数量</th>
              <th>COID</th>
            </tr>
          </thead>
          <tbody>
            {fills.map((f) => (
              <tr key={f.id}>
                <td>{f.strategy_id}</td>
                <td>{f.venue}</td>
                <td>{f.symbol}</td>
                <td>{f.side}</td>
                <td>{fmtPx(f.px)}</td>
                <td>{f.qty}</td>
                <td>{f.coid}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}
