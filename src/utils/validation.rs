use std::collections::BTreeMap;

use axum::{
    Json,
    extract::{FromRequest, Request, rejection::JsonRejection},
};
use serde::de::DeserializeOwned;
use serde_json::json;
use validator::{Validate, ValidationErrors};

use crate::error::AppError;

pub struct ValidatedJson<T>(pub T);

impl<S, T> FromRequest<S> for ValidatedJson<T>
where
    T: DeserializeOwned + Validate,
    S: Send + Sync,
    Json<T>: FromRequest<S, Rejection = JsonRejection>,
{
    type Rejection = AppError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let Json(value) = Json::<T>::from_request(req, state).await.map_err(|e| {
            // Return a generic parse error; log the detailed rejection.
            tracing::debug!(error = %e, "JSON body rejected");
            AppError::Validation("invalid JSON body".to_string())
        })?;

        value
            .validate()
            .map_err(|e| AppError::Validation(format_validation_errors(&e)))?;

        Ok(ValidatedJson(value))
    }
}

/// Flatten a `ValidationErrors` tree into a single-line JSON string of the
/// form `{"field": ["error1", "error2"]}`. This is user-friendly, stable,
/// and doesn't leak `validator` internal struct formatting.
fn format_validation_errors(errors: &ValidationErrors) -> String {
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (field, field_errors) in errors.field_errors() {
        let messages: Vec<String> = field_errors
            .iter()
            .map(|err| {
                err.message
                    .clone()
                    .map(|c| c.into_owned())
                    .unwrap_or_else(|| err.code.clone().into_owned())
            })
            .collect();
        map.insert(field.to_string(), messages);
    }
    json!(map).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use validator::ValidationError;

    #[derive(Debug, Validate)]
    struct Demo {
        #[validate(length(min = 3, message = "too short"))]
        name: String,
        #[validate(email)]
        email: String,
    }

    #[test]
    fn format_errors_is_stable_sorted_json() {
        // Two fields are intentionally added in reverse alphabetical order
        // to prove the output is sorted (so snapshot tests or frontend
        // error rendering can rely on deterministic ordering).
        let bad = Demo {
            name: "ab".into(),
            email: "not-an-email".into(),
        };
        let errs = bad.validate().unwrap_err();
        let formatted = format_validation_errors(&errs);
        // `BTreeMap` guarantees alphabetical key order → email before name.
        let email_pos = formatted.find("\"email\"").expect("email key present");
        let name_pos = formatted.find("\"name\"").expect("name key present");
        assert!(email_pos < name_pos, "keys must be sorted alphabetically");
        assert!(formatted.contains("too short"));
    }

    #[test]
    fn format_errors_uses_code_when_message_missing() {
        // `#[validate(email)]` doesn't attach a custom message, so the
        // formatter should fall back to the error `code` ("email") rather
        // than producing an empty string.
        let mut errs = ValidationErrors::new();
        errs.add("email", ValidationError::new("email"));
        let formatted = format_validation_errors(&errs);
        assert!(
            formatted.contains("\"email\":[\"email\"]"),
            "expected code fallback, got {formatted}"
        );
    }

    #[test]
    fn format_errors_empty_input_is_empty_object() {
        let errs = ValidationErrors::new();
        assert_eq!(format_validation_errors(&errs), "{}");
    }
}
