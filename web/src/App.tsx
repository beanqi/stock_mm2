import { Navigate, NavLink, Route, Routes } from "react-router-dom";
import { useEffect, useState } from "react";
import { api, wsUrl, type RiskView, type Snapshot, type VenueHealth } from "./api";
import Login from "./pages/Login";
import Strategies from "./pages/Strategies";
import StrategyForm from "./pages/StrategyForm";
import StrategyDetail from "./pages/StrategyDetail";
import Orders from "./pages/Orders";

export default function App() {
  return (
    <Routes>
      <Route path="/login" element={<Login />} />
      <Route path="*" element={<Shell />} />
    </Routes>
  );
}

function Shell() {
  const [gate, setGate] = useState<"loading" | "ok" | "login">("loading");
  const [user, setUser] = useState<string | null>(null);
  const [venues, setVenues] = useState<VenueHealth[]>([]);
  const [risk, setRisk] = useState<RiskView | null>(null);
  const [snaps, setSnaps] = useState<Record<string, Snapshot>>({});

  useEffect(() => {
    let cancelled = false;
    api
      .authStatus()
      .then(async (s) => {
        if (!s.enabled) {
          if (!cancelled) setGate("ok");
          return;
        }
        try {
          const me = await api.me();
          if (!cancelled) {
            setUser(me.username);
            setGate("ok");
          }
        } catch {
          if (!cancelled) setGate("login");
        }
      })
      .catch(() => {
        if (!cancelled) setGate("ok");
      });
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    if (gate !== "ok") return;
    api.venues().then(setVenues).catch(() => {});
    api.risk().then(setRisk).catch(() => {});
    const ws = new WebSocket(wsUrl());
    ws.onmessage = (ev) => {
      try {
        const msg = JSON.parse(ev.data);
        if (msg.type === "hello") {
          setVenues(msg.venues || []);
          setRisk(msg.risk || null);
          const next: Record<string, Snapshot> = {};
          for (const s of msg.snapshots || []) next[s.strategy_id] = s;
          setSnaps(next);
        } else if (msg.type === "health") {
          setVenues(msg.venues || []);
        } else if (msg.type === "risk") {
          setRisk(msg.risk);
        } else if (msg.type === "snapshot") {
          const s = msg.snapshot as Snapshot;
          setSnaps((prev) => ({ ...prev, [s.strategy_id]: s }));
        }
      } catch {
        /* ignore */
      }
    };
    return () => ws.close();
  }, [gate]);

  if (gate === "loading") return null;
  if (gate === "login") return <Navigate to="/login" replace />;

  return (
    <div className="app">
      <header className="top">
        <div className="brand">STOCK MM</div>
        <nav className="nav">
          <NavLink to="/" end>
            策略
          </NavLink>
          <NavLink to="/new">新建</NavLink>
          <NavLink to="/orders">订单</NavLink>
          <NavLink to="/fills">成交</NavLink>
        </nav>
        <div className="spacer" />
        <div className="pills">
          {(["binance", "gate"] as const).map((v) => {
            const h = venues.find((x) => x.venue === v);
            return (
              <span key={v}>
                <i className={`dot ${h?.md ? "on" : "off"}`} />
                {v} 行情
                {h?.has_keys ? (
                  <>
                    <i className={`dot ${h?.trading ? "on" : "off"}`} style={{ marginLeft: 8 }} />
                    交易
                  </>
                ) : (
                  " ·无Key"
                )}
              </span>
            );
          })}
          <span>敞口 {risk?.total_abs_qty ?? "0"}</span>
        </div>
        {risk?.kill ? (
          <button className="primary" onClick={() => api.resume().then(() => api.risk().then(setRisk))}>
            解除 Kill
          </button>
        ) : (
          <button className="danger" onClick={() => api.kill().then(() => api.risk().then(setRisk))}>
            Kill Switch
          </button>
        )}
        {user && <span className="who">{user}</span>}
        {user && (
          <button
            className="ghost"
            onClick={() => api.logout().finally(() => location.assign("/login"))}
          >
            退出
          </button>
        )}
      </header>
      <main className="main">
        <Routes>
          <Route path="/" element={<Strategies live={snaps} />} />
          <Route path="/new" element={<StrategyForm />} />
          <Route path="/strategies/:id" element={<StrategyDetail live={snaps} />} />
          <Route path="/strategies/:id/edit" element={<StrategyForm />} />
          <Route path="/orders" element={<Orders kind="orders" />} />
          <Route path="/fills" element={<Orders kind="fills" />} />
        </Routes>
      </main>
    </div>
  );
}
