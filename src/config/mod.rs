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

/// Normalizes the shape produced by the `config::Environment` source (see
/// `env_source()` below) for `APP__SERVER__ALLOWED_ORIGINS`.
///
/// `list_separator` is the only way to turn that env var into a `Vec<String>`,
/// but `str::split` always yields at least one item — splitting `""` by `,`
/// gives `[""]`, not `[]`. Without this, an operator explicitly setting the
/// env var to an empty string (meaning "no restricted origins") would instead
/// get a one-element vec containing an empty string. TOML-sourced values (a
/// native array, never a single empty string) pass through unchanged.
fn deserialize_allowed_origins<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let origins = Vec::<String>::deserialize(deserializer)?;
    Ok(match origins.as_slice() {
        [only] if only.is_empty() => Vec::new(),
        _ => origins,
    })
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
}

fn default_google_token_url() -> String {
    "https://oauth2.googleapis.com/token".to_string()
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
}

impl fmt::Debug for SmsConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SmsConfig")
            .field("twilio_account_sid", &self.twilio_account_sid)
            .field("twilio_auth_token", &"[REDACTED]")
            .field("twilio_from_number", &self.twilio_from_number)
            .finish()
    }
}

/// Environment-variable source shared by `AppConfig::load()` and its tests,
/// so tests exercise the exact list-parsing configuration used at runtime.
///
/// `list_separator` only takes effect when `try_parsing` is enabled (`config`
/// 0.15 `Environment::list_separator` docs), and is scoped to
/// `server.allowed_origins` via `with_list_parse_key` so no other config
/// field is affected by the added parsing.
fn env_source() -> config::Environment {
    config::Environment::default()
        .separator("__")
        .prefix("APP")
        .try_parsing(true)
        .list_separator(",")
        .with_list_parse_key("server.allowed_origins")
}

impl AppConfig {
    pub fn load() -> Result<Self, config::ConfigError> {
        let env = std::env::var("APP_ENV").unwrap_or_else(|_| "development".to_string());

        let config = config::Config::builder()
            .add_source(config::File::with_name("config/default"))
            .add_source(config::File::with_name(&format!("config/{env}")).required(false))
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
        if env != "development" && app_config.auth.jwt_secret.len() < 32 {
            return Err(config::ConfigError::Message(
                "auth.jwt_secret must be at least 32 characters outside development".to_string(),
            ));
        }

        // Reject shipped example / placeholder strings so they can't reach a
        // running server even if someone forgets to override them.
        const FORBIDDEN_SECRETS: &[&str] = &[
            "change-me-in-production-use-a-long-random-string",
            "change-me",
            "your-secret-here",
        ];
        if env != "development"
            && FORBIDDEN_SECRETS
                .iter()
                .any(|f| app_config.auth.jwt_secret == *f)
        {
            return Err(config::ConfigError::Message(
                "auth.jwt_secret is a placeholder value; refusing to start".to_string(),
            ));
        }

        Ok(app_config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Debug, Deserialize)]
    struct ServerOnly {
        server: ServerConfig,
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
}
