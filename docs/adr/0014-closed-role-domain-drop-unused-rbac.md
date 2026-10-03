# ADR-0014: 角色是封閉值域;刪除未用的 RBAC 端點與資料表

## Context

授權實際上只看一件事:使用者在 `user_roles` 掛了哪幾個 `roles.name`。這四個名字(`admin`/`coach`/`member`/
`guest`)由 init migration 種下,程式裡以 17 個字串字面值散在 route 閘門(`require_admin`/`require_staff`/
`require_coach`)、`AuthUser::is_admin`、`rewards::list`、`messages::pairing`、四個角色授予站點與 seed 裡——
拼錯一個字(`"coaches"`)編譯照過,執行期就是 403 或默默授予不到角色(`assign_role_by_name` 遇到不存在的
名字 `INSERT … SELECT` 選不到列,回 `Ok`)。

同時,init migration 還建了一套「通用 RBAC」:`permissions`(resource × action)、`role_permissions`、
`permission_conditions`(JSONB 條件),以及 admin 端點 `/roles`(列出/建立角色、授予/撤銷使用者角色)。
這套東西從未被用上:

- 三張表沒有任何寫入者;唯一讀取者 `find_permissions_for_role` 只服務 `get_role_with_permissions`,而後者
  沒有掛上任何路由。任何授權判斷都不讀這三張表。
- `/roles` 前端沒有呼叫(規劃時已查)。`POST /roles` 能建出新角色名,但沒有任何程式碼會檢查那個名字,建了
  也不授予任何權限;`POST/DELETE /roles/{id}/users` 是 `coaches::service::create_coach` 之外第二條改角色的
  路徑,卻不在契約裡。

## Decision

**角色是封閉值域 `permissions::model::Role { Admin, Coach, Member, Guest }`,以 `ALL` + `as_str` 擁有值域,
比照其他 PG enum 的模式;刪除 `/roles` 端點群與從未使用的 RBAC 讀取路徑和三張空表。**

- `AuthUser::has(Role)`/`require_role(Role)`/`require_any_role(&[Role])`;`is_admin()` 保留為
  `has(Role::Admin)`。`AuthUser.roles` 維持 `Vec<String>`——它是 wire(`UserResponse.roles`)與
  access cache 的形狀,本次不動;比較一律經 `Role::as_str`。
- `permissions::repository::assign_role(conn, user, Role)` 取代 `assign_role_by_name`。以 CTE 先選角色列:
  角色列不存在 → `Err(sqlx::Error::RowNotFound)`(不再默默成功);已持有該角色的重複授予仍 `Ok`
  (`ON CONFLICT DO NOTHING`)。
- `roles` 仍是一張表(`user_roles.role_id` 的 FK 目標),不改成 PG enum。值域與表列的一致由
  `tests/wire_enums.rs::role_all_matches_roles_table` 釘住(集合相等;表無固有順序)。
- 刪除 `permissions::{handlers,routes,service,dto}`、repository 的 `find_all_roles`/`find_role_by_id`/
  `create_role`/`find_permissions_for_role`/`assign_role_to_user`/`remove_role_from_user`、
  `startup.rs` 的 `admin_router()` merge。permissions 模組只剩 `model`(`Role`)與 repository 的
  `assign_role` + 兩支角色名讀取。
- Migration `20261003000001_drop_unused_rbac_tables`:`DROP TABLE permission_conditions,
  role_permissions, permissions;`(不加 CASCADE)。`roles`、`user_roles` 保留——仍有讀寫者。

## 落選方案

- **把 `roles` 換成 PG enum 欄位(`user_roles.role role_name`)**:值域會由 `enum_range` 直接擁有,但要改
  `user_roles` 的主鍵與 FK、重寫兩支讀取查詢與 access cache 的 DB fallback,換來的只是把「表列 = `Role::ALL`」
  的測試換成「enum_range = `Role::ALL`」的測試——一致性本來就能用一條測試釘住,不值得一次資料遷移。
- **保留 `/roles` 與 RBAC 表,等日後細粒度權限再用**:沒有任何授權判斷讀它們;保留的代價是一條不在契約裡、
  能改角色卻不經 `coaches` 流程的 admin 路徑,以及能建出「沒人會檢查」角色名的端點。真要細粒度權限時,需求
  會決定表的形狀,不必先背著這套從未被驗證的 schema。
- **`AuthUser.roles` 改成 `Vec<Role>`**:會改 access cache 的序列化與 `UserResponse.roles` 的來源型別,波及
  wire;封閉值域的好處(呼叫端不能拼錯)已由 `has`/`require_*` 的參數型別拿到。

## Consequences

- 新增角色需要同時:新 migration 插入 `roles` 列、`Role` 加 variant(`ALL`/`as_str`)——漏任一邊,
  `role_all_matches_roles_table` 失敗。
- 若部署環境的 `roles` 表少了某列,授予該角色會回 `RowNotFound`(呼叫端轉成 500),不再默默授予失敗。
- 改使用者角色的路徑只剩程式內的授予站點(註冊/Google 首登 `member`、`POST /coaches` 的 `coach`、seed);
  沒有撤銷角色的端點。需要 admin 改角色時另開需求、寫進契約。
- `rg '"(admin|coach|member|guest)"' src` 只剩 `Role::as_str` 與 `#[cfg(test)]` 模組。
