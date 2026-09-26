use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use uuid::Uuid;

use crate::error::AppError;
use crate::modules::auth::access;
use crate::state::AppState;
use crate::utils::jwt;

#[derive(Clone)]
pub struct AuthUser {
    pub user_id: Uuid,
    pub email: String,
    pub roles: Vec<String>,
}

impl AuthUser {
    pub fn require_role(&self, role: &str) -> Result<(), AppError> {
        if self.roles.iter().any(|r| r == role) {
            Ok(())
        } else {
            Err(AppError::Forbidden("insufficient permissions".into()))
        }
    }

    pub fn require_any_role(&self, roles: &[&str]) -> Result<(), AppError> {
        if roles.iter().any(|r| self.roles.iter().any(|ur| ur == r)) {
            Ok(())
        } else {
            Err(AppError::Forbidden("insufficient permissions".into()))
        }
    }

    pub fn is_admin(&self) -> bool {
        self.roles.iter().any(|r| r == "admin")
    }

    /// 資源所有權授權原語:呼叫者是 `owner_id` 本人或 admin 才放行,否則回傳
    /// `Err(AppError::Forbidden(forbidden_msg))`。文案由呼叫端傳入,讓各站點
    /// 收斂到同一判斷邏輯的同時仍保留各自逐字的錯誤訊息(先例見
    /// `coaches::service::require_course_coach`)。
    pub fn owns_or_admin(&self, owner_id: Uuid, forbidden_msg: &str) -> Result<(), AppError> {
        if self.user_id == owner_id || self.is_admin() {
            Ok(())
        } else {
            Err(AppError::Forbidden(forbidden_msg.into()))
        }
    }

    /// 非 owner 且非 admin → NotFound(not_found_msg):對外遮蔽資源存在性
    pub fn owns_or_admin_masked(&self, owner_id: Uuid, not_found_msg: &str) -> Result<(), AppError> {
        if self.user_id == owner_id || self.is_admin() {
            Ok(())
        } else {
            Err(AppError::NotFound(not_found_msg.into()))
        }
    }

    /// 僅資源本人可通過;admin 亦不可代 → 否則 Forbidden(forbidden_msg)
    pub fn owner_only(&self, owner_id: Uuid, forbidden_msg: &str) -> Result<(), AppError> {
        if self.user_id == owner_id {
            Ok(())
        } else {
            Err(AppError::Forbidden(forbidden_msg.into()))
        }
    }
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        // 0. 快路徑:route 層閘門(`require_admin`/`require_staff`/
        //    `require_coach`)已驗證過 token/角色並把 `AuthUser` 注入 request
        //    extensions。命中即 clone 回傳,handler 端的 extractor 不再重打
        //    一次 Redis/DB(閘門後所有已上移端點皆走此路)。閘門外的端點
        //    extensions 無此值,照常走下方完整流程。
        if let Some(cached) = parts.extensions.get::<AuthUser>() {
            return Ok(cached.clone());
        }

        // 1. Extract "Authorization: Bearer <token>" header
        let auth_header = parts
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .ok_or(AppError::Unauthorized)?;

        let token = auth_header
            .strip_prefix("Bearer ")
            .ok_or(AppError::Unauthorized)?;

        // 2. Decode JWT (signature + exp + aud + iss)
        let claims = jwt::decode_access_token(&state.config.auth, token)?;

        // 3. Parse user_id from claims.sub
        let user_id: Uuid = claims.sub.parse().map_err(|_| AppError::Unauthorized)?;

        // 4. Account access (is_active + roles), cache-first — see
        //    `auth::access::resolve`. `None` = deactivated or unknown user.
        let roles = access::resolve(&state.db, state.access_cache.as_ref(), user_id)
            .await
            .map_err(AppError::Database)?
            .ok_or(AppError::Unauthorized)?;

        // 5. Return AuthUser
        Ok(AuthUser {
            user_id,
            email: claims.email,
            roles,
        })
    }
}

/// 「需登入、不看角色」端點的具名閘門。掛在無閘門 router() 的 handler 用它,
/// 讓「這個參數就是登入檢查」在簽章可見——與 37 個已刪除的儀式參數區隔。
pub struct LoginRequired(pub AuthUser);

impl FromRequestParts<AppState> for LoginRequired {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        AuthUser::from_request_parts(parts, state).await.map(LoginRequired)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auth(user_id: Uuid, roles: &[&str]) -> AuthUser {
        AuthUser {
            user_id,
            email: "test@example.com".into(),
            roles: roles.iter().map(|r| (*r).to_string()).collect(),
        }
    }

    #[test]
    fn owner_non_admin_is_ok() {
        let id = Uuid::now_v7();
        let a = auth(id, &["member"]);
        assert!(a.owns_or_admin(id, "nope").is_ok());
    }

    #[test]
    fn non_owner_admin_is_ok() {
        let owner_id = Uuid::now_v7();
        let a = auth(Uuid::now_v7(), &["admin"]);
        assert!(a.owns_or_admin(owner_id, "nope").is_ok());
    }

    #[test]
    fn neither_owner_nor_admin_is_forbidden_with_message() {
        let owner_id = Uuid::now_v7();
        let a = auth(Uuid::now_v7(), &["member"]);
        let err = a
            .owns_or_admin(owner_id, "you shall not pass")
            .unwrap_err();
        assert!(matches!(err, AppError::Forbidden(ref m) if m == "you shall not pass"));
    }

    #[test]
    fn owner_and_admin_is_ok() {
        let id = Uuid::now_v7();
        let a = auth(id, &["admin"]);
        assert!(a.owns_or_admin(id, "nope").is_ok());
    }

    #[test]
    fn masked_owner_non_admin_is_ok() {
        let id = Uuid::now_v7();
        let a = auth(id, &["member"]);
        assert!(a.owns_or_admin_masked(id, "nope").is_ok());
    }

    #[test]
    fn masked_non_owner_admin_is_ok() {
        let owner_id = Uuid::now_v7();
        let a = auth(Uuid::now_v7(), &["admin"]);
        assert!(a.owns_or_admin_masked(owner_id, "nope").is_ok());
    }

    #[test]
    fn masked_neither_owner_nor_admin_is_not_found_with_message() {
        let owner_id = Uuid::now_v7();
        let a = auth(Uuid::now_v7(), &["member"]);
        let err = a
            .owns_or_admin_masked(owner_id, "resource not found")
            .unwrap_err();
        assert!(matches!(err, AppError::NotFound(ref m) if m == "resource not found"));
    }

    #[test]
    fn owner_only_owner_is_ok() {
        let id = Uuid::now_v7();
        let a = auth(id, &["member"]);
        assert!(a.owner_only(id, "nope").is_ok());
    }

    #[test]
    fn owner_only_admin_non_owner_is_forbidden() {
        let owner_id = Uuid::now_v7();
        let a = auth(Uuid::now_v7(), &["admin"]);
        let err = a.owner_only(owner_id, "僅本人可取消請假申請").unwrap_err();
        assert!(matches!(err, AppError::Forbidden(ref m) if m == "僅本人可取消請假申請"));
    }

    #[test]
    fn owner_only_neither_owner_nor_admin_is_forbidden_with_message() {
        let owner_id = Uuid::now_v7();
        let a = auth(Uuid::now_v7(), &["member"]);
        let err = a.owner_only(owner_id, "僅本人可預約補課").unwrap_err();
        assert!(matches!(err, AppError::Forbidden(ref m) if m == "僅本人可預約補課"));
    }
}
