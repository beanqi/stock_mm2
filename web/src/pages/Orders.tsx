import { useEffect, useState } from "react";
import { api, fmtPx, type Fill, type Order } from "../api";

export default function Orders({ kind }: { kind: "orders" | "fills" }) {
  const [orders, setOrders] = useState<Order[]>([]);
  const [fills, setFills] = useState<Fill[]>([]);
  const [err, setErr] = useState("");

  useEffect(() => {
    if (kind === "orders") {
      api.orders().then(setOrders).catch((e) => setErr(String(e)));
    } else {
      api.fills().then(setFills).catch((e) => setErr(String(e)));
    }
  }, [kind]);

  return (
    <div>
      <h1>{kind === "orders" ? "订单" : "成交"}</h1>
      {err && <div className="err">{err}</div>}
      <div className="card">
        {kind === "orders" ? (
          <table className="table">
            <thead>
              <tr>
                <th>策略</th>
                <th>交易所</th>
                <th>标的</th>
                <th>方向</th>
                <th>价格</th>
                <th>数量</th>
                <th>成交</th>
                <th>状态</th>
                <th>COID</th>
              </tr>
            </thead>
            <tbody>
              {orders.map((o) => (
                <tr key={o.coid}>
                  <td>{o.strategy_id}</td>
                  <td>{o.venue}</td>
                  <td>{o.symbol}</td>
                  <td>{o.side}</td>
                  <td>{fmtPx(o.px)}</td>
                  <td>{o.qty}</td>
                  <td>{o.filled_qty}</td>
                  <td>{o.status}</td>
                  <td>{o.coid}</td>
                </tr>
              ))}
            </tbody>
          </table>
        ) : (
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
        )}
      </div>
    </div>
  );
}
