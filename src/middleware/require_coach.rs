//! Coach 角色閘門(route 層單點)。
//!
//! **為何是 route 層 middleware,而非逐 handler 首行**:`attendance::my_students`、
//! `reports::coach_report` 兩支過去各自在 handler 首行留
//! `auth.require_role("coach")?;` 一行,並記錄「建第三個單角色 gate 不值得」
//! 的決定——彼時 carve-out 僅零星一兩處,不成規模。現況兩條路徑同形(皆
//! coach-only、admin 刻意排除),審查認定 route-table 可信度(掛在哪個
//! router 上即一望可知授權層級,不必逐支翻 handler 首行)已值得多開一個
//! gate 家族成員,故反轉該決定,比照 `require_admin`/`require_staff` 補上
//! 本檔——沿用兩者的兩步 fail-closed 結構,獨立成一個函式而非參數化
//! factory(理由同 `require_staff` 檔頭)。
//!
//! **Fail-closed**:與 `require_admin`/`require_staff` 相同的兩步短路——先
//! `AuthUser::from_request_parts`(401 平價),再角色判斷(403 平價),任一
//! 失敗即短路回錯誤,`next` 不執行;僅第二步換成 `require_role("coach")`
//! ——**不含 admin bypass**,兩條 carve-out 路徑語意皆為 admin 刻意排除。
//! 驗證通過者才把 `AuthUser` 注入 extensions,供 handler 端 extractor 走快
//! 路徑,細節見 `require_admin` 檔頭註解。
//!
//! **405 位移語意**:與 `require_admin` 相同(見該檔頭註解)——`route_layer`
//! 施加於 `coach_api` 時,該路徑上不存在的 method 的 405 fallback 會一併
//! 包進 layer,共用路徑上未定義的 method 對非 coach 呼叫者先撞 401/403。

use axum::extract::{FromRequestParts, Request, State};
use axum::middleware::Next;
use axum::response::Response;

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::state::AppState;

/// Route 層 coach 閘門:驗證 `AuthUser` 且具 `coach` 角色(不含 admin),通過
/// 後把 `AuthUser` 注入 request extensions 再放行。掛在 `startup.rs` 的
/// `coach_api` 上(單點)。
pub async fn require_coach(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, AppError> {
    let (mut parts, body) = req.into_parts();
    let auth = AuthUser::from_request_parts(&mut parts, &state).await?; // 401 平價
    auth.require_role("coach")?; // 403 平價
    let mut req = Request::from_parts(parts, body);
    req.extensions_mut().insert(auth);
    Ok(next.run(req).await)
}
