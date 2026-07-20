//! Staff(admin 或 coach)角色閘門(route 層單點)。
//!
//! **為何是 route 層 middleware,而非逐 handler 首行**:~9 個 admin|coach
//! 粗粒度 handler(leave 清單/核決、今日課程、點名名冊/登記、成績單/證書
//! 建立、訂閱核銷、發文)過去每支開頭都重複
//! `auth.require_any_role(&["admin", "coach"])?;` 一行——與 `require_admin`
//! (見該檔頭註解)同一種儀式重複問題,同一種解法:上移到 route seam,
//! 授權判斷從「每支 handler 各自負責」收斂為掛在 staff 半邊 router 上的
//! 單一 layer。Request-data-dependent 的細粒度檢查(`require_course_coach`、
//! `is_admin()` 分支)不屬此類,留在 service;`rewards` 的條件式
//! `?all=true` 閘門依賴 query 參數,不併入此閘門,原地保留並加註解(見對應
//! handler)。`attendance::my_students`/`reports::coach_report` 原為
//! coach-only carve-out,現已獨立收斂至 `middleware::require_coach`
//! (`coach_api`),不再是本閘門的例外。
//!
//! **Fail-closed**:與 `require_admin` 相同的兩步短路——先
//! `AuthUser::from_request_parts`(401 平價),再角色判斷(403 平價),任一
//! 失敗即短路回錯誤,`next` 不執行;僅第二步換成
//! `require_any_role(&["admin", "coach"])`。驗證通過者才把 `AuthUser` 注入
//! extensions,供 handler 端 extractor 走快路徑,細節見 `require_admin`
//! 檔頭註解。
//!
//! **405 位移語意**:與 `require_admin` 相同(見該檔頭註解)——`route_layer`
//! 施加於 `staff_api` 時,該路徑上不存在的 method 的 405 fallback 會一併
//! 包進 layer,共用路徑上未定義的 method 對非 staff 呼叫者先撞 401/403。

use axum::extract::{FromRequestParts, Request, State};
use axum::middleware::Next;
use axum::response::Response;

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::state::AppState;

/// Route 層 staff 閘門:驗證 `AuthUser` 且具 `admin` 或 `coach` 角色,通過
/// 後把 `AuthUser` 注入 request extensions 再放行。掛在 `startup.rs` 的
/// `staff_api` 上(單點)。
pub async fn require_staff(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, AppError> {
    let (mut parts, body) = req.into_parts();
    let auth = AuthUser::from_request_parts(&mut parts, &state).await?; // 401 平價
    auth.require_any_role(&["admin", "coach"])?; // 403 平價
    let mut req = Request::from_parts(parts, body);
    req.extensions_mut().insert(auth);
    Ok(next.run(req).await)
}
