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
}
