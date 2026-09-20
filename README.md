# stock_mm

股票永续跨市场参考价网格做市（v1）。Maker / Reference 可在 Binance 与 Gate 之间任选；默认 observe 不下单。密钥只从 `.env` 读取，控制台默认绑在 `127.0.0.1`。

## 启动

```bash
cp .env.example .env
# 按需填写 BINANCE_* / GATE_* ；不填也能跑行情与 observe

cd web && npm install && npm run build && cd ..
cargo run
```

浏览器打开 <http://127.0.0.1:8080>。控制台默认启用登录：用户名 + 密码，再输入验证器中的 6 位 TOTP 验证码。首次启动若未设置 `AUTH_PASSWORD`，会生成一份密码写到 `data/initial-password.txt` 并打到日志；登录后扫描二维码绑定验证器。换绑验证器设 `AUTH_RESET_TOTP=1` 后重启；设 `AUTH_DISABLED=1` 可关闭登录。

前端开发热更新：

```bash
# 终端 1
cargo run
# 终端 2
cd web && npm install && npm run dev
```

然后打开 <http://127.0.0.1:5173>。

## 配置

- `.env`：API Key、`TRADING_ENV=testnet|mainnet`、`API_LISTEN`、`DATABASE_URL`、控制台登录（`AUTH_USERNAME` / `AUTH_PASSWORD` / 可选 `AUTH_TOTP_SECRET`）
- `config/app.toml`：推荐标的兜底名单、全局风控上限

策略在网页上创建，字段对应 `docs/策略设计.md` §9。`mode=observe` 只计算 FairPrice / 网格，不下单；切到 `live` 才会发单。仓位上限与网格数量都按 **USDT 名义**。右上角 Kill Switch 会广播全平。

公开行情与合约目录走主网（股票永续在测试网通常不可用）；`TRADING_ENV=testnet` 只影响签名后的交易 / 私有推送。Gate 合约按**统一账户 + 单向持仓**接入（`position_mode=single`）；API Key 需要勾选 unified 权限，不会自动把经典账户升级成统一账户。

## 默认标的

启动时拉取两所 USDT 永续合约，优先取两边都有的股票永续，按 24h 成交额取约 20 个；不足则用 `config/app.toml` 的名单补齐。

## 文档

- [docs/策略设计.md](docs/策略设计.md)
- [docs/架构设计.md](docs/架构设计.md)
