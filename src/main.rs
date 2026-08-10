use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use sqlx::postgres::PgPoolOptions;
use tokio::net::TcpListener;
use tokio::signal;
use tokio::sync::watch;
use tokio_util::task::TaskTracker;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

use dream_fly_backend::config::{AppConfig, AppEnv, validate_production_config};
use dream_fly_backend::kafka;
use dream_fly_backend::kafka::producer::KafkaPublisher;
use dream_fly_backend::startup;
use dream_fly_backend::state::AppState;
use dream_fly_backend::utils::clock::{Clock, SystemClock};
use dream_fly_backend::utils::email::{EmailClient, EmailSender};
use dream_fly_backend::utils::google_oauth::JwksCache;
use dream_fly_backend::utils::sms::SmsClient;

/// Bound on the total time we'll wait for background tasks and the DB
/// pool to finish during graceful shutdown. Container orchestrators
/// (e.g. Kubernetes `terminationGracePeriodSeconds` defaulting to 30s)
/// will send SIGKILL if we take longer — exit earlier so they don't.
const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(25);

/// Wait for SIGTERM or Ctrl+C. Resolves when either fires.
///
/// Signal handler registration can realistically only fail if the process
/// is missing permissions or signal slots — both are startup-time errors,
/// not runtime errors, so a panic here is reasonable. We keep the expect
/// messages verbose so operators see the actual cause in the log.
async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler — process signal slots exhausted?");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler — process signal slots exhausted?")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("Ctrl+C received, shutting down"),
        _ = terminate => tracing::info!("SIGTERM received, shutting down"),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let app_env = AppEnv::from_env();

    // Initialize tracing. Production emits structured JSON so the log
    // aggregator can parse fields; development uses the pretty formatter.
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "dream_fly_backend=debug,tower_http=debug,axum=info".into());

    if app_env.is_production() {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_current_span(true)
                    .with_span_list(false)
                    .with_target(true),
            )
            .init();
    } else {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(tracing_subscriber::fmt::layer())
            .init();
    }

    // Load configuration
    let config = AppConfig::load().context(
        "failed to load configuration — check APP_ENV, config/*.toml overlays, and APP__* env vars",
    )?;
    validate_production_config(&config, &app_env)?;
    tracing::info!("Configuration loaded");

    // Connect to PostgreSQL. `after_connect` pins session-level safety
    // settings so a runaway query cannot hold a connection forever.
    let db = PgPoolOptions::new()
        .max_connections(config.database.max_connections)
        .min_connections(config.database.min_connections)
        .acquire_timeout(Duration::from_secs(10))
        .idle_timeout(Some(Duration::from_secs(300)))
        .max_lifetime(Some(Duration::from_secs(1800)))
        .test_before_acquire(true)
        .after_connect(|conn, _meta| {
            Box::pin(async move {
                use sqlx::Executor;
                conn.execute("SET application_name = 'dream_fly_backend'").await?;
                // Kill any query that runs longer than 15s at the server level.
                conn.execute("SET statement_timeout = '15s'").await?;
                // Kill any idle-in-transaction connection after 30s.
                conn.execute("SET idle_in_transaction_session_timeout = '30s'").await?;
                Ok(())
            })
        })
        .connect(&config.database.url)
        .await
        .context("failed to connect to PostgreSQL — check APP__DATABASE__URL and that the DB is reachable")?;
    tracing::info!("Connected to PostgreSQL");

    // Run migrations
    sqlx::migrate!("./migrations")
        .run(&db)
        .await
        .context("failed to run database migrations")?;
    tracing::info!("Database migrations applied");

    // Connect to Redis
    let redis_client = redis::Client::open(config.redis.url.as_str())
        .context("failed to build Redis client — check APP__REDIS__URL syntax")?;
    let redis = redis::aio::ConnectionManager::new(redis_client)
        .await
        .context("failed to connect to Redis — check APP__REDIS__URL and that Redis is reachable")?;
    tracing::info!("Connected to Redis");

    // Optionally connect to Kafka
    let kafka_producer = if config.kafka.enabled {
        match kafka::producer::create_producer(&config.kafka.brokers) {
            Ok(producer) => {
                tracing::info!("Connected to Kafka at {}", config.kafka.brokers);
                Some(Arc::new(producer))
            }
            Err(err) => {
                tracing::warn!("Failed to connect to Kafka: {err}, continuing without Kafka");
                None
            }
        }
    } else {
        tracing::info!("Kafka is disabled");
        None
    };

    // Build a shared HTTP client (connection pooling). Keep the global
    // timeout short — handlers that need longer can set per-call timeouts.
    // A 10s global blocks async tasks + DB connections when Google/Twilio
    // stalls; 5s is a reasonable cap.
    let http_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .connect_timeout(Duration::from_secs(3))
        .pool_idle_timeout(Duration::from_secs(30))
        .build()
        .context("failed to build HTTP client")?;

    // Build the shared SMTP client once (TLS + connection pooling) and
    // erase the concrete type at the AppState boundary so integration tests
    // can inject a recording mock in its place.
    let email_client: Arc<dyn EmailSender> = Arc::new(
        EmailClient::new(&config.email)
            .context("failed to build email client — check APP__EMAIL__* settings")?,
    );

    // Twilio client — reuses the HTTP connection pool. Concrete type, not
    // trait-erased like email: test substitution goes through
    // `SmsConfig::twilio_base_url` instead (see `AppState::sms_client`).
    let sms_client: Arc<SmsClient> = Arc::new(SmsClient::new(&config.sms, http_client.clone()));

    // Wall-clock source for handler-sampled `now` — production always reads
    // the real system clock; tests substitute `MockClock`.
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);

    // Wrap the config in Arc once — AppState::clone is per-request.
    let config_arc = Arc::new(config);

    // Independent binding, created before `AppState` so it survives past
    // `startup::build_router` moving `state` away — see `background_tasks`
    // teardown below and the doc on `AppState::background_tasks`.
    let background_tasks = TaskTracker::new();

    // Build application state
    let state = AppState {
        db: db.clone(),
        redis,
        kafka_producer,
        config: config_arc.clone(),
        http_client,
        email_client,
        sms_client,
        clock,
        jwks_cache: Arc::new(JwksCache::new()),
        background_tasks: background_tasks.clone(),
    };

    // Shutdown signaling channel: broadcasts a single `true` when the
    // server needs to wind down, so both the HTTP layer and the Kafka
    // consumer can observe it.
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    // Background task: periodically delete expired/revoked refresh tokens
    // to prevent the `refresh_tokens` table from growing unboundedly.
    let cleanup_db = db.clone();
    let mut cleanup_shutdown = shutdown_rx.clone();
    let cleanup_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(3600));
        loop {
            tokio::select! {
                biased;

                // Shutdown wins over the next tick: don't start a cleanup
                // pass we cannot finish before the main task drops the DB pool.
                _ = cleanup_shutdown.changed() => {
                    if *cleanup_shutdown.borrow() {
                        tracing::info!("token cleanup task received shutdown, exiting");
                        break;
                    }
                }

                _ = interval.tick() => {
                    match dream_fly_backend::modules::auth::repository::delete_expired_tokens(
                        &cleanup_db,
                    )
                    .await
                    {
                        Ok(n) if n > 0 => {
                            tracing::info!(deleted = n, "expired refresh tokens cleaned up");
                        }
                        Ok(_) => {}
                        Err(e) => {
                            tracing::error!(error = ?e, "refresh token cleanup failed");
                        }
                    }
                }
            }
        }
    });

    // Start Kafka consumer if enabled
    let consumer_handle = if config_arc.kafka.enabled {
        match kafka::consumer::create_consumer(&config_arc.kafka.brokers, &config_arc.kafka.group_id)
        {
            Ok(consumer) => {
                let consumer_db = state.db.clone();
                let consumer_shutdown = shutdown_rx.clone();
                let handle = tokio::spawn(async move {
                    kafka::consumer::start_consumer(consumer, consumer_db, consumer_shutdown).await;
                });
                tracing::info!("Kafka consumer started");
                Some(handle)
            }
            Err(err) => {
                tracing::warn!("Failed to create Kafka consumer: {err}, continuing without consumer");
                None
            }
        }
    } else {
        None
    };

    // Start outbox dispatcher: drains `events_outbox` rows to Kafka with
    // at-least-once semantics. If Kafka is disabled, events accumulate in
    // the table (a clear operational signal) until the feature is enabled
    // and the dispatcher drains the backlog at boot.
    let outbox_handle = match (&state.kafka_producer, config_arc.kafka.enabled) {
        (Some(producer), true) => {
            let dispatcher_db = state.db.clone();
            let dispatcher_publisher = Arc::new(KafkaPublisher((**producer).clone()));
            let dispatcher_shutdown = shutdown_rx.clone();
            let handle = tokio::spawn(async move {
                kafka::outbox::start_dispatcher(
                    dispatcher_db,
                    dispatcher_publisher,
                    dispatcher_shutdown,
                )
                .await;
            });
            Some(handle)
        }
        _ => {
            if config_arc.kafka.enabled {
                tracing::warn!(
                    "Kafka enabled but producer unavailable — outbox dispatcher not started"
                );
            }
            None
        }
    };

    // Build router
    let app = startup::build_router(state);

    // Start server
    let addr = format!("{}:{}", config_arc.server.host, config_arc.server.port);
    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("Server listening on {addr}");

    // `into_make_service_with_connect_info` exposes the peer `SocketAddr` to
    // handlers via `ConnectInfo`, which the rate limiter uses as the
    // trusted fallback when no proxy is configured.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;

    // ---------- orderly teardown ----------
    tracing::info!("HTTP server stopped, winding down background tasks");

    // Tell the Kafka consumer to drain and exit.
    let _ = shutdown_tx.send(true);

    // Bound the whole teardown by [`SHUTDOWN_DEADLINE`]. A stuck Kafka
    // handler or a query holding a pool connection indefinitely could
    // otherwise delay exit past the orchestrator's termination grace
    // period, forcing a SIGKILL and leaving resources in an unclean state.
    let teardown = async {
        // Drain in-flight background tasks (currently: password-reset email
        // sends) first, under its own 5s sub-budget. lettre's default SMTP
        // command timeout (60s) exceeds the whole-teardown deadline, so an
        // unbounded wait here could starve the Kafka joins and DB pool close
        // below. Email is best-effort: if the budget elapses, log and move
        // on rather than block shutdown on it.
        background_tasks.close();
        if tokio::time::timeout(Duration::from_secs(5), background_tasks.wait())
            .await
            .is_err()
        {
            tracing::warn!(
                "background task drain exceeded 5s budget; continuing shutdown with tasks still in flight"
            );
        }

        if let Some(handle) = consumer_handle {
            if let Err(e) = handle.await {
                tracing::warn!("Kafka consumer task exited with error: {e}");
            }
        }
        if let Some(handle) = outbox_handle {
            if let Err(e) = handle.await {
                tracing::warn!("Kafka outbox dispatcher exited with error: {e}");
            }
        }
        if let Err(e) = cleanup_handle.await {
            tracing::warn!("token cleanup task exited with error: {e}");
        }
        // Close the DB pool. This waits for in-flight queries to finish
        // (bounded by `statement_timeout` set in `after_connect`).
        db.close().await;
    };

    match tokio::time::timeout(SHUTDOWN_DEADLINE, teardown).await {
        Ok(()) => tracing::info!("Database pool closed, shutdown complete"),
        Err(_) => tracing::error!(
            deadline_seconds = SHUTDOWN_DEADLINE.as_secs(),
            "shutdown exceeded deadline; exiting with resources still draining"
        ),
    }

    Ok(())
}
