// Canonical User struct lives in auth::model. Re-export here so that
// users::repository and users::dto can continue using `super::model::User`.
pub use crate::modules::auth::model::User;
