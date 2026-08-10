use std::fmt;

use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub redis: RedisConfig,
    pub kafka: KafkaConfig,
    pub auth: AuthConfig,
    pub email: EmailConfig,
    pub sms: SmsConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    #[serde(default, deserialize_with = "deserialize_allowed_origins")]
    pub allowed_origins: Vec<String>,
    /// Whether to trust `X-Forwarded-For`/`X-Real-IP` headers for client IP
    /// extraction. Enable only when the server is behind a reverse proxy
    /// that strips/rewrites these headers for untrusted clients.
    #[serde(default)]
    pub trust_proxy: bool,
    /// IANA timezone name for the studio (e.g. "Asia/Taipei"). Used for
    /// human-facing rules such as the 24-hour cancellation window, where
    /// the stored naïve `date` + `time` must be interpreted in the studio's
    /// local time. Defaults to `UTC` if unset.
    #[serde(default = "default_studio_timezone")]
    pub studio_timezone: String,
}

fn default_studio_timezone() -> String {
    "UTC".to_string()
}

/// Accepts either shape that can reach `server.allowed_origins`:
///   - a comma-separated **string** from the `APP__SERVER__ALLOWED_ORIGINS`
///     env var (the `config` env source hands every var over as a raw string),
///   - a native **array** from a `config/*.toml` overlay.
///
/// The env source is deliberately left with no `try_parsing`/`list_separator`
/// (those are process-global and would corrupt other `String` fields, e.g.
/// mangling an E.164 `+1…` phone number into an integer), so the comma-split
/// is done here instead. An empty string means "no restricted origins" and
/// collapses to an empty `Vec` rather than `[""]`.
fn deserialize_allowed_origins<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct AllowedOrigins;

    impl<'de> serde::de::Visitor<'de> for AllowedOrigins {
        type Value = Vec<String>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a comma-separated string or a list of origin strings")
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(if value.is_empty() {
                Vec::new()
            } else {
                value.split(',').map(str::to_owned).collect()
            })
        }

        fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            self.visit_str(&value)
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: serde::de::SeqAccess<'de>,
        {
            let mut origins = Vec::new();
            while let Some(origin) = seq.next_element::<String>()? {
                origins.push(origin);
            }
            Ok(origins)
        }
    }

    deserializer.deserialize_any(AllowedOrigins)
}

#[derive(Debug, Deserialize, Clone)]
pub struct DatabaseConfig {
    pub url: String,
    pub max_connections: u32,
    pub min_connections: u32,
}

#[derive(Debug, Deserialize, Clone)]
pub struct RedisConfig {
    pub url: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct KafkaConfig {
    pub brokers: String,
    pub group_id: String,
    pub enabled: bool,
}

#[derive(Deserialize, Clone)]
pub struct AuthConfig {
    pub jwt_secret: String,
    pub jwt_access_expiration_minutes: u64,
    pub jwt_refresh_expiration_days: u64,
    pub google_client_id: String,
    pub google_client_secret: String,
    pub google_redirect_url: String,
    /// Google OAuth token exchange endpoint. Defaults to the real
    /// `https://oauth2.googleapis.com/token`; integration tests override this
    /// via `APP__AUTH__GOOGLE_TOKEN_URL` to point at a `wiremock` server.
    #[serde(default = "default_google_token_url")]
    pub google_token_url: String,
    /// Google's published JWKS (public signing keys) endpoint, used to verify
    /// id_token signatures. Defaults to the real
    /// `https://www.googleapis.com/oauth2/v3/certs`; integration tests
    /// override this via `APP__AUTH__GOOGLE_JWKS_URL` to point at a
    /// `wiremock` server.
    #[serde(default = "default_google_jwks_url")]
    pub google_jwks_url: String,
}

fn default_google_token_url() -> String {
    "https://oauth2.googleapis.com/token".to_string()
}

fn default_google_jwks_url() -> String {
    "https://www.googleapis.com/oauth2/v3/certs".to_string()
}

impl fmt::Debug for AuthConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthConfig")
            .field("jwt_secret", &"[REDACTED]")
            .field("jwt_access_expiration_minutes", &self.jwt_access_expiration_minutes)
            .field("jwt_refresh_expiration_days", &self.jwt_refresh_expiration_days)
            .field("google_client_id", &self.google_client_id)
            .field("google_client_secret", &"[REDACTED]")
            .field("google_redirect_url", &self.google_redirect_url)
            .field("google_token_url", &self.google_token_url)
            .field("google_jwks_url", &self.google_jwks_url)
            .finish()
    }
}

#[derive(Deserialize, Clone)]
pub struct EmailConfig {
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_username: String,
    pub smtp_password: String,
    pub from_email: String,
    pub from_name: String,
}

impl fmt::Debug for EmailConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EmailConfig")
            .field("smtp_host", &self.smtp_host)
            .field("smtp_port", &self.smtp_port)
            .field("smtp_username", &self.smtp_username)
            .field("smtp_password", &"[REDACTED]")
            .field("from_email", &self.from_email)
            .field("from_name", &self.from_name)
            .finish()
    }
}

#[derive(Deserialize, Clone)]
pub struct SmsConfig {
    pub twilio_account_sid: String,
    pub twilio_auth_token: String,
    pub twilio_from_number: String,
    /// Twilio Messages API base URL. Defaults to the real
    /// `https://api.twilio.com`; integration tests override this via
    /// `APP__SMS__TWILIO_BASE_URL` to point at a `wiremock` server.
    #[serde(default = "default_twilio_base_url")]
    pub twilio_base_url: String,
}

fn default_twilio_base_url() -> String {
    "https://api.twilio.com".to_string()
}

impl fmt::Debug for SmsConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SmsConfig")
            .field("twilio_account_sid", &self.twilio_account_sid)
            .field("twilio_auth_token", &"[REDACTED]")
            .field("twilio_from_number", &self.twilio_from_number)
            .field("twilio_base_url", &self.twilio_base_url)
            .finish()
    }
}

/// Environment-variable source shared by `AppConfig::load()` and its tests,
/// so tests exercise the exact configuration used at runtime.
///
/// Intentionally does NOT enable `try_parsing`/`list_separator`: those apply to
/// every `APP__*` key process-wide and would coerce non-list `String` fields
/// (e.g. an E.164 `+1…` phone number parsed as an integer, losing the `+`).
/// The one field that needs list handling, `server.allowed_origins`, is parsed
/// from its raw string in `deserialize_allowed_origins` instead.
fn env_source() -> config::Environment {
    config::Environment::default()
        .separator("__")
        .prefix("APP")
}

/// The running deployment environment (`APP_ENV`), e.g. `development`,
/// `staging`, `production`. Single owner for reading/comparing the env name
/// — previously read and compared ad hoc (and case-sensitively) at four
/// separate sites: this module's `load()`, `main.rs`'s production guard,
/// `main.rs`'s log-format switch, and `bin/seed.rs`'s production refusal.
///
/// Not to be confused with the `config` crate's `config::Environment`
/// (`env_source()` above) — that's a config *source* that reads
/// `APP__*`-prefixed process env vars into the config tree. This type is
/// unrelated: it's just "which deployment tier is this process running as".
pub struct AppEnv(String);

impl AppEnv {
    /// Reads `APP_ENV`; absent defaults to `"development"`.
    pub fn from_env() -> Self {
        Self(std::env::var("APP_ENV").unwrap_or_else(|_| "development".to_string()))
    }

    /// Test/construction helper — builds directly from a raw string.
    pub fn from_raw(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    /// The raw string as configured, case preserved. Used for the
    /// `config/{env}.toml` overlay filename, which must match the file on
    /// disk verbatim.
    pub fn raw(&self) -> &str {
        &self.0
    }

    /// Case-insensitive: `development`/`Development`/`DEVELOPMENT` all match.
    pub fn is_development(&self) -> bool {
        self.0.eq_ignore_ascii_case("development")
    }

    /// Case-insensitive: `production`/`Production`/`PRODUCTION` all match.
    pub fn is_production(&self) -> bool {
        self.0.eq_ignore_ascii_case("production")
    }
}

impl AppConfig {
    pub fn load() -> Result<Self, config::ConfigError> {
        let env = AppEnv::from_env();

        let config = config::Config::builder()
            .add_source(config::File::with_name("config/default"))
            .add_source(config::File::with_name(&format!("config/{}", env.raw())).required(false))
            .add_source(env_source())
            .build()?;

        let app_config: Self = config.try_deserialize()?;

        if app_config.auth.jwt_secret.is_empty() {
            return Err(config::ConfigError::Message(
                "auth.jwt_secret must be set — use APP__AUTH__JWT_SECRET env var or config overlay".to_string(),
            ));
        }

        // Fail at startup rather than silently fall back to UTC at request
        // time — a misspelled `server.studio_timezone` would otherwise
        // produce bookings offset by hours with no operator-visible signal.
        if app_config
            .server
            .studio_timezone
            .parse::<chrono_tz::Tz>()
            .is_err()
        {
            return Err(config::ConfigError::Message(format!(
                "server.studio_timezone '{}' is not a valid IANA timezone name",
                app_config.server.studio_timezone
            )));
        }

        // 32 bytes is the minimum useful HS256 key length (equal to the
        // output size of the HMAC). Anything shorter is trivially
        // brute-forceable offline given any captured token.
        if !env.is_development() && app_config.auth.jwt_secret.len() < 32 {
            return Err(config::ConfigError::Message(
                "auth.jwt_secret must be at least 32 characters outside development".to_string(),
            ));
        }

        // Reject shipped example / placeholder strings so they can't reach a
        // running server even if someone forgets to override them.
        if !env.is_development() && jwt_secret_is_placeholder(&app_config.auth.jwt_secret) {
            return Err(config::ConfigError::Message(
                "auth.jwt_secret is a placeholder value; refusing to start".to_string(),
            ));
        }

        Ok(app_config)
    }
}

/// Placeholder/example JWT secrets that must never reach a running server:
/// the exact strings shipped in docs/examples, plus anything containing
/// `dev-only` (the convention used by local `.env` files). Shared by
/// `AppConfig::load()`'s own check above and `validate_production_config`'s
/// guard below — previously two separately-maintained lists (an exact-match
/// set here, a `contains("dev-only") || == "change-me"` check in `main.rs`).
fn jwt_secret_is_placeholder(secret: &str) -> bool {
    const FORBIDDEN_SECRETS: &[&str] = &[
        "change-me-in-production-use-a-long-random-string",
        "change-me",
        "your-secret-here",
    ];
    FORBIDDEN_SECRETS.contains(&secret) || secret.contains("dev-only")
}

/// Guard against footguns that have historically shipped to prod: weak JWT
/// secrets, empty CORS whitelist, dev-only secrets leaking into prod,
/// localhost DB/Redis URLs, missing SMTP credentials.
pub fn validate_production_config(config: &AppConfig, env: &AppEnv) -> anyhow::Result<()> {
    if !env.is_production() {
        // Surface a footgun that applies in every env: trusting XFF without
        // a reverse proxy that strips it lets clients spoof per-IP rate
        // limits. We emit a warn rather than bail because legit dev setups
        // (tunnels, staging behind a proxy) may legitimately want it on.
        if config.server.trust_proxy {
            tracing::warn!(
                "APP__SERVER__TRUST_PROXY=true — this server is trusting X-Forwarded-For. \
                 Per-IP rate limits can be spoofed unless a reverse proxy strips the header \
                 for untrusted clients."
            );
        }
        return Ok(());
    }

    if config.auth.jwt_secret.len() < 32 {
        anyhow::bail!(
            "APP_ENV=production but auth.jwt_secret is shorter than 32 chars. \
             Set APP__AUTH__JWT_SECRET to a long random string."
        );
    }
    if jwt_secret_is_placeholder(&config.auth.jwt_secret) {
        anyhow::bail!(
            "APP_ENV=production but auth.jwt_secret looks like a placeholder. Refusing to start."
        );
    }
    if config.server.allowed_origins.is_empty() {
        anyhow::bail!(
            "APP_ENV=production but server.allowed_origins is empty. \
             This would serve any origin via CORS. Set APP__SERVER__ALLOWED_ORIGINS."
        );
    }

    // A localhost DB or Redis URL in production almost certainly means the
    // env files weren't overridden at deploy time. The service would start,
    // then fail on the first request with a connection error from outside
    // the pod's network namespace.
    if config.database.url.contains("localhost") || config.database.url.contains("127.0.0.1") {
        anyhow::bail!(
            "APP_ENV=production but database.url points at localhost. \
             This is almost always a config-overlay mistake. Set APP__DATABASE__URL."
        );
    }
    if config.redis.url.contains("localhost") || config.redis.url.contains("127.0.0.1") {
        anyhow::bail!(
            "APP_ENV=production but redis.url points at localhost. \
             Set APP__REDIS__URL to the production Redis endpoint."
        );
    }

    if config.email.smtp_password.is_empty() {
        anyhow::bail!(
            "APP_ENV=production but email.smtp_password is empty. \
             Password reset and OTP emails would fail silently. Set APP__EMAIL__SMTP_PASSWORD."
        );
    }

    if config.auth.google_client_id.is_empty() || config.auth.google_client_secret.is_empty() {
        tracing::warn!(
            "APP_ENV=production but Google OAuth credentials are missing. \
             `/auth/google` will fail until APP__AUTH__GOOGLE_CLIENT_{{ID,SECRET}} are set."
        );
    }

    if config.server.trust_proxy {
        tracing::info!(
            "APP__SERVER__TRUST_PROXY=true — relying on upstream proxy to strip \
             X-Forwarded-For for untrusted clients. Verify this is the case."
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Debug, Deserialize)]
    struct ServerOnly {
        server: ServerConfig,
    }

    #[derive(Debug, Deserialize)]
    struct SmsOnly {
        sms: SmsConfig,
    }

    /// Builds a `ServerConfig` through the same `env_source()` builder used by
    /// `AppConfig::load()`, injecting an in-memory source (`Environment::source`)
    /// instead of mutating real process env vars — keeps tests isolated from
    /// each other since env vars are process-global.
    fn allowed_origins_from_env(value: &str) -> Vec<String> {
        let mut source = HashMap::new();
        source.insert("APP__SERVER__HOST".to_string(), "0.0.0.0".to_string());
        source.insert("APP__SERVER__PORT".to_string(), "3000".to_string());
        source.insert("APP__SERVER__ALLOWED_ORIGINS".to_string(), value.to_string());

        let config = config::Config::builder()
            .add_source(env_source().source(Some(source)))
            .build()
            .expect("config should build from injected in-memory source");

        let parsed: ServerOnly = config
            .try_deserialize()
            .expect("ServerConfig should deserialize from injected source");

        parsed.server.allowed_origins
    }

    #[test]
    fn empty_env_var_deserializes_to_empty_vec() {
        assert_eq!(allowed_origins_from_env(""), Vec::<String>::new());
    }

    #[test]
    fn comma_separated_env_var_deserializes_to_two_element_vec() {
        assert_eq!(
            allowed_origins_from_env("http://a.com,http://b.com"),
            vec!["http://a.com".to_string(), "http://b.com".to_string()]
        );
    }

    #[test]
    fn single_origin_without_comma_is_one_element_vec() {
        assert_eq!(
            allowed_origins_from_env("http://a.com"),
            vec!["http://a.com".to_string()]
        );
    }

    /// Regression guard for the `try_parsing` footgun: an E.164 phone number
    /// (`+14155551234`, the only format Twilio accepts) is a `String` field.
    /// With `Environment::try_parsing(true)` the `config` crate greedily parses
    /// it as an `i64` — dropping the leading `+` — so it must stay off. Injected
    /// through the same `env_source()` builder used at runtime; fails loudly if
    /// process-wide numeric parsing is ever re-introduced.
    #[test]
    fn e164_phone_number_survives_config_load_verbatim() {
        let mut source = HashMap::new();
        source.insert(
            "APP__SMS__TWILIO_ACCOUNT_SID".to_string(),
            "AC_test".to_string(),
        );
        source.insert(
            "APP__SMS__TWILIO_AUTH_TOKEN".to_string(),
            "tok_test".to_string(),
        );
        source.insert(
            "APP__SMS__TWILIO_FROM_NUMBER".to_string(),
            "+14155551234".to_string(),
        );

        let config = config::Config::builder()
            .add_source(env_source().source(Some(source)))
            .build()
            .expect("config should build from injected in-memory source");

        let parsed: SmsOnly = config
            .try_deserialize()
            .expect("SmsConfig should deserialize from injected source");

        assert_eq!(parsed.sms.twilio_from_number, "+14155551234");
    }

    /// Default-value regression: a typo in `default_twilio_base_url()` would
    /// silently redirect production SMS to the wrong domain, and `serde`
    /// gives no compile-time or runtime signal when a
    /// `#[serde(default = ...)]` function's return value is wrong — only an
    /// explicit assertion catches it.
    #[test]
    fn twilio_base_url_defaults_to_real_twilio_api() {
        let mut source = HashMap::new();
        source.insert(
            "APP__SMS__TWILIO_ACCOUNT_SID".to_string(),
            "AC_test".to_string(),
        );
        source.insert(
            "APP__SMS__TWILIO_AUTH_TOKEN".to_string(),
            "tok_test".to_string(),
        );
        source.insert(
            "APP__SMS__TWILIO_FROM_NUMBER".to_string(),
            "+14155551234".to_string(),
        );
        // Deliberately no APP__SMS__TWILIO_BASE_URL — exercises the
        // `#[serde(default = "default_twilio_base_url")]` fallback.

        let config = config::Config::builder()
            .add_source(env_source().source(Some(source)))
            .build()
            .expect("config should build from injected in-memory source");

        let parsed: SmsOnly = config
            .try_deserialize()
            .expect("SmsConfig should deserialize from injected source");

        assert_eq!(parsed.sms.twilio_base_url, "https://api.twilio.com");
    }

    // ------------------------------------------------------------------
    // AppEnv
    // ------------------------------------------------------------------

    #[test]
    fn app_env_is_production_matches_case_insensitively() {
        assert!(AppEnv::from_raw("production").is_production());
        assert!(AppEnv::from_raw("Production").is_production());
        assert!(AppEnv::from_raw("PRODUCTION").is_production());
    }

    #[test]
    fn app_env_is_development_matches_case_insensitively() {
        assert!(AppEnv::from_raw("development").is_development());
        assert!(AppEnv::from_raw("Development").is_development());
        assert!(AppEnv::from_raw("DEVELOPMENT").is_development());
    }

    #[test]
    fn app_env_from_env_defaults_to_development_when_unset() {
        // `APP_ENV` is process-global; save/restore around the mutation.
        // Safe in this binary: no other unit test in `src/**` reads any env
        // var (`AppEnv::from_env`'s only other caller, `AppConfig::load`, is
        // never invoked from a unit test — integration tests under `tests/`
        // build `AppConfig` by hand and run as separate processes anyway, so
        // they can't race with this one regardless).
        let original = std::env::var("APP_ENV").ok();
        unsafe {
            std::env::remove_var("APP_ENV");
        }

        let env = AppEnv::from_env();

        if let Some(val) = original {
            unsafe {
                std::env::set_var("APP_ENV", val);
            }
        }

        assert!(env.is_development());
    }

    #[test]
    fn app_env_staging_is_neither_production_nor_development() {
        let env = AppEnv::from_raw("staging");
        assert!(!env.is_production());
        assert!(!env.is_development());
    }

    // ------------------------------------------------------------------
    // validate_production_config
    // ------------------------------------------------------------------

    /// A hand-built `AppConfig` that satisfies every `validate_production_config`
    /// guard — the baseline for the per-guard cases below, each of which
    /// clones it and breaks exactly one field. Field shapes mirror
    /// `tests/common/http.rs::test_app_config` (not reusable here — `tests/`
    /// isn't reachable from this unit-test module), tuned to pass every
    /// guard rather than to run against local infra.
    fn valid_prod_config() -> AppConfig {
        AppConfig {
            server: ServerConfig {
                host: "0.0.0.0".into(),
                port: 3000,
                allowed_origins: vec!["https://dreamfly.tw".into()],
                trust_proxy: false,
                studio_timezone: "Asia/Taipei".into(),
            },
            database: DatabaseConfig {
                url: "postgres://prod-db.internal:5432/dream_fly".into(),
                max_connections: 10,
                min_connections: 2,
            },
            redis: RedisConfig {
                url: "redis://prod-redis.internal:6379".into(),
            },
            kafka: KafkaConfig {
                brokers: "prod-kafka.internal:9092".into(),
                group_id: "dreamfly".into(),
                enabled: false,
            },
            auth: AuthConfig {
                jwt_secret: "a-sufficiently-long-random-production-secret-1234".into(),
                jwt_access_expiration_minutes: 15,
                jwt_refresh_expiration_days: 30,
                google_client_id: "prod-client-id".into(),
                google_client_secret: "prod-client-secret".into(),
                google_redirect_url: "https://dreamfly.tw/oauth/callback".into(),
                google_token_url: "https://oauth2.googleapis.com/token".into(),
                google_jwks_url: "https://www.googleapis.com/oauth2/v3/certs".into(),
            },
            email: EmailConfig {
                smtp_host: "smtp.dreamfly.tw".into(),
                smtp_port: 587,
                smtp_username: "noreply@dreamfly.tw".into(),
                smtp_password: "s3cr3t-smtp-password".into(),
                from_email: "noreply@dreamfly.tw".into(),
                from_name: "Dream Fly".into(),
            },
            sms: SmsConfig {
                twilio_account_sid: "AC_prod".into(),
                twilio_auth_token: "prod-token".into(),
                twilio_from_number: "+14155551234".into(),
                twilio_base_url: "https://api.twilio.com".into(),
            },
        }
    }

    #[test]
    fn validate_production_config_ok_for_non_production_even_with_weak_secret() {
        let mut config = valid_prod_config();
        config.auth.jwt_secret = "short".into();
        assert!(validate_production_config(&config, &AppEnv::from_raw("staging")).is_ok());
    }

    #[test]
    fn validate_production_config_rejects_short_jwt_secret() {
        let mut config = valid_prod_config();
        config.auth.jwt_secret = "too-short".into();
        assert!(validate_production_config(&config, &AppEnv::from_raw("production")).is_err());
    }

    #[test]
    fn validate_production_config_rejects_placeholder_jwt_secret() {
        let mut config = valid_prod_config();
        config.auth.jwt_secret = "change-me-in-production-use-a-long-random-string".into();
        assert!(validate_production_config(&config, &AppEnv::from_raw("production")).is_err());
    }

    #[test]
    fn validate_production_config_rejects_empty_allowed_origins() {
        let mut config = valid_prod_config();
        config.server.allowed_origins = vec![];
        assert!(validate_production_config(&config, &AppEnv::from_raw("production")).is_err());
    }

    #[test]
    fn validate_production_config_rejects_localhost_database_url() {
        let mut config = valid_prod_config();
        config.database.url = "postgres://localhost:5432/dream_fly".into();
        assert!(validate_production_config(&config, &AppEnv::from_raw("production")).is_err());
    }

    #[test]
    fn validate_production_config_rejects_localhost_redis_url() {
        let mut config = valid_prod_config();
        config.redis.url = "redis://localhost:6379".into();
        assert!(validate_production_config(&config, &AppEnv::from_raw("production")).is_err());
    }

    #[test]
    fn validate_production_config_rejects_empty_smtp_password() {
        let mut config = valid_prod_config();
        config.email.smtp_password = String::new();
        assert!(validate_production_config(&config, &AppEnv::from_raw("production")).is_err());
    }

    #[test]
    fn validate_production_config_ok_when_all_guards_satisfied() {
        let config = valid_prod_config();
        assert!(validate_production_config(&config, &AppEnv::from_raw("production")).is_ok());
    }

    // ------------------------------------------------------------------
    // jwt_secret_is_placeholder
    // ------------------------------------------------------------------

    #[test]
    fn jwt_secret_is_placeholder_matches_known_placeholders() {
        assert!(jwt_secret_is_placeholder(
            "change-me-in-production-use-a-long-random-string"
        ));
        assert!(jwt_secret_is_placeholder("change-me"));
        assert!(jwt_secret_is_placeholder("your-secret-here"));
        assert!(jwt_secret_is_placeholder("my-dev-only-secret-key"));
    }

    #[test]
    fn jwt_secret_is_placeholder_rejects_qualified_secret() {
        assert!(!jwt_secret_is_placeholder(
            "a-sufficiently-long-random-production-secret-1234"
        ));
    }
}
