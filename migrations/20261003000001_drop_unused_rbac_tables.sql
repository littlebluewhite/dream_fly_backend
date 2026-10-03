-- =============================================================================
-- 刪除未用的 RBAC 表:permission_conditions、role_permissions、permissions。
--
-- 三張表自 init migration 建立以來沒有任何寫入者(沒有端點、seed、service
-- 寫過它們),唯一的讀取者是 `permissions::repository::find_permissions_for_role`
-- ——它只服務已刪除的 `/roles` 端點群(`permissions::{handlers,routes,service,dto}`),
-- 隨本次一併刪除。授權只看 `user_roles` → `roles.name`(封閉值域 `Role`,
-- ADR-0014),因此 `roles` 與 `user_roles` 保留。
--
-- 一條 DROP TABLE 同時刪三張表,FK(兩張關聯表 → permissions)在同一陳述式內
-- 一起解除;刻意不加 CASCADE——若有預期外的相依物件(view、FK),migration
-- 失敗而不是默默連帶刪除。兩個 `idx_permission_conditions_*` 索引隨表刪除。
-- =============================================================================

DROP TABLE permission_conditions, role_permissions, permissions;
