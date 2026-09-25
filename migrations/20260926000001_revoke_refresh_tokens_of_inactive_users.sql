-- =============================================================================
-- 補撤已停用帳號的 refresh token(漏洞 2 的存量修補)。
--
-- 此前 admin 停用帳號(`PATCH /users/{id}` `is_active: false`)只清 Redis
-- 快取,不撤 refresh token——token 只在使用者「停用期間」剛好呼叫
-- `/auth/refresh` 時才被惰性整族撤銷。停用期間沒 refresh 的帳號一旦被重新
-- 啟用,舊 refresh token 就原地復活。
--
-- 程式側已修:`users::service::admin_update_user` 停用時在同一 tx 內呼叫
-- `auth::session::end_all`。這支 migration 補的是修補前就已停用、仍掛著
-- 未撤 token 的存量帳號。冪等:重跑只會命中 0 列。
-- =============================================================================

UPDATE refresh_tokens rt
SET revoked = true
FROM users u
WHERE rt.user_id = u.id
  AND u.is_active = false
  AND rt.revoked = false;
