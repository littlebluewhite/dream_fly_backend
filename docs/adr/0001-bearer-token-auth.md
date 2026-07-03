# ADR-0001: 前端用 Bearer Token（記憶體 + localStorage），不做 Cookie/BFF

## Context

後端已實作 JWT 雙 token 機制：access token（15 分鐘）+ refresh token（30 天，輪替、重用即整個裝置家族撤銷）。前端（Task 11 起）需要決定怎麼保存與夾帶這兩顆 token。常見選項有三：

1. **Bearer token，前端自行保存**（access 存記憶體、refresh 存 `localStorage`），每個請求手動加 `Authorization` header。
2. **HttpOnly Cookie**，瀏覽器自動夾帶，後端需另立 CSRF 防護（double-submit token 或 `SameSite` 策略）。
3. **BFF（Backend for Frontend）**：前端框架的伺服器端（如 Next.js API route）代管 token，瀏覽器只跟 BFF 交換 session cookie，BFF 再用 Bearer 呼叫本後端。

本專案前端是純 SPA（Vite/React，無自帶 server-side runtime），且後端已經是純 API 服務（無 cookie 簽發邏輯、無 CSRF middleware）。

## Decision

採用 **方案 1：Bearer token，前端自管**。

- Access token 存在**記憶體**（JS 變數/store，不落地）——分頁重整即遺失，靠 refresh token 換回。
- Refresh token 存 **`localStorage`**——換取「重整頁面不用重新登入」，代價是接受 XSS 情境下 refresh token 可能被讀取（此風險由後端的輪替 + 重用偵測家族撤銷機制降低影響半徑：一旦偷到的 token 被使用過一次，原裝置下次 refresh 會失敗並觸發全家族撤銷）。
- 每個需認證的請求由 HTTP client 攔截器自動加上 `Authorization: Bearer <access_token>`。
- Access token 過期（401）時，攔截器觸發 **single-flight** refresh：同一時間只送出一個 `/auth/refresh` 請求，其餘並發的 401 請求排隊等這次 refresh 結果，成功後重放，失敗則導回登入頁——避免多個並發請求同時各自觸發 refresh，彼此用掉對方的 rotation 而互相撤銷。

## Consequences

- **不需要**後端新增 CSRF token 端點、`SameSite`/`Secure` cookie 設定、或 BFF 層——維持現有純 API 形狀，Task 10 的契約文件不必為認證機制新增額外端點。
- 前端需自行實作：記憶體 token store、`localStorage` 讀寫、401 攔截 + single-flight refresh 佇列、refresh 失敗時的登出導頁。這些邏輯集中在 Task 11 的 HTTP client 層，其餘模組（cart/orders/...）呼叫時無感。
- XSS 防護責任更依賴前端（CSP、避免 `dangerouslySetInnerHTML`/`v-html` 等），因為 refresh token 對前端 JS 可讀。這是主動接受的取捨，非疏漏。
- 換分頁/開新分頁不會自動共享登入態（access token 只在記憶體），需要靠 refresh token 重新換發——多分頁情境下可能出現短暫的重複 refresh 呼叫，但輪替機制保證最終只有一顆有效 refresh token 留存，不會產生資料不一致。
- 若未來改走行動 App 或需要更嚴格的 XSS 防護，可重新評估搬到 Cookie/BFF；本 ADR 僅針對目前 SPA 前端范圍。
