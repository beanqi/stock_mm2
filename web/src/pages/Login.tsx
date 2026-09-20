import { FormEvent, useEffect, useState } from "react";
import { useNavigate } from "react-router-dom";
import { api, type AuthStatus, type LoginChallenge } from "../api";

export default function Login() {
  const nav = useNavigate();
  const [status, setStatus] = useState<AuthStatus | null>(null);
  const [username, setUsername] = useState("admin");
  const [password, setPassword] = useState("");
  const [code, setCode] = useState("");
  const [challenge, setChallenge] = useState<LoginChallenge | null>(null);
  const [err, setErr] = useState("");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    api.authStatus()
      .then(async (s) => {
        setStatus(s);
        if (!s.enabled) {
          nav("/", { replace: true });
          return;
        }
        try {
          await api.me();
          nav("/", { replace: true });
        } catch {
          /* stay on login */
        }
      })
      .catch(() => setStatus({ enabled: true, enrolled: false }));
  }, [nav]);

  const submitPassword = async (e: FormEvent) => {
    e.preventDefault();
    setErr("");
    setBusy(true);
    try {
      setChallenge(await api.login(username, password));
      setCode("");
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const submitCode = async (e: FormEvent) => {
    e.preventDefault();
    if (!challenge) return;
    setErr("");
    setBusy(true);
    try {
      await api.verify(challenge.ticket, code);
      nav("/", { replace: true });
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  if (!status?.enabled) {
    return null;
  }

  return (
    <div className="login-wrap">
      <form className="login-card card" onSubmit={challenge ? submitCode : submitPassword}>
        <div className="brand">STOCK MM</div>
        <h1>{challenge?.enroll ? "绑定验证器" : challenge ? "二次验证" : "控制台登录"}</h1>
        {!challenge ? (
          <>
            <p className="meta">用户名 + 密码，然后输入验证器中的 6 位验证码。</p>
            <label>
              用户名
              <input
                autoComplete="username"
                value={username}
                onChange={(e) => setUsername(e.target.value)}
              />
            </label>
            <label>
              密码
              <input
                type="password"
                autoComplete="current-password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
              />
            </label>
            <button className="primary" type="submit" disabled={busy || !username || !password}>
              下一步
            </button>
          </>
        ) : (
          <>
            {challenge.enroll && (
              <div className="enroll">
                <p className="meta">
                  用 Google Authenticator、1Password 或 Bitwarden 扫描二维码，或手动输入密钥。
                </p>
                {challenge.qr_svg && (
                  <div
                    className="qr-box"
                    dangerouslySetInnerHTML={{ __html: challenge.qr_svg }}
                  />
                )}
                {challenge.secret && (
                  <div className="secret">
                    <span>密钥</span>
                    <code>{challenge.secret}</code>
                  </div>
                )}
              </div>
            )}
            <p className="meta">请输入验证器中的 6 位验证码。</p>
            <label>
              验证码
              <input
                className="otp"
                inputMode="numeric"
                autoComplete="one-time-code"
                autoFocus
                maxLength={6}
                value={code}
                onChange={(e) => setCode(e.target.value.replace(/\D/g, "").slice(0, 6))}
              />
            </label>
            <button className="primary" type="submit" disabled={busy || code.length !== 6}>
              {challenge.enroll ? "验证并绑定" : "登录"}
            </button>
            <button
              type="button"
              className="ghost"
              onClick={() => {
                setChallenge(null);
                setCode("");
                setErr("");
              }}
            >
              返回
            </button>
          </>
        )}
        {err && <div className="err">{err}</div>}
      </form>
    </div>
  );
}
