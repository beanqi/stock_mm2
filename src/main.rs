use anyhow::Context;
use stock_mm::config::load_app_config;
use stock_mm::engine::Engine;
use stock_mm::store::Store;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg = load_app_config().context("load config")?;
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&cfg.log_filter)),
        )
        .init();

    tracing::info!(env = ?cfg.trading_env, listen = %cfg.listen, "starting stock_mm");
    let store = Store::open(&cfg.database_url).await?;
    let engine = Engine::start(cfg, store).await?;

    if let Ok(strats) = engine.store.list_strategies().await {
        for s in strats {
            let _ = engine.upsert_strategy(s).await;
        }
    }

    stock_mm::api::serve(engine).await
}
