//! Idempotent development seed data.
//!
//! Run with `cargo run --bin seed`. Loads configuration the same way
//! `main.rs` does (`AppConfig::load()` — `config/default.toml` →
//! `config/{APP_ENV}.toml` → `APP__*` env vars) and applies migrations before
//! seeding, so this works standalone against a freshly-`docker-compose up -d`
//! database with no other setup step.
//!
//! This binary is only the entry point: env/config/pool/migrate, one sampled
//! `StudioNow` (`server.studio_timezone` + `Utc::now()`, taken exactly once
//! so every seeded timestamp derives from the same instant), then handed to
//! [`dataset::run`], which owns the dataset itself — see that module's doc
//! for the idempotency keys and points-ledger settlement shape.

mod dataset;

use anyhow::Context;
use chrono::Utc;
use sqlx::postgres::PgPoolOptions;

use dream_fly_backend::config::{AppConfig, AppEnv};
use dream_fly_backend::utils::studio_clock::StudioNow;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    // Refuse to run against production: this binary unconditionally upserts
    // a known admin credential (admin@dreamfly.tw / Admin#2026), which must
    // never exist outside development/staging. Read `APP_ENV` the same way
    // `config::AppConfig::load` and `config::validate_production_config` do,
    // and check it before the config is loaded or any DB connection is
    // opened.
    let app_env = AppEnv::from_env();
    if app_env.is_production() {
        anyhow::bail!(
            "refusing to run: APP_ENV={env} looks like production. This binary seeds \
             known credentials (admin@dreamfly.tw / Admin#2026) and must never run against \
             a production database.",
            env = app_env.raw()
        );
    }

    let config = AppConfig::load().context(
        "failed to load configuration — check APP_ENV, config/*.toml overlays, and APP__* env vars",
    )?;

    let db = PgPoolOptions::new()
        .max_connections(5)
        .connect(&config.database.url)
        .await
        .context("failed to connect to PostgreSQL — check APP__DATABASE__URL and that the DB is reachable")?;

    sqlx::migrate!("./migrations")
        .run(&db)
        .await
        .context("failed to run database migrations")?;

    println!("Connected + migrated. Seeding dev data (idempotent, safe to re-run)...");

    let at = StudioNow {
        tz: config.server.studio_timezone,
        now: Utc::now(),
    };
    let report = dataset::run(&db, at).await?;

    println!("\n-- row counts --");
    for (table, n) in &report.row_counts {
        println!("{table:<19} {n}");
    }

    println!("\nSeed complete. Dev accounts:");
    println!("  admin:  admin@dreamfly.tw  / Admin#2026");
    println!("  member: member@dreamfly.tw / Member#2026 (points_balance=1250)");
    println!("  coach:  coach1..coach4@dreamfly.tw / Coach#2026");
    println!(
        "  members: seed-member-01..24@dreamfly.tw / Member#2026 (12-month reporting dataset)"
    );

    db.close().await;
    Ok(())
}
