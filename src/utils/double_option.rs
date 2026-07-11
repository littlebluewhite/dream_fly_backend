use serde::{Deserialize, Deserializer};

/// Plain `Option<Option<T>>` cannot distinguish "key absent" from "key
/// present with JSON `null`" — serde's built-in `Option<T>` deserialize
/// collapses a `null` straight to the *outer* `None`, so a bare
/// `Option<Option<T>>` field could never actually clear a nullable column
/// back to `NULL` via PATCH. Paired with `#[serde(default)]`, this makes the
/// present-with-`null` case reach the *inner* `Option`, producing
/// `Some(None)` (clear) instead of `None` (don't touch) — mirrors
/// `venues::dto::deserialize_some` (venues d91ad85).
pub(crate) fn deserialize_some<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Deserialize::deserialize(deserializer).map(Some)
}
