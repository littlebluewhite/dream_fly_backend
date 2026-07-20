//! Wiremock helpers for the Twilio Messages API — the SMS analogue of the
//! `google_token_url`/`google_jwks_url` config-URL seam already used for
//! Google OAuth (see the `/auth/google` section of `tests/http_auth.rs`).
//! `SmsClient` (`src/utils/sms.rs`) has no trait/mock seam: tests instead
//! point `SmsConfig::twilio_base_url` at a `MockServer` and exercise the
//! real client code — URL assembly, basic auth, form encoding, and the
//! non-2xx error branch — end to end.

#![allow(dead_code)]

use wiremock::matchers::{basic_auth, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A single outbound SMS as reconstructed from the form-encoded POST body
/// `SmsClient::send_sms` sends to Twilio's Messages endpoint.
#[derive(Debug, Clone)]
pub struct TwilioMessage {
    pub to: String,
    pub from: String,
    pub body: String,
}

/// Mount a 201-Created stub (Twilio's real success status) for the Messages
/// endpoint, matching the exact path + basic-auth credentials that
/// `common::http::test_app_config`'s `SmsConfig` uses (`test-sid` /
/// `test-token`). A request that doesn't match falls through to wiremock's
/// default 404, which `SmsClient` surfaces as a 500 — a 404 here means the
/// URL or basic-auth construction in `SmsClient` is broken, not that
/// SMS-sending failed for a legitimate reason.
pub async fn mount_twilio(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/2010-04-01/Accounts/test-sid/Messages.json"))
        .and(basic_auth("test-sid", "test-token"))
        .respond_with(ResponseTemplate::new(201))
        .mount(server)
        .await;
}

/// Every request `server` has received, decoded from
/// `application/x-www-form-urlencoded` into its `To`/`From`/`Body` fields.
pub async fn twilio_sent(server: &MockServer) -> Vec<TwilioMessage> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .map(|req| {
            let mut to = String::new();
            let mut from = String::new();
            let mut body = String::new();
            for (key, value) in url::form_urlencoded::parse(&req.body) {
                match key.as_ref() {
                    "To" => to = value.into_owned(),
                    "From" => from = value.into_owned(),
                    "Body" => body = value.into_owned(),
                    _ => {}
                }
            }
            TwilioMessage { to, from, body }
        })
        .collect()
}

/// Find the 6-digit OTP code embedded in an already-decoded SMS body, e.g.
/// `"Your Dream Fly verification code is: 483920. Valid for 5 minutes."`.
///
/// Callers must pass the isolated `Body` field (such as a [`TwilioMessage`]'s
/// `body`, from [`twilio_sent`]) — never the raw multi-field POST body.
/// Scanning the raw form-encoded body for 6 consecutive digits before
/// parsing it would false-match inside an adjacent field: the test
/// `twilio_from_number` alone, `From=%2B10000000000`, contains a run of 6+
/// digits that would win before the scan ever reached the real code in
/// `Body`.
pub fn extract_otp_code(body: &str) -> Option<String> {
    let bytes = body.as_bytes();
    for start in 0..bytes.len() {
        match bytes.get(start..start + 6) {
            Some(window) if window.iter().all(u8::is_ascii_digit) => {
                return Some(body[start..start + 6].to_string());
            }
            Some(_) => {}
            None => break,
        }
    }
    None
}
