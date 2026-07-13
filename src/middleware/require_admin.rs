//! Admin 角色閘門(route 層單點)。
//!
//! **為何是 route 層 middleware,而非逐 handler 首行**:34 個 admin-only handler
//! 過去每支開頭都重複 `auth.require_role("admin")?;` 一行。這一行是「儀式」——
//! 純閘門、無業務語意,卻散落在 16 個模組、任何一支漏寫就是靜默開放。上移到
//! route seam 後,授權判斷從「每支 handler 各自負責」收斂為「掛在 admin 半邊
//! router 上的單一 layer」:admin_router() 的存在本身即是授權契約,新增 admin
//! 端點只需掛進 admin 半邊,不可能再漏寫首行。
//!
//! **Fail-closed**:閘門先 `AuthUser::from_request_parts`(維持 401 平價——
//! 無 token / token 失效與原逐 handler extractor 同一 `AppError` 路徑),再
//! `require_role("admin")`(維持 403 平價)。兩步任一失敗即短路回錯誤,`next`
//! 不執行——請求到不了 handler。驗證通過者才把 `AuthUser` 注入 extensions,
//! 供 handler 端 extractor 走快路徑(見 `extractors::auth` 的 fast-path),
//! 閘門後零額外 Redis/DB。
//!
//! **405 位移語意(記錄,非缺陷)**:`route_layer` 施加於 `admin_api` 時,axum
//! 的 `MethodRouter::layer` 會連同「該路徑上不存在的 method 的 405 fallback」
//! 一起包進 layer。因此共用/admin 路徑上一個未定義的 method,對非 admin 呼叫者
//! 會先撞閘門(401/403)而非 405 Method Not Allowed。全 repo 無 405 斷言、
//! 契約亦未文件化 405,故無破壞;公開 sibling 方法(由另一個 public router 帶入
//! 同路徑)不在此 layer 內,不受影響。

use axum::extract::{FromRequestParts, Request, State};
use axum::middleware::Next;
use axum::response::Response;

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::state::AppState;

/// Route 層 admin 閘門:驗證 `AuthUser` 且具 `admin` 角色,通過後把 `AuthUser`
/// 注入 request extensions 再放行。掛在 `startup.rs` 的 `admin_api` 上(單點)。
pub async fn require_admin(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, AppError> {
    let (mut parts, body) = req.into_parts();
    let auth = AuthUser::from_request_parts(&mut parts, &state).await?; // 401 平價
    auth.require_role("admin")?; // 403 平價
    let mut req = Request::from_parts(parts, body);
    req.extensions_mut().insert(auth);
    Ok(next.run(req).await)
}
