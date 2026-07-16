# Dream Fly API 契約文件（前端整合用）

給前端團隊（Task 11-19）的 API 契約總覽。內容皆對照後端原始碼（DTO / route / service）confirm 過，非憑記憶／推測。若程式碼與本文件不一致，以程式碼為準並回報後端修正本文件。

## 1. 基本慣例

### 1.1 Base URL

- 開發環境：`http://localhost:3000/api/v1`
- 所有路徑前綴皆為 `/api/v1`（`docs` 以下端點表的 path 皆省略此前綴）。
- 健康檢查（無需認證）：`GET /api/v1/health` → `{"status": "healthy"|"degraded", "services": {"database": "up"|"down", "redis": "up"|"down", "kafka": "connected"|"disabled"}}`，degraded 時回 503。

### 1.2 認證（Bearer Token）

- 除各端點表中標註「公開」者外，其餘皆需帶 `Authorization: Bearer <access_token>`。
- 取得 token 的端點（`/auth/register`、`/auth/login`、`/auth/google`、`/auth/refresh`）回應皆為同一形狀：

  ```jsonc
  // AuthResponse
  {
    "access_token": "eyJ...",
    "refresh_token": "eyJ...",
    "user": {
      "id": "uuid",
      "email": "string",
      "name": "string",
      "phone": "string | null",
      "phone_verified": "boolean",
      "avatar_url": "string | null",
      "is_active": "boolean",
      "created_at": "ISO8601",
      "roles": ["member" /* | "admin" | "coach" | "guest" */]
    }
  }
  ```

- **沒有 `expires_in` 欄位** — access/refresh token 的存活期不隨回應提供，前端必須依本文件記載的固定值自行判斷：
  - **access token：15 分鐘**（`config/default.toml` 的 `auth.jwt_access_expiration_minutes`，開發環境不變）。
  - **refresh token：30 天**（`auth.jwt_refresh_expiration_days`）。
  - 若後端調整這兩個數字，本文件需同步更新——前端不應嘗試從 JWT payload 自行解析 `exp`（可行但非契約保證的介面，後端保留調整 payload 形狀的權利）。
- **Refresh 輪替（rotation）**：每次呼叫 `POST /auth/refresh` 都會讓舊 refresh token 失效並核發一組全新的 access+refresh token；前端必須用回應中的新 `refresh_token` 覆蓋本機儲存的舊值，不可重複使用同一顆 refresh token。
- **重用偵測即家族撤銷**：若一顆「已被撤銷」的 refresh token 再次被拿來呼叫 `/auth/refresh`（例如舊 token 外洩、或前端 race condition 下重放了舊值），後端視為憑證竊用，會撤銷該使用者**所有** refresh token（整個裝置/session 家族），並回 401。使用者需重新登入。因此前端必須確保同一時間只有一個 refresh 請求在飛行中（single-flight，見 ADR-0001）。
- `POST /auth/logout` 撤銷傳入的 refresh token（單一裝置登出），是幂等操作（傳入無效 token 也回 200）。

### 1.3 錯誤格式

所有錯誤回應皆為：

```json
{ "error": "訊息文字" }
```

狀態碼對應：

| 狀態碼 | 意義 | 常見情境 |
| --- | --- | --- |
| 400 Bad Request | 請求格式錯誤 / 業務規則拒絕 | 無效的 coupon、無法轉換的訂單狀態、購物車為空 |
| 401 Unauthorized | 未帶 token / token 無效或過期 / 帳密錯誤 | 缺少或錯誤的 `Authorization` header |
| 403 Forbidden | 已認證但權限不足 | 一般會員呼叫 admin-only 端點 |
| 404 Not Found | 資源不存在 | 課程 / 商品 / 優惠碼 / 訂單不存在 |
| 409 Conflict | 唯一性衝突 / 併發衝突 | Email 已註冊、優惠碼重複、庫存不足、點數不足 |
| 422 Unprocessable Entity | 欄位驗證失敗 | `validator` 規則不通過（長度、格式、必填） |
| 500 Internal Server Error | 未預期錯誤 | 一律回通用訊息，不洩漏內部細節 |

**總則：admin-only 端點的角色閘門先於請求驗證。** admin 專屬端點在 route 層即檢查
角色，非 admin 呼叫者一律先回 403——不論 payload/query 是否合法（即 403 先於 422/400）。

### 1.4 分頁慣例

- Query 參數：`page`（預設 1）、`per_page`（預設 20，最大 **100**，超過會被 clamp，不會報錯）。
- 分頁回應形狀一律為：`{ "<items_key>": [...], "total": number, "page": number, "per_page": number }`。
- **有分頁**的端點：`GET /courses`、`GET /products`、`GET /coupons`（admin）、`GET /orders`（admin）、`GET /orders/me`、`GET /posts`、`GET /contact/inquiries`（admin）、`GET /points/me`（ledger 部分分頁，balance 不分頁）、`GET /leave-requests`（admin/coach，見 §3.20）、`GET /conversations/{id}/messages`（見 §3.21）、`GET /rewards/redemptions/me`（見 §3.23）。
- **純陣列（無分頁）**的端點：`GET /coaches`、`GET /venues`、`GET /subscriptions/me`、`GET /enrolments/me`、`GET /waitlist/me`、`GET /waitlist?course_id=`、`GET /notifications`（僅接受 `page`/`per_page` 但回應是純陣列，見下方 Notifications 一節）、`GET /schedule`、`GET /courses/{id}/sessions`、`GET /sessions/today`、`GET /schedule/me`（後三者見 §3.18）、`GET /leave-requests/me`（見 §3.20）、`GET /conversations/me`（見 §3.21）、`GET /report-cards/me`、`GET /certificates/me`（後兩者見 §3.22）、`GET /rewards`（見 §3.23）。

### 1.5 金額慣例

- 所有金額欄位（`*_cents`）皆為**新台幣 × 100 的整數**（例：`price_cents: 35000` = NT$350）。前端顯示時需除以 100。
- 折扣（`discount_cents`）在結帳時會被 clamp 到「不超過 subtotal」，永遠 `>= 0`。
- **場租計價**（Round 4 Task P4-B2）：`time_slots.price_cents` 是該時段的場租定價，admin 透過 `POST /schedule/slots`（見 §3.6）建立時段時可選填，省略預設 `0`。`POST /bookings` 建立預約時，會把當下 slot 的 `price_cents` **複製（快照）**進 `bookings.price_cents`——之後 slot 改價不會回溯影響已建立的 booking；取消預約（`PATCH /bookings/{id}/cancel`）也不會清除或歸零這個快照值。此快照供未來場租營收報表使用，報表僅計入 `confirmed`/`completed` 狀態，由聚合邏輯負責過濾（非本欄位語意）。

### 1.6 點數慣例

- **1 點 = NT$1**（消費時每 100 元折抵 1 點，即 `points_used * 100 <= 折扣後金額`）。
- **賺點**：結帳成功時，依「折扣與點數折抵後的實際應付金額」的 **5%** 無條件四捨五入計算（`round(total_nt * 0.05)`，`total_nt = total_cents / 100`）。例：實付 NT$1000 → 賺 50 點；實付 NT$730 → 賺 37 點（36.5 四捨五入）。
- **兌換**：`POST /rewards/{id}/redeem` 成功會扣點，寫入一筆 `point_ledger`，`reason = "redeem"`（`delta = -points_cost`）——與結帳的 `checkout_redeem` 是不同 reason，前端可用此欄位區分「結帳折抵」與「兌換獎勵」兩種扣點來源。見 §3.23。
- 點數餘額與明細見 `GET /points/me`。`reason` 目前有 `checkout_earn`/`checkout_redeem`/`admin_adjust`/`redeem` 四種。

### 1.7 Idempotency-Key（`POST /orders`）

- 結帳請求可帶 `Idempotency-Key` header（任意 1-128 字元的 ASCII 可見字元 / `-` / `_`；不合法格式會被忽略，等同未帶）。
- 同一使用者、同一 key 重放（無論購物車或 body 是否相同）都會回傳**第一次**呼叫產生的訂單（相同 `order_number`），不會重複扣款、重複建立報名/訂閱、重複發點。
- 不帶 `Idempotency-Key` 時，每次呼叫都會建立新訂單（無防重放保護）— 前端結帳按鈕應永遠帶上此 header。

### 1.8 模擬付款（Mock Payment）語意

**本系統沒有真正的金流串接。** `POST /orders` 呼叫成功即代表「付款成功」：

- 建立的訂單 `status` 直接是 `"paid"`，`paid_at` 於當下立即寫入（不會經過 `pending` 狀態）。
- 沒有「付款中」「等待付款」的中介狀態，也沒有 webhook 回呼流程。
- 若購物車包含課程或方案（membership/ticket）商品，對應的報名（enrolment）與訂閱（subscription）會在**同一個交易**內立即建立完成（見 §3.10）。
- 後續狀態流轉（`paid → processing → completed / refunded / cancelled`）僅能由 admin 透過 `PATCH /orders/{id}/status` 手動觸發，代表出貨、完成、退款等後續營運操作，與「付款」本身無關。
- `payment_method`（Round 4 Task P4-B1，報表基礎欄位）記錄本筆訂單的付款方式，值域：`credit_card`（預設）/ `line_pay` / `atm` / `jkopay` / `cash`；純應用層值域，非 DB enum。`POST /orders` 不帶此欄時預設 `credit_card`；帶入值域外的字串回 422。此欄位新增前建立的歷史訂單為 `null`。

---

## 2. 端點總覽表

| 模組 | Method | Path | 認證 |
| --- | --- | --- | --- |
| Auth | POST | `/auth/register` | 公開 |
| Auth | POST | `/auth/login` | 公開 |
| Auth | POST | `/auth/google` | 公開 |
| Auth | POST | `/auth/refresh` | 公開（帶 refresh token） |
| Auth | POST | `/auth/logout` | 公開（帶 refresh token） |
| Auth | POST | `/auth/otp/send` | 需登入 |
| Auth | POST | `/auth/otp/verify` | 需登入 |
| Auth | POST | `/auth/password/forgot` | 公開 |
| Auth | POST | `/auth/password/reset` | 公開 |
| Users | GET | `/users/me` | 需登入 |
| Users | PATCH | `/users/me` | 需登入 |
| Users | GET | `/users?page=&per_page=` | admin |
| Users | GET | `/users/{id}` | admin |
| Users | POST | `/users` | admin |
| Users | PATCH | `/users/{id}` | admin |
| Courses | GET | `/courses` | 公開 |
| Courses | GET | `/courses/{slugOrId}` | 公開 |
| Courses | POST | `/courses` | admin |
| Courses | PATCH | `/courses/{id}` | admin |
| Coaches | GET | `/coaches` | 公開 |
| Coaches | GET | `/coaches/{id}` | 公開 |
| Coaches | POST | `/coaches` | admin |
| Coaches | PATCH | `/coaches/{id}` | admin |
| Coaches | GET | `/coaches/{id}/schedule` | 公開 |
| Coaches | PUT | `/coaches/{id}/schedule` | 需登入（本人或 admin，見備註） |
| Coaches | POST | `/coaches/{id}/clock-in` | 需登入 |
| Coaches | POST | `/coaches/{id}/clock-out` | 需登入 |
| Coaches | GET | `/coaches/{id}/clock-records` | 需登入 |
| Venues | GET | `/venues` | 公開 |
| Venues | GET | `/venues/{slug}` | 公開 |
| Venues | POST | `/venues` | admin |
| Venues | PATCH | `/venues/{id}` | admin |
| Schedule | GET | `/schedule?year=&month=` | 公開 |
| Schedule | GET | `/schedule/availability?date=` | 公開 |
| Schedule | POST | `/schedule/slots` | 需登入（實務為 admin，見備註） |
| Schedule | PATCH | `/schedule/slots/{id}` | admin |
| Products | GET | `/products` | 公開 |
| Products | GET | `/products/{slugOrId}` | 公開 |
| Products | POST | `/products` | admin |
| Products | PATCH | `/products/{id}` | admin |
| Cart | GET | `/cart` | 需登入 |
| Cart | POST | `/cart/items` | 需登入 |
| Cart | PATCH | `/cart/items/{id}` | 需登入 |
| Cart | DELETE | `/cart/items/{id}` | 需登入 |
| Cart | DELETE | `/cart` | 需登入 |
| Coupons | GET | `/coupons` | admin |
| Coupons | POST | `/coupons` | admin |
| Coupons | PATCH | `/coupons/{id}` | admin |
| Coupons | DELETE | `/coupons/{id}` | admin |
| Coupons | GET | `/coupons/{code}/validate` | 需登入 |
| Orders | POST | `/orders` | 需登入 |
| Orders | GET | `/orders/me` | 需登入 |
| Orders | GET | `/orders/{id}` | 需登入（本人或 admin） |
| Orders | GET | `/orders` | admin |
| Orders | PATCH | `/orders/{id}/status` | admin |
| Subscriptions | GET | `/subscriptions/me` | 需登入 |
| Subscriptions | POST | `/subscriptions/{id}/redeem` | admin 或 coach |
| Enrolments | GET | `/enrolments/me` | 需登入 |
| Enrolments | PATCH | `/enrolments/{id}/cancel` | 需登入（本人或 admin） |
| Enrolments | GET | `/enrolments/{id}/attendance` | 需登入（本人或 admin） |
| Sessions | GET | `/courses/{id}/sessions?from=&to=` | 需登入 |
| Sessions | GET | `/sessions/today` | admin 或 coach |
| Sessions | GET | `/schedule/me` | 需登入 |
| Attendance | GET | `/sessions/{id}/roster` | admin 或該課教練 |
| Attendance | PUT | `/sessions/{id}/attendance` | admin 或該課教練 |
| Attendance | GET | `/coaches/me/students` | coach |
| Leave Requests | POST | `/leave-requests` | 需登入 |
| Leave Requests | GET | `/leave-requests/me` | 需登入 |
| Leave Requests | DELETE | `/leave-requests/{id}` | 需登入（僅本人，無 admin 例外） |
| Leave Requests | GET | `/leave-requests?status=&course_id=` | admin 或該課教練 |
| Leave Requests | PATCH | `/leave-requests/{id}` | admin 或該課教練 |
| Leave Requests | POST | `/leave-requests/{id}/makeup` | 需登入（僅本人） |
| Messages | POST | `/conversations` | 需登入（member 或 coach） |
| Messages | GET | `/conversations/me` | 需登入 |
| Messages | GET | `/conversations/{id}/messages` | 需登入（僅參與者） |
| Messages | POST | `/conversations/{id}/messages` | 需登入（僅參與者） |
| Messages | PATCH | `/conversations/{id}/read` | 需登入（僅參與者） |
| Report Cards | POST | `/report-cards` | admin 或該課教練 |
| Report Cards | GET | `/report-cards/me` | 需登入 |
| Certificates | POST | `/certificates` | admin 或教練（限自己課程學員） |
| Certificates | GET | `/certificates/me` | 需登入 |
| Waitlist | POST | `/waitlist` | 需登入 |
| Waitlist | GET | `/waitlist/me` | 需登入 |
| Waitlist | GET | `/waitlist?course_id=` | admin |
| Waitlist | DELETE | `/waitlist/{id}` | 需登入（本人或 admin） |
| Points | GET | `/points/me` | 需登入 |
| Rewards | GET | `/rewards?all=` | 需登入（`all=true` 需 admin） |
| Rewards | POST | `/rewards/{id}/redeem` | 需登入 |
| Rewards | GET | `/rewards/redemptions/me` | 需登入 |
| Rewards | POST | `/rewards` | admin |
| Rewards | PATCH | `/rewards/{id}` | admin |
| Notifications | GET | `/notifications` | 需登入 |
| Notifications | GET | `/notifications/unread-count` | 需登入 |
| Notifications | PATCH | `/notifications/{id}/read` | 需登入 |
| Posts | GET | `/posts` | 公開（僅 published） |
| Posts | GET | `/posts/{slugOrId}` | 公開（僅 published） |
| Posts | POST | `/posts` | admin 或 coach |
| Posts | PATCH | `/posts/{id}` | admin 或作者本人 |
| Posts | DELETE | `/posts/{id}` | admin |
| Contact | POST | `/contact` | 公開 |
| Contact | GET | `/contact/inquiries` | admin |
| Contact | PATCH | `/contact/inquiries/{id}` | admin |
| Reports | GET | `/reports/admin` | admin |
| Reports | GET | `/reports/admin/activity` | admin |
| Reports | GET | `/reports/coach` | coach |
| Reports | GET | `/reports/me` | 需登入 |
| Settings | GET | `/settings` | admin |
| Settings | PUT | `/settings` | admin |

---

## 3. 端點詳述

### 3.1 Auth

#### `POST /auth/register` — 公開
Body：`{ email, name, password }`（email 格式、name 2-100 字、password 8-128 字）。
成功回應：`AuthResponse`（見 §1.2）。註冊即自動指派 `member` 角色。
錯誤：409（email 已註冊，訊息一律為通用 `"registration failed"`，不洩漏帳號存在與否的細節）。

#### `POST /auth/login` — 公開
Body：`{ email, password }`。回應：`AuthResponse`。
錯誤：401（帳密錯誤、帳號停用、或觸發每信箱 15 分鐘鎖定 — 皆回同一訊息，不區分原因）。

#### `POST /auth/google` — 公開
Body：`{ code }`（Google OAuth authorization code）。回應：`AuthResponse`。
首次登入自動建立帳號並指派 `member`；若該 email 已是密碼帳號則自動關聯 Google 身分。

#### `POST /auth/refresh` — 公開（帶 refresh token）
Body：`{ refresh_token }`。回應：`AuthResponse`（含輪替後的新 token 組）。
錯誤：401（token 無效、過期、或被偵測為重放 — 見 §1.2）。

#### `POST /auth/logout` — 公開（帶 refresh token）
Body：`{ refresh_token }`。回應：`{ "message": "logged out successfully" }`。幂等。

#### `POST /auth/otp/send` — 需登入
Body：`{ phone }`（8-20 字）。回應：`{ "message": "verification code sent" }`。
限制：每使用者每小時最多 3 次；驗證碼透過 Twilio 簡訊寄送，5 分鐘內有效。

#### `POST /auth/otp/verify` — 需登入
Body：`{ phone, code }`（code 恰 6 碼）。回應：`{ "message": "phone verified successfully" }`。成功後 `users.phone_verified = true`。
限制：每組驗證碼最多錯 5 次。

#### `POST /auth/password/forgot` — 公開
Body：`{ email }`。回應恆為 `{ "message": "if that email exists, a password reset link has been sent" }`（不論 email 是否存在，避免帳號枚舉）。重設連結 15 分鐘有效，寄送至信箱。

#### `POST /auth/password/reset` — 公開
Body：`{ token, new_password }`（new_password 8-128 字）。回應：`{ "message": "password reset successfully" }`。成功後撤銷該使用者所有 refresh token（需重新登入）。
錯誤：400（token 無效或過期）。

---

### 3.2 Users

#### `GET /users/me` — 需登入
回應（`UserResponse`）：

```jsonc
{
  "id": "uuid", "email": "string", "name": "string",
  "phone": "string|null", "phone_verified": "boolean",
  "avatar_url": "string|null", "is_active": "boolean",
  "last_login": "ISO8601|null", "created_at": "ISO8601",
  "roles": ["member"], "points_balance": "number",
  "preferences": "object|null", "birth_date": "YYYY-MM-DD|null"
}
```

#### `PATCH /users/me` — 需登入
Body（皆為選填）：`{ name?, phone?, avatar_url?, preferences?, birth_date? }`（name 2-100 字；phone 8-20 字；avatar_url 須通過內部 URL 安全檢查；`preferences` 可為任意合法 JSON 值，**整包覆寫**——帶了就整個取代舊值，不做深合併，也不逐 key 驗證；不帶則維持原值不動；`birth_date` 為 `YYYY-MM-DD` 字串，範圍 **`1900-01-01` 至今天（含）**，超出範圍回 422——未來日期與早於 1900 年皆同一錯誤類型；帶 JSON `null` 會清空為 `NULL`，不帶此欄則維持原值不動）。未設定過的使用者，`preferences`／`birth_date` 皆為 `null`。回應：`UserResponse`。

本輪前端慣例 key（**僅文件性列舉，後端不驗證其形狀，也不限制其他 key 名稱**）：`class_reminder`/`coach_msg`/`promo`/`dark`，皆為布林值，對應 mobile 設定畫面的班別提醒／教練訊息／促銷通知／深色模式四個開關。

`UserResponse` 是 users 模組唯一的回應型別——`GET /users`、`GET /users/{id}`、`POST /users`、`PATCH /users/{id}`（見下，皆為 admin 視角）回應也是同一型別，因此同樣帶出 `preferences`／`birth_date`；但這些 admin 端點中只有 `POST /users`（見下）能寫入 `birth_date`，`preferences` 則仍須由使用者本人透過 `PATCH /users/me` 設定。

#### `GET /users?page=&per_page=` — admin
回應（`UserListResponse`）：`{ "users": [UserResponse], "total", "page", "per_page" }`。Task 18 起前端 admin 學員管理頁消費此端點（`points_balance` 映射為學員點數）。

#### `GET /users/{id}` — admin
回應：單筆 `UserResponse`。404 若查無。

#### `POST /users` — admin
Body（`CreateUserRequest`）：`{ email, name, phone?, password, birth_date? }`（email 格式；name 2-100 字；phone 8-20 字，選填；password 8-128 字；`birth_date` 為 `YYYY-MM-DD` 字串，選填，範圍同 `PATCH /users/me`：`1900-01-01` 至今天）。建立流程比照 `POST /auth/register`：Argon2 hash 密碼、`is_active = true`、於同一交易內指派 `member` 角色。回應：`UserResponse`（見上）。
錯誤：409（email 已存在，訊息 `"Email 已被使用"`——與 `/auth/register` 刻意通用化的 409 訊息不同，因為呼叫者是 admin，不受帳號枚舉考量限制）；422（password < 8 字；`birth_date` 超出範圍）。

**`POST /auth/register`（自助註冊）刻意不收 `birth_date`**——維持較低的註冊摩擦；自助註冊帳號的 `birth_date` 起始值為 `null`，會員本人可日後透過 `PATCH /users/me` 補填。

#### `PATCH /users/{id}` — admin
Body（皆為選填）：`{ name?, phone?, is_active? }`（name 2-100 字；phone 8-20 字；phone 異動會重置 `phone_verified = false`，與 `PATCH /users/me` 同一規則）。**不可改 `email`／`roles`／`password`／`birth_date`**——這幾者不是本端點的欄位，body 中帶了也會被忽略（v1 範圍外；`birth_date` 只能透過使用者本人的 `PATCH /users/me` 或建立當下的 `POST /users` 設定）。回應：`UserResponse`。
錯誤：422（`name`/`phone`/`is_active` 皆未提供，訊息 `"至少提供一個欄位"`）；404（查無此使用者）。
備註：`is_active` 有變動時，後端會立即清除該使用者的 Redis 快取（角色 + `is_active`），停用在下一次請求即生效，不必等待 `AuthUser` extractor 的 60 秒快取 TTL。

---

### 3.3 Courses

#### `GET /courses?page=&per_page=` — 公開
回應（`CourseListResponse`）：`{ "courses": [CourseResponse], "total", "page", "per_page" }`。**目前不支援 category/level 篩選 query**，一次拉全部再前端篩選，或等後端加篩選端點。**列表項目不含 `schedule_slots`**——見下方 `GET /courses/{slugOrId}` 的裁決說明。

`CourseResponse`：

```jsonc
{
  "id": "uuid", "name": "string", "slug": "string",
  "level": "foundation|beginner|intermediate|advanced|elite",
  "description": "string|null", "duration_minutes": "number",
  "price_cents": "number", "max_students": "number",
  "min_age": "number|null", "max_age": "number|null",
  "features": ["string"], "is_active": "boolean",
  "coach_id": "uuid|null", "category": "string|null",
  "schedule_text": "string|null", "is_highlighted": "boolean",
  "created_at": "ISO8601", "updated_at": "ISO8601",
  "enrolled_count": "number", "waitlist_count": "number"
}
```

`enrolled_count`/`waitlist_count` 為即時計算（分別數 `enrolments.status='active'`、`waitlist_entries.status='waiting'`），非快取值。

`level` 值域（Task 7 起由 3 級擴充為 5 級，`course_level` Postgres enum 由低到高）：

| 值 | 中文對照 |
| --- | --- |
| `foundation` | 啟蒙 |
| `beginner` | 入門 |
| `intermediate` | 基礎 |
| `advanced` | 進階 |
| `elite` | 選手 |

#### `GET /courses/{slugOrId}` — 公開
`{slugOrId}` 可為 slug 或 UUID（後端先嘗試 parse 成 UUID，失敗則當 slug 查詢，皆大小寫不敏感）。回應（`CourseDetailResponse`）：`CourseResponse` 的所有欄位（同一層，非巢狀）再加一個 `schedule_slots` 陣列：

```jsonc
{
  "id": "uuid", "name": "string", /* ...其餘欄位同 CourseResponse... */
  "schedule_slots": [
    { "id": "uuid", "day_of_week": 0, "start_time": "HH:MM:SS",
      "end_time": "HH:MM:SS", "venue": "string|null" }
  ]
}
```

`day_of_week` 為 **0=Sunday .. 6=Saturday**（PostgreSQL `EXTRACT(DOW)` 慣例，也是 JS `Date.getDay()` 慣例；詳見 §3.18）。404 若查無課程。

**裁決**：`schedule_slots` 只在單一課程回應出現（本端點、`POST`、`PATCH`），`GET /courses`（列表）刻意不附加——避免對每筆課程多查一次 slots 造成 N+1。前端要顯示某課程週模式時，一律呼叫本端點取得該課程 detail。

#### `POST /courses` — admin
Body（`CreateCourseRequest`）：`{ name, slug?, level, description?, duration_minutes, price_cents, max_students, min_age?, max_age?, features?, coach_id?, category?, schedule_text?, is_highlighted?, schedule_slots? }`。`schedule_slots`（選填）：`[{ day_of_week, start_time: "HH:MM", end_time: "HH:MM", venue? }]`——不帶則建立的課程沒有任何週模式。回應：`CourseDetailResponse`。

#### `PATCH /courses/{id}` — admin
Body（`UpdateCourseRequest`，皆選填，同名欄位語意同上）。`min_age`/`max_age`/`coach_id`/`category`/`schedule_text` 可明確傳 `null` 清空，欄位不帶則維持原值不動。**`schedule_slots` 為整組替換語意**：帶此欄位（即使是空陣列 `[]`）會在同一交易內刪除該課程現有全部 slots 並以新內容取代；**不帶此欄位（欄位整個不存在於 JSON body）則完全不動現有 slots**。回應：`CourseDetailResponse`。

---

### 3.4 Coaches

#### `GET /coaches` — 公開
回應：`CoachResponse[]`（**純陣列，不分頁**，依 `display_order` 排序）。

```jsonc
{
  "id": "uuid", "user_id": "uuid", "name": "string", "title": "string",
  "bio": "string|null", "experience": "string|null",
  "specialties": ["string"], "certifications": ["string"],
  "is_active": "boolean", "display_order": "number",
  "slug": "string|null", "photo_url": "string|null",
  "created_at": "ISO8601"
}
```

`name` 為教練姓名（join `users.name`，coaches 表本身無此欄位）；`title` 是職稱（如「資深體操教練」），**不含姓名**——兩者是不同語意的欄位。

#### `GET /coaches/{id}` — 公開
`{id}` 為教練的 UUID（非使用者 id，也非 slug）。回應（`CoachDetailResponse`）：`{ "coach": CoachResponse, "schedules": CoachScheduleResponse[] }`。

#### `POST /coaches` — admin
將既有使用者（先用 `POST /users` 建帳號）綁定為教練。Body：`{ user_id, title, bio?, experience?, specialties?, certifications?, display_order?, slug?, photo_url?, is_active? }`。`user_id`/`title` 必填（`title` 對應 `coaches.title`，NOT NULL 無 DEFAULT）；其餘欄位省略時採 DB 預設（`specialties`/`certifications` 預設空陣列、`is_active` 預設 `true`、`display_order` 預設 `0`、`slug`/`photo_url` 維持 `NULL`）。姓名不在此——那是 `users.name`。

Service 內同一交易完成兩件事：新增 coaches 列 + 指派該 user `coach` 角色；成功後會清除該 user 的 Redis 角色快取（`user_roles:{id}`），下一次請求即可看到新角色，不必等 15 分鐘 TTL 到期。

回應：`CoachResponse`（與 `GET /coaches` 同型）。
錯誤：404（`user_id` 查無此使用者）；409（該 user 已是教練，或 `slug` 與其他教練衝突）。

#### `PATCH /coaches/{id}` — admin
只動教練自身欄位：`{ title?, bio?, experience?, specialties?, certifications?, is_active?, display_order?, slug?, photo_url? }`。姓名不在此——走既有 `PATCH /users/{id}`。`bio`/`experience`/`slug`/`photo_url` 可明確傳 `null` 清空，欄位不帶則維持原值不動；空 body 視為 no-op，僅刷新 `updated_at`——同 `PATCH /venues/{id}` 的既有行為。

回應：`CoachResponse`。
錯誤：404（查無此教練）；409（`slug` 與其他教練衝突）。

#### `GET /coaches/{id}/schedule` — 公開
回應：`CoachScheduleResponse[]`：`{ id, day_of_week (0-6), start_time ("HH:MM:SS"), end_time, is_available }`。

#### `PUT /coaches/{id}/schedule` — 需登入
Body：`{ schedules: [{ day_of_week, start_time, end_time, is_available }] }`。整批覆蓋該教練的排班。回應：更新後的 `CoachScheduleResponse[]`。
錯誤：409（教練班表時段重疊）。

#### `POST /coaches/{id}/clock-in` / `POST /coaches/{id}/clock-out` — 需登入
Body（clock-in）：`{ note? }`（≤500 字）。回應（`ClockRecordResponse`）：`{ id, clock_in, clock_out, note, created_at }`。clock-out 無 body。同一教練同時只能有一筆未結束的打卡（DB 唯一索引保證）。

#### `GET /coaches/{id}/clock-records?page=&per_page=` — 需登入
回應：`ClockRecordResponse[]`（**純陣列**，依 `clock_in DESC`；雖吃 `page`/`per_page` query 但回應本身不含分頁 meta，`total` 需前端自行處理或忽略）。

---

### 3.5 Venues

#### `GET /venues` — 公開
回應：`VenueResponse[]`（**純陣列，不分頁**）：`{ id, category_id, name, slug, description, features, image_url, is_active, created_at }`。

#### `GET /venues/{slug}` — 公開
**僅接受 slug**（不像 courses/products 支援 UUID fallback）。回應：`VenueResponse`。

#### `POST /venues` — admin
Body：`{ name, slug?, category_id?, description?, features?, image_url? }`。回應：`VenueResponse`。

#### `PATCH /venues/{id}` — admin
Update 為對應欄位皆選填的 PATCH：`{ name?, slug?, category_id?, description?, features?, image_url?, is_active? }`。`category_id`/`description`/`image_url` 可明確傳 `null` 清空（清為 `NULL`），欄位不帶則維持原值不動。空 body（所有欄位皆未提供）視為 no-op，僅刷新 `updated_at`——同 `PATCH /courses/{id}` 的既有行為。回應：`VenueResponse`。
錯誤：409（`slug` 與其他場館衝突，訊息含衝突的 slug 值）；404（查無此場館）。

---

### 3.6 Schedule

#### `GET /schedule?year=&month=` — 公開
回應：`DaySchedule[]`（每日一筆）：`{ date: "YYYY-MM-DD", slots: TimeSlotResponse[] }`。

`TimeSlotResponse`：`{ id, date, start_time, end_time, venue_id, course_id, capacity, booked, status: "available"|"limited"|"full"|"closed", price_cents }`。`price_cents`（Round 4 Task P4-B2）是該時段的場租定價，見 §1.5。

#### `GET /schedule/availability?date=YYYY-MM-DD` — 公開
回應：`TimeSlotResponse[]`（純陣列，當日所有時段）。

#### `POST /schedule/slots` — 需登入
Body：`{ slots: [{ date, start_time, end_time, venue_id?, course_id?, capacity, price_cents? }] }`。`price_cents` 選填，省略預設 `0`（§1.5）。回應：建立後的時段列表（`TimeSlotResponse[]`）。
錯誤：409（場地時段與既有時段重疊）。

#### `PATCH /schedule/slots/{id}` — admin
Body：`{ is_closed: boolean }`。admin 手動關閉／重新開放單一時段——`is_closed` 是落地儲存的管理意圖旗標，`status` 本身不落地，讀取時依 `booked`/`capacity`/`is_closed` 即時推導（見 CONTEXT.md「時段狀態」詞條）。設為 `true` 後，回應與後續任何讀取（`GET /schedule`、`GET /schedule/availability`）該時段的 `status` 立即變為 `"closed"`——優先於 booked/capacity 判斷，即使該時段仍有空位。`POST /bookings` 對已關閉時段的新預約會被拒絕（400，訊息 `"time slot is full or closed"`，與滿位共用同一分支、同一狀態碼）；關閉不影響該時段既有的預約，取消既有預約仍正常運作。回應：更新後的 `TimeSlotResponse`。
錯誤：404（時段不存在）。

#### Bookings（場租預約）— `price_cents` 快照語意

本文件目前未收錄 `/bookings/*`（`POST /bookings`、`GET /bookings/me`、`PATCH /bookings/{id}/cancel`、`GET /bookings` admin）端點的完整請求/回應形狀——這是既有缺口，不在本任務（P4-B2）範圍內修補。以下僅記錄 Task P4-B2 新增的 `price_cents` 相關語意：

- `POST /bookings` 建立預約時，會把當下 `time_slot_id` 對應 slot 的 `price_cents` **複製（快照）**進新建立的 `bookings.price_cents`——之後該 slot 被改價，既有 booking 的 `price_cents` 不受影響。
- `PATCH /bookings/{id}/cancel` 取消預約**不會**清除或歸零 `price_cents`；`BookingResponse` 回應中的 `price_cents` 在取消前後維持不變。
- 詳見 §1.5 金額慣例。

---

### 3.7 Products

#### `GET /products?product_type=&page=&per_page=` — 公開
`product_type` 選填篩選：`ticket|course_package|membership|merchandise`。回應（`ProductListResponse`）：`{ "products": [ProductResponse], "total", "page", "per_page" }`。

`ProductResponse`：

```jsonc
{
  "id": "uuid", "name": "string", "slug": "string",
  "product_type": "ticket|course_package|membership|merchandise",
  "description": "string|null", "price_cents": "number",
  "original_price_cents": "number|null", "features": ["string"],
  "is_highlighted": "boolean", "badge": "string|null",
  "stock": "number|null", "quota": "number|null", "sold": "number",
  "valid_days": "number|null",
  "session_count": "number|null", "is_active": "boolean",
  "created_at": "ISO8601", "updated_at": "ISO8601"
}
```

`stock: null` = 無限庫存（票券/方案皆為 null；只有實體商品 merchandise 才會有限量庫存數字）。`quota` 為 `stock` 的直接映射（同一個值，語意相同，null = 無限）。`sold` = 該商品在「已付款類」訂單（`paid`/`processing`/`completed`）中 `order_items.quantity` 的總和，一次 GROUP BY 查詢算完，無訂單時為 `0`。

#### `GET /products/{slugOrId}` — 公開
同 courses，slug 或 UUID 皆可。回應：`ProductResponse`。

#### `POST /products` / `PATCH /products/{id}` — admin
Create body：`{ name, slug?, product_type, description?, price_cents, original_price_cents?, features?, is_highlighted?, badge?, stock?, valid_days?, session_count? }`。Update 為對應欄位皆選填的 PATCH（`Some(null)` 語意清除欄位，前端只需照一般 PATCH 語意送想改的欄位）。

---

### 3.8 Cart（新契約：`item_type` / `item_id`）

購物車不再只認 `product_id` — 現在每筆項目透過 `item_type`（`"product"` 或 `"course"`）+ `item_id`（該 product 或 course 的 UUID）指定目標。

#### `GET /cart` — 需登入
回應（`CartResponse`）：

```jsonc
{
  "items": [
    {
      "id": "uuid",              // cart item 自己的 id（PATCH/DELETE 用這個）
      "item_type": "product|course",
      "item_id": "uuid",         // 對應的 product_id 或 course_id
      "name": "string", "slug": "string",
      "quantity": "number",
      "unit_price_cents": "number",
      "subtotal_cents": "number"
    }
  ],
  "total_cents": "number"
}
```

#### `POST /cart/items` — 需登入
Body：`{ item_type: "product"|"course", item_id: "uuid", quantity? }`（quantity 預設 1，範圍 1-999）。回應：更新後的 `CartResponse`。
規則：**課程項目 quantity 永遠視為 1**（DB constraint `cart_items_course_qty` 強制）；同一使用者對同一 product 或同一 course 只能有一筆購物車項目（重複加入視為 upsert，quantity 不會累加，以最後一次的 quantity 為準——實際行為請以 `service.rs` 為準，前端可假設「加入已存在項目」不會拋錯而是更新該筆）。
錯誤：422（`item_type` 不是 `product`/`course`）。

#### `PATCH /cart/items/{id}` — 需登入
`{id}` 為 cart item 的 id（不是 product_id/course_id）。Body：`{ quantity }`（1-999）。回應：`CartResponse`。

#### `DELETE /cart/items/{id}` — 需登入
回應：`CartResponse`（移除後的購物車）。

#### `DELETE /cart` — 需登入
清空整台購物車。回應：204 No Content（無 body）。

---

### 3.9 Coupons

#### `GET /coupons?page=&per_page=` — admin
回應（`CouponListResponse`）：`{ "coupons": [CouponResponse], "total", "page", "per_page" }`（分頁慣例見 §1.4）。

`CouponResponse`：

```jsonc
{
  "id": "uuid", "code": "string", "discount_cents": "number",
  "is_active": "boolean", "expires_at": "ISO8601|null", "created_at": "ISO8601"
}
```

#### `POST /coupons` — admin
Body（`CreateCouponRequest`）：`{ code, discount_cents, expires_at? }`（code 1-50 字；discount_cents 須 `>= 1`）。回應：`CouponResponse`（見上）。
`code` 儲存前會正規化（trim + 轉大寫），回應與後續比對皆用正規化後的值，故大小寫、前後空白視為同一張優惠碼；`code` 建立後不可修改（見下方 `PATCH /coupons/{id}`）。
錯誤：409（`"coupon code already exists"` — 正規化後的 code 重複）。

#### `PATCH /coupons/{id}` — admin
Body（皆選填，`UpdateCouponRequest`）：`{ discount_cents?, is_active?, expires_at? }`。**`code` 不可改**——它是對外發放的識別，不在 PATCH body 中。`expires_at` 可明確傳 `null` 清成永久有效（清為 `NULL`），欄位不帶則維持原值不動。回應：`CouponResponse`（見上）。
錯誤：404（查無此 coupon）。

#### `DELETE /coupons/{id}` — admin
硬刪除。**語意設計**：停用（`PATCH` 設 `is_active: false`）為主要路徑，DELETE 留給誤建且尚未被使用的 code。`orders` 只存 `coupon_code` 字串快照、無 FK 關聯到 coupons 表，故刪除 coupon 不影響任何歷史訂單。回應：204 No Content。
錯誤：404（查無此 coupon）。

#### `GET /coupons/{code}/validate?subtotal_cents=` — 需登入（任何已登入使用者，無角色限制）
`subtotal_cents` 選填。回應（`CouponValidateResponse`）：`{ "code": "string", "discount_cents": "number", "applied_discount_cents"?: "number" }`。`discount_cents` 恆為券面額，不受夾擠影響；帶了 `subtotal_cents` 才多回 `applied_discount_cents = min(discount_cents, subtotal_cents)`——與結帳（`POST /orders`，見 §3.10）同一夾擠規則（`orders::pricing::clamp_coupon_discount`）。不帶 `subtotal_cents` 時，回應逐位元組不變（`applied_discount_cents` 完全不出現在 JSON 中，向後相容既有呼叫端）。
判定「有效」= `is_active = true` 且（`expires_at` 為 null 或尚未過期）。
錯誤：404（`"coupon not found"` — 不存在、未啟用、已過期皆回此訊息，不區分原因）；422（`subtotal_cents` 為負數——此檢查先於 coupon 查詢，故未知 code 加負值一律回 422，不回 404）；400（`subtotal_cents` 無法解析為整數，如 `?subtotal_cents=abc`——回 `"subtotal_cents must be an integer"`，維持 §1.3 的 JSON 錯誤格式，不是框架預設的純文字拒絕）。

---

### 3.10 Orders

#### `POST /orders` — 需登入（結帳）
Header（建議）：`Idempotency-Key: <前端產生的唯一字串>`（見 §1.7）。
Body（`CheckoutRequest`，**整包皆選填，可傳 `{}` 或完全不帶 body**）：

```jsonc
{ "coupon_code": "string?", "use_points": "boolean?", "payment_method": "string?" }
```

- `coupon_code` 不帶或空字串 = 不套用折扣。無效碼會整筆拒絕（400 `"invalid coupon"`），不會靜默略過。
- `use_points: true` 時，會自動用掉「折扣後金額換算可扣的最大點數」（`min(目前餘額, 折扣後金額NT$)`），前端無法指定扣多少點——要嘛全扣（到可扣上限）要嘛不扣。
- `payment_method` 不帶時預設 `credit_card`；值域見 §1.8。不在值域內的字串回 422，整筆結帳不會建立（購物車保留）。
- 結帳對象為**當下購物車全部內容**，成功後購物車會被清空。購物車為空時回 400 `"cart is empty"`。

回應（`OrderResponse`）：

```jsonc
{
  "id": "uuid", "order_number": "string", "status": "paid",
  "total_cents": "number", "discount_cents": "number",
  "coupon_code": "string|null", "points_used": "number",
  "points_earned": "number", "payment_method": "string|null",
  "paid_at": "ISO8601", "created_at": "ISO8601",
  "items": [
    { "id": "uuid", "item_type": "product|course", "product_id": "uuid|null",
      "course_id": "uuid|null", "quantity": "number", "unit_price_cents": "number" }
  ],
  "enrolments": [ /* EnrolmentResponse[]，見 §3.12 — 本次購買產生的課程報名 */ ],
  "subscriptions": [ /* SubscriptionResponse[]，見 §3.11 — 本次購買產生的方案/票券 */ ]
}
```

`enrolments`/`subscriptions` 只包含**這筆訂單**產生的項目（用 `order_id` 反查），不是使用者的全部報名/訂閱清單——那些請另外呼叫 `/enrolments/me` / `/subscriptions/me`。

`payment_method` 為 `null` 僅出現在此欄位新增（Round 4 Task P4-B1）前建立的歷史訂單。

錯誤：400（購物車為空、無效優惠碼）；422（付款方式不在值域內）；409（商品庫存不足、課程已滿或重複報名 — 整筆結帳一起回滾，不會部分成功）。

#### `GET /orders/me?page=&per_page=` — 需登入
回應（`OrderListResponse`）：`{ "orders": [OrderSummary], "total", "page", "per_page" }`。

`OrderSummary`（**摘要，不含 enrolments/subscriptions artifacts，但含品項摘要**）：`{ id, order_number, status, total_cents, created_at, items }`。

`items`：`[{ name: string, quantity: number }]`——`name` 取自 `order_items` 下單當時的快照欄位（結帳當下的商品/課程名稱），**不是**即時 join 現在的商品目錄，所以商品改名或下架後，舊訂單的品項名稱仍維持下單當時的樣子。

#### `GET /orders/{id}` — 需登入（本人或 admin）
回應：完整 `OrderResponse`（同結帳回應形狀，含 items + enrolments + subscriptions）。403 若非本人也非 admin。

#### `GET /orders?page=&per_page=` — admin
回應（`AdminOrderListResponse`）：`{ "orders": [AdminOrderSummary], "total", "page", "per_page" }`。

`AdminOrderSummary`：`{ id, order_number, user_name, user_email, status, total_cents, points_used, coupon_code, created_at, items }`（含買家姓名/信箱，一般 `OrderSummary` 沒有；`items` 同上）。

#### `PATCH /orders/{id}/status` — admin
Body：`{ status: "pending"|"paid"|"processing"|"completed"|"cancelled"|"refunded" }`。回應：更新後的 `OrderResponse`。
狀態機（非法轉換回 400）：`pending→paid|cancelled`；`paid→processing|refunded|cancelled`；`processing→completed|refunded`；`completed→refunded`；同狀態原地不動視為合法（幂等）。**Seed 出來的訂單一律已是 `paid`**（見 §1.8），實務上前端幾乎不會看到 `pending`。

---

### 3.11 Subscriptions（方案/票券 entitlement）

購買 `ticket` 或 `membership` 類商品會產生一筆 subscription，記錄「剩餘堂數」與/或「到期日」。

#### `GET /subscriptions/me` — 需登入
回應：`SubscriptionResponse[]`（**純陣列，不分頁**，新到舊）：

```jsonc
{
  "id": "uuid", "product_id": "uuid", "product_name": "string",
  "status": "active|expired|cancelled",
  "started_at": "ISO8601", "expires_at": "ISO8601|null",
  "total_sessions": "number|null", "remaining_sessions": "number|null",
  "price_cents": "number"
}
```

`status` 為**讀取當下即時計算**：DB 裡的 `cancelled` 直接回傳；否則若 `expires_at` 已過或 `remaining_sessions == 0` 回 `"expired"`；都沒有才回 `"active"`（DB 儲存值本身不會因為到期而被動改寫）。

#### `POST /subscriptions/{id}/redeem` — admin 或 coach
無 body。核銷一堂課（`remaining_sessions -= 1`，原子操作）。回應：更新後的 `SubscriptionResponse`。
錯誤：404（不存在）；409（`"subscription has no session quota"` — 純天數方案沒有堂數可核銷；或 `"subscription is not redeemable"` — 已無剩餘堂數/已過期/已取消）。

---

### 3.12 Enrolments（課程報名）

#### `GET /enrolments/me` — 需登入
回應：`MyEnrolmentResponse[]`（**純陣列，不分頁**，新到舊）：

```jsonc
{
  "id": "uuid", "course_id": "uuid", "course_name": "string",
  "course_level": "foundation|beginner|intermediate|advanced|elite",
  "schedule_text": "string|null", "status": "active|cancelled",
  "enrolled_at": "ISO8601",
  "attended": "number", "total": "number"
}
```

`attended`/`total` 為即時計算（單一 LEFT JOIN `countable_attendance` 聚合，非儲存欄位）：`attended` 為該 enrolment 被標記 `status='present'` 的筆數；`total` 為該 enrolment 的 `present`+`absent` 筆數之和——view 的成員資格本身即為分母。**`leave` 與尚未點名的場次一律不計入 `total`**（也就是說 `total` 不是「該課程至今已上過幾堂」，也不是「已點名場次數」，而是「present+absent 的場次數」；純請假、從未點名的場次皆不影響這兩個統計）。無 `present`/`absent` 紀錄時兩者皆為 `0`（即使該 enrolment 有請假紀錄）。詳見 §3.19 Attendance。

#### `PATCH /enrolments/{id}/cancel` — 需登入（本人或 admin）
無 body。回應：更新後的 `EnrolmentResponse`（`status: "cancelled"`，**不含** `attended`/`total`——僅 `GET /enrolments/me` 回傳這兩個統計欄位）。

#### `GET /enrolments/{id}/attendance` — 需登入（本人或 admin）
這筆報名的逐堂出勤紀錄：`attendance_records` JOIN `course_sessions`，只回**已點名**的場次(未點名場次不出現)，依 `session_date`(次要鍵 `start_time`)**舊到新**排序。回應（`AttendanceEntryResponse[]`，純陣列）：

```jsonc
[
  { "session_date": "YYYY-MM-DD", "start_time": "HH:MM:SS", "end_time": "HH:MM:SS",
    "status": "present|absent|leave", "marked_at": "ISO8601" }
]
```

`status` 為 §3.19 Attendance 的 `attendance_status` enum 原樣輸出。無任何點名紀錄的 enrolment 回 `200` 空陣列(不是 404)。

**Ownership gate**：非本人呼叫一律 **404**(與 `PATCH /enrolments/{id}/cancel` 的 403 不同——本端點刻意用 404 遮蔽存在性，不讓非本人用來探測某 enrolment id 是否存在)；enrolment id 不存在同樣回 404，兩種情況回應完全相同。admin 例外比照本模組 `cancel` 的「本人或 admin」慣例，可查看任意 enrolment。

---

### 3.13 Waitlist（候補）

候補為純登記名單：名額釋出（取消報名）**不會**觸發自動遞補或通知，遞補由 admin 依
`GET /waitlist?course_id=`（舊到新）名單人工聯絡——名單項目目前不含會員聯絡欄位，身分對照需另行處理；遞
補成功後的候補列由 admin 或會員手動取消。（ADR-0006）

#### `POST /waitlist` — 需登入
Body：`{ course_id: "uuid" }`。回應（`WaitlistResponse`）：`{ id, course_id, course_name, status: "waiting"|"cancelled", created_at }`。

#### `GET /waitlist/me` — 需登入
回應：`WaitlistResponse[]`（**純陣列**，新到舊）。

#### `GET /waitlist?course_id=uuid` — admin
回應：`WaitlistResponse[]`（該課程候補中的名單，舊到新）。缺少/無效 `course_id` 回 422（admin 以外先 403）。

#### `DELETE /waitlist/{id}` — 需登入（本人或 admin）
取消候補。回應：204 No Content。

---

### 3.14 Points（點數）

#### `GET /points/me?page=&per_page=` — 需登入
回應（`PointsMeResponse`，**balance 不分頁，ledger 分頁**）：

```jsonc
{
  "balance": "number",
  "ledger": [
    { "id": "uuid", "delta": "number", "balance_after": "number",
      "reason": "checkout_earn|checkout_redeem|admin_adjust|redeem",
      "order_id": "uuid|null", "created_at": "ISO8601" }
  ],
  "total": "number", "page": "number", "per_page": "number"
}
```

`delta` 可正可負（`checkout_redeem`/`redeem` 恆為負、`checkout_earn` 恆為正）。`reason = "redeem"` 的列一律 `order_id: null`（來自 `POST /rewards/{id}/redeem`，與訂單無關，見 §3.23）。

---

### 3.15 Notifications

#### `GET /notifications?page=&per_page=` — 需登入
回應：`NotificationResponse[]`（**純陣列**——吃 `page`/`per_page` query 但無分頁 meta 包裹）：

```jsonc
{
  "id": "uuid", "type": "booking_confirmed|booking_cancelled|order_placed|order_status|system|promotion",
  "title": "string", "message": "string", "is_read": "boolean",
  "metadata": "object|null", "created_at": "ISO8601"
}
```

注意 JSON key 是 `type`（Rust 欄位名 `notification_type` 經 `#[serde(rename = "type")]` 對外呈現為 `type`）。

#### `GET /notifications/unread-count` — 需登入
回應：`{ "count": "number" }`。

#### `PATCH /notifications/{id}/read` — 需登入
無 body。回應：更新後的 `NotificationResponse`（`is_read: true`）。

---

### 3.16 Posts（公告/文章）

#### `GET /posts?page=&per_page=` — 公開
只回傳 `status = "published"` 的文章。回應（`PostListResponse`）：`{ "posts": [PostResponse], "total", "page", "per_page" }`。

`PostResponse`（**列表用，不含 `content`**）：

```jsonc
{
  "id": "uuid", "author_id": "uuid", "title": "string", "slug": "string",
  "excerpt": "string|null",
  "category": "announcement|article|promotion|event",
  "status": "published",
  "cover_image": "string|null", "published_at": "ISO8601|null",
  "created_at": "ISO8601"
}
```

#### `GET /posts/{slugOrId}` — 公開
slug 或 UUID 皆可。回應（`PostDetailResponse`，**多了 `content` 與 `updated_at`**）：同上欄位 + `content: string`、`updated_at: ISO8601`。草稿/封存文章走此端點一律 404（非 admin 亦看不到）。

#### `POST /posts` — admin 或 coach
Body：`{ title, slug?, content, excerpt?, category, cover_image? }`（category 1-50 字，非嚴格 enum 檢查但預期為上述四值之一）。新建文章預設 `status: "draft"`。回應：`PostDetailResponse`。

#### `PATCH /posts/{id}` — admin 或該文章作者本人
Body（皆選填）：`{ title?, slug?, content?, excerpt?, category?, status?, cover_image? }`（`status` 可設為 `draft|published|archived`，設為 `published` 才會出現在公開端點）。`excerpt`/`cover_image` 可明確傳 `null` 清空，欄位不帶則維持原值不動。回應：`PostDetailResponse`。

#### `DELETE /posts/{id}` — admin
回應：204 No Content。

---

### 3.17 Contact（聯絡表單）

#### `POST /contact` — 公開
Body：`{ name, email, phone?, subject, message, inquiry_type?, metadata? }`。`inquiry_type` 選填，預設 `"general"`，僅接受 `"general"`／`"trial"`（應用層驗證，非 DB CHECK/enum）；非法值 422。`metadata` 選填 JSONB 物件，後端不逐欄驗證、原樣存取——`trial`（試上預約）慣例欄位：`category`／`student_age`／`preferred_day`／`preferred_slot`／`parent_name`／`parent_phone`／`student_name`／`note`（僅文件性列舉，非後端 schema）。回應（`InquiryResponse`）：`{ id, name, email, phone, subject, message, status: "new", assigned_to: null, inquiry_type, metadata, created_at, updated_at }`。既有呼叫端不帶 `inquiry_type`/`metadata` 時行為不變。

#### `GET /contact/inquiries?page=&per_page=` — admin
回應（`InquiryListResponse`）：`{ "inquiries": [InquiryResponse], "total", "page", "per_page" }`。

#### `PATCH /contact/inquiries/{id}` — admin
Admin 人工跟進用（Round 4 Task B5）。Body（皆選填，`UpdateInquiryRequest`）：`{ status?, assigned_to? }`。`status` 僅接受 `new`／`in_progress`／`resolved`／`closed`（`InquiryStatus` 既有值域），非法值 422。`assigned_to` 可明確傳 `null` 清空指派（清為 `NULL`），欄位不帶則維持原值不動。回應：`InquiryResponse`（見上）。
錯誤：404（查無此 inquiry）。

---

### 3.18 Course Sessions & Weekly Schedule（課程場次與週課表）

課程的結構化週模式（`course_schedule_slots`，見 §3.3 的 `schedule_slots`）與由週模式物化到實際日期的上課場次（`course_sessions`）。`course_schedule_slots`／`course_sessions` 資料表本身皆**無 `status` 欄位**——v1 不支援停課；但 `course_sessions` 的回應（`CourseSessionResponse`/`TodaySessionResponse`）會附上後端即時推導的 `status`（`upcoming`/`ongoing`/`done`），前端不需再自行依 `start_time`/`end_time` 判斷，見下方裁決 4。

**裁決**：
1. **與 `time_slots`（§3.6 Schedule，場館時段行事曆）是完全不同的資源**，彼此不共用資料表、不互相影響。`GET /schedule`（月曆）、`POST /schedule/slots` 等既有端點語意不變；本節端點（含 `GET /schedule/me`）是課程本身的週課表/場次，路徑雖有 `schedule` 前綴但與場館時段是兩回事，前端請勿混用。
2. **時間採牆鐘（wall-clock）語意**：`session_date`／`start_time`／`end_time` 皆為 naive 值（無時區資訊），直接對應館所課表上的日期與時刻。本節所有「今天」的判定（`GET /sessions/today`，以及 `GET /courses/{id}/sessions` 未帶 `from` 時的預設值）一律為 **`studio_timezone`（`Asia/Taipei`）的當地日期**——伺服器以 UTC 當下時間轉換至館所時區後取日期，與 `schedule`/`bookings` 模組的時區慣例一致。因此台北清晨（例如 07:00，等於前一日 23:00 UTC）呼叫 `GET /sessions/today`，拿到的是台北的「今天」，不會因 UTC 日期落後而偏移一天。
3. `day_of_week` 為 **0=Sunday .. 6=Saturday**（PostgreSQL `EXTRACT(DOW)` 慣例，也是 JavaScript `Date.getDay()` 慣例）——`course_schedule_slots` 與 `GET /schedule/me` 回應皆遵循此編碼。
4. **`status`（`upcoming`/`ongoing`/`done`）是牆鐘衍生值，不是狀態機**：沒有 `suspended`/`cancelled` 等額外狀態，每次讀取當下即時計算、不落地儲存。邊界採 **[start, end) 閉開**——`now == start_time` 即 `ongoing`，`now == end_time` 即 `done`，三態剛好無縫銜接。換算 `session_date`+`start_time`/`end_time` 為 UTC 時如遇 DST 造成當地時間不存在或有歧義（裁決 2 的換算規則），**降級為以 studio-local 日期層級比較**：`session_date` 早於今天 → `done`；晚於今天 → `upcoming`；等於今天則依「是否已開始」二分為 `ongoing`/`upcoming`——不會讓端點因此報錯（`Asia/Taipei` 無 DST，此分支 production 不可達）。前端原本若有自行依 `start_time`/`end_time` 推導狀態的邏輯，現在可以直接淘汰，改讀這裡的 `status`。

#### `GET /courses/{id}/sessions?from=YYYY-MM-DD&to=YYYY-MM-DD` — 需登入
先物化（依該課程的 `schedule_slots`，為 `[from, to]` 範圍內尚未存在的場次執行 `INSERT ... ON CONFLICT DO NOTHING`；重複呼叫同一範圍不會產生重複場次），再回傳該課程在此範圍內的場次列表。`from`/`to` 皆選填：預設 `from=今天`、`to=from+28 天`（只給其中一個時，另一個仍依此規則相對計算）。422：`to < from`，或範圍跨距（`to - from`）超過 **60 天**（剛好 60 天可接受）。404：課程不存在。

回應（`CourseSessionResponse[]`，純陣列，依 `session_date, start_time` 排序）：

```jsonc
[
  { "id": "uuid", "course_id": "uuid", "session_date": "YYYY-MM-DD",
    "start_time": "HH:MM:SS", "end_time": "HH:MM:SS",
    "status": "upcoming|ongoing|done" }
]
```

#### `GET /sessions/today` — admin 或 coach
教練：先物化、再只回「自己課程」（`courses.coach_id` 對應呼叫者的 `coaches.id`）今日場次；若呼叫者掛 `coach` 角色但查無對應 `coaches` 資料列（資料異常），回空陣列而非錯誤。admin：物化並回**全部課程**今日場次。回應（`TodaySessionResponse[]`，純陣列，依 `start_time` 排序，教練與 admin 兩分支共用同一回應型）：

```jsonc
[
  { "id": "uuid", "course_id": "uuid", "course_name": "string",
    "coach_name": "string|null",
    "start_time": "HH:MM:SS", "end_time": "HH:MM:SS",
    "enrolled_count": "number", "venue": "string|null",
    "status": "upcoming|ongoing|done" }
]
```

`enrolled_count` 為即時計算（該課程 `enrolments.status='active'` 筆數）。`coach_name`（Round 4 Task B8 新增）為 `null` 表示該課程尚未指定教練，語意同 `GET /schedule/me` 的 `coach_name`。`venue`（同批新增）由該場次的日期反推 `day_of_week` + `start_time`，回頭 JOIN `course_schedule_slots`（`course_schedule_slots_unique (course_id, day_of_week, start_time)` 為可逆鍵）取得；找不到對應 slot（slot 已被修改或刪除）時為 `null`。

#### `GET /schedule/me` — 需登入
回呼叫者「active enrolments 對應課程」的週模式（**不物化，直接讀 `course_schedule_slots`**——與上面兩個端點不同，這裡回的是週模式本身，不是實際日期場次）。回應（`MyScheduleEntryResponse[]`，純陣列，依 `day_of_week, start_time` 排序）：

```jsonc
[
  { "course_id": "uuid", "course_name": "string", "coach_name": "string|null",
    "day_of_week": 0, "start_time": "HH:MM:SS", "end_time": "HH:MM:SS",
    "venue": "string|null" }
]
```

`coach_name` 為 `null` 表示該課程尚未指定教練（`courses.coach_id IS NULL`）。

---

### 3.19 Attendance（出勤/點名）

`attendance_records`：每筆代表某場次（`course_sessions`）中、某筆報名（`enrolments`）的出勤狀態，`status` 為 `present`/`absent`/`leave` 三選一。`UNIQUE(session_id, enrolment_id)`——同一場次對同一筆報名只會有一筆紀錄，重複點名是「覆寫」而非新增一筆。

**裁決**：
1. 權限採「該課教練或 admin」：`courses.coach_id` 指向的 `coaches` 列之 `user_id` 等於呼叫者，才算「該課教練」；非本課教練（含掛 `coach` 角色但教的是別堂課）一律 403。呼叫者掛 `coach` 角色但查無對應 `coaches` 資料列（資料異常）同樣視為非本課教練 → 403（與 `GET /sessions/today` 查無資料時「降級為空陣列」不同——這裡是存取單一場次資源，403 才是正確語意）。
2. `PUT /sessions/{id}/attendance` 的驗證發生在任何寫入之前：先驗證每筆 `status` 是合法值、每筆 `enrolment_id` 都屬於該場次所在課程且狀態為 `active`；只要有一筆不符合，**整批 422 拒絕，零寫入**（即使批次中其餘筆數本身合法有效）。
3. `GET /coaches/me/students` 僅限 `coach` 角色（無 admin 例外）。「我的 active 課程」＝ `courses.coach_id` 指向呼叫者的課程且 `is_active = true`；「active enrolments」＝該課程 `enrolments.status = 'active'`。同一學員在此教練名下多堂課皆有效報名時只會出現一筆，`courses` 欄位彙整該學員在這位教練名下的所有課程，每筆課程條目皆帶該學員在該課程的 `enrolment_id`（供前端「寫評語」呼叫 `POST /report-cards` 使用）。
4. `PUT /sessions/{id}/attendance` 要求場次已經開始才能點名（與 §3.20 請假「開課前皆可申請」極性相反）：「已開始」的判定與 §3.18 裁決 2 一致，以 `studio_timezone` 當地牆鐘時間比較 `session_date`+`start_time` 與呼叫當下，開始瞬間本身即視為已開始（含界，同 `has_started`）；尚未開始 → 422（訊息「場次尚未開始，無法點名」）。此檢查發生在裁決 1 的教練/admin 權限驗證之後、裁決 2 的批次內容驗證之前——**即使 `records` 為空陣列，未開始場次一樣回 422**（空批次不再是恆成功的 no-op；行為變更，舊版無此檢查恆回 200）。

#### `GET /sessions/{id}/roster` — admin 或該課教練
該場次名冊：課程的 active enrolments JOIN `users`，並 LEFT JOIN 這個場次自己的出勤紀錄（尚未點名的學員該欄位為 `null`）。404：場次不存在。403：非本課教練。

回應（`RosterEntryResponse[]`，純陣列，依學員姓名排序）：

```jsonc
[
  { "enrolment_id": "uuid", "user_id": "uuid", "user_name": "string",
    "attendance_status": "present|absent|leave|null" }
]
```

#### `PUT /sessions/{id}/attendance` — admin 或該課教練
Body：`{ "records": [ { "enrolment_id": "uuid", "status": "present|absent|leave" }, ... ] }`。批次 upsert（`ON CONFLICT (session_id, enrolment_id) DO UPDATE`）——重複呼叫同樣的 body 冪等、不會產生重複紀錄；同一 enrolment 再次點名會覆寫先前狀態，不影響 `created_at`。回應：更新後的完整名冊（`RosterEntryResponse[]`，與 `GET /sessions/{id}/roster` 同形狀）。

錯誤：404（場次不存在）；403（非本課教練）；422（場次尚未開始，訊息「場次尚未開始，無法點名」，見上方裁決 4——**此檢查先於下方這條，即使 `records` 為空陣列也會觸發**）；422（`status` 不是 `present`/`absent`/`leave` 之一，或任何 `enrolment_id` 不屬於該場次所在課程／狀態非 `active`——**整批拒絕，零寫入**，見上方裁決 2）。

#### `GET /coaches/me/students` — coach
呼叫者（教練）「active 課程」的「active enrolments」，去重後的學員清單。回應（`MyStudentResponse[]`，純陣列，依學員姓名排序）：

```jsonc
[
  { "user_id": "uuid", "name": "string", "phone": "string|null",
    "courses": [ { "course_id": "uuid", "course_name": "string", "enrolment_id": "uuid" } ] }
]
```

呼叫者掛 `coach` 角色但查無對應 `coaches` 資料列時回空陣列（非錯誤，同 `GET /sessions/today` 的慣例）。這是前端「我的學員」列表（FE getStudents）的資料源；每筆 `courses` 條目的 `enrolment_id` 是該學員在該課程的 active enrolment id，前端「寫評語」需以此呼叫 `POST /report-cards`。

---

### 3.20 Leave Requests（請假/補課）

`leave_requests`：會員針對某一堂已報名課程的特定場次申請請假，由該課教練或 admin 審核。`status` 為 `pending`/`approved`/`rejected`/`cancelled` 四選一；`UNIQUE(enrolment_id, session_id)`（partial，僅 `pending`/`approved` 兩狀態生效）——同一場次的請假若已被取消或駁回，允許重新申請同一場次。

**裁決 4（請假規則 v1，任務規格原文）**：開課前皆可申請（無最短提前期）；教練（該課）或 admin 審核；核准即在該場次寫入 attendance `leave` 紀錄；一張核准假單可預約一次同課程未來場次補課，補課受名額檢查。

其他細節：
- 「場次已開始」的判定與 §3.18 裁決 2 一致：以 `studio_timezone`（`Asia/Taipei`）當地牆鐘時間比較 `session_date`+`start_time` 與呼叫當下（`POST /leave-requests` 檢查原場次；`POST /leave-requests/{id}/makeup` 檢查補課目標場次）。
- `DELETE /leave-requests/{id}` **僅本人（owner）可取消，無 admin 例外**——與本節其餘教練/admin 端點的權限模式不同；admin/教練若要否決一張假單，走 `PATCH` 駁回。
- 核准/駁回後，會對該學員寫入一筆 `system` 類型 notification（見 §3.15），文案：「你的請假申請已核准：{課程名} {場次日期}」或「你的請假申請已婉拒：{課程名} {場次日期}」（`session_date` 為 `YYYY-MM-DD`）。
- `makeup_session_id`/`makeup_session_date`/`makeup_start_time` 在假單尚未預約補課時皆為 `null`；只有 `POST /leave-requests/{id}/makeup` 成功後才會補上。

#### `POST /leave-requests` — 需登入
Body：`{ session_id: "uuid", reason?: "string" }`（`reason` 最長 500 字，選填）。伺服器由 `session_id` 找出所屬課程，再找呼叫者在該課程的 active enrolment。回應（`LeaveRequestResponse`）：

```jsonc
{
  "id": "uuid", "course_id": "uuid", "course_name": "string",
  "session_id": "uuid", "session_date": "YYYY-MM-DD", "start_time": "HH:MM:SS",
  "reason": "string|null", "status": "pending",
  "makeup_session_id": null, "makeup_session_date": null, "makeup_start_time": null,
  "decided_at": null, "created_at": "ISO8601"
}
```

錯誤：404（場次不存在；或呼叫者在該課程無 active enrolment，訊息「未報名此課程」——兩者是不同的 404 情境，各自獨立判定）；422（場次已開始，訊息「場次已開始，無法請假」）；409（`(enrolment_id, session_id)` 已有 `pending`/`approved` 的請假紀錄，訊息「此場次已有請假紀錄」）。

#### `GET /leave-requests/me` — 需登入
回應：`LeaveRequestResponse[]`（**純陣列，不分頁**，新到舊）——形狀同上，每筆皆含 `makeup_session_id`/`makeup_session_date`/`makeup_start_time`。

#### `DELETE /leave-requests/{id}` — 需登入（僅本人 owner，無 admin 例外）
無 body。僅 `status = "pending"` 的假單可取消 → 更新為 `cancelled`。回應：**204 No Content**。錯誤：404（不存在）；403（非本人）；409（非 pending，例如已核准/已駁回/已取消）。

#### `GET /leave-requests?status=&course_id=` — admin 或該課教練
分頁列表；`status`（`pending`/`approved`/`rejected`/`cancelled`）與 `course_id` 皆選填。教練僅能看到自己教的課程（`courses.coach_id` 對應的 `coaches.user_id` = 呼叫者）；admin 看全部。回應（`LeaveRequestListResponse`）：

```jsonc
{
  "leave_requests": [
    { "id": "uuid", "course_id": "uuid", "course_name": "string",
      "user_id": "uuid", "user_name": "string",
      "session_id": "uuid", "session_date": "YYYY-MM-DD", "start_time": "HH:MM:SS",
      "reason": "string|null", "status": "pending|approved|rejected|cancelled",
      "makeup_session_id": "uuid|null", "makeup_session_date": "YYYY-MM-DD|null",
      "makeup_start_time": "HH:MM:SS|null",
      "decided_at": "ISO8601|null", "created_at": "ISO8601" }
  ],
  "total": "number", "page": "number", "per_page": "number"
}
```

呼叫者掛 `coach` 角色但查無對應 `coaches` 資料列時回空頁（`leave_requests: []`, `total: 0`）而非錯誤，同 §3.18/§3.19 既有慣例。`status` 帶入無法辨識的值回 422。

#### `PATCH /leave-requests/{id}` — admin 或該課教練
Body：`{ status: "approved" | "rejected" }`（其他任何值，包含 `pending`/`cancelled`，一律 422）。僅 `status = "pending"` 的假單可審核。**核准在同一交易內**完成兩件事：更新假單為 `approved`（寫入 `decided_by`/`decided_at`），並 upsert 該場次的 `attendance_records` 為 `status = 'leave'`（`marked_by` = 決定者）；駁回僅更新假單狀態，**不寫入**任何出勤紀錄。決定完成（交易提交後）才同步寫入通知，見上方「其他細節」。回應：更新後的 `LeaveRequestResponse`（此時 `makeup_session_id` 等欄位必為 `null`——補課須另呼叫下方端點）。

錯誤：404（不存在）；403（非本課教練且非 admin）；409（非 pending）；422（`status` 非 `approved`/`rejected`）。

#### `POST /leave-requests/{id}/makeup` — 需登入（僅本人 owner）
Body：`{ session_id: "uuid" }`（欲預約的補課目標場次）。驗證順序：假單須為 `approved` 且尚未預約過補課（`makeup_session_id IS NULL`，否則 409）→ 目標場次須與原假單同一課程（否則 422）→ 目標場次須尚未開始（否則 422）→ 名額檢查（見下，否則 409）。成功寫入 `makeup_session_id`，回應更新後的 `LeaveRequestResponse`（`makeup_session_date`/`makeup_start_time` 補上目標場次的日期/時間）。

**名額公式（物理座位模型，controller 定案 2026-07-06）**：目標場次剩餘座位 = `course.max_students − 該課程 active enrolments 數 + 該場次核准請假數 − 已補進該場次的補課數`，剩餘 `> 0` 才允許預約——**請假釋出座位、補課佔用座位**。兩個計數皆只計 enrolment 仍為 `active` 者：請假後退課的人不釋出幽靈座位，補課後退課的人不繼續佔位。範例：`max_students=10`、active 10 人（滿班）、該場次 3 人核准請假、0 人補課 → 剩餘 `10−10+3−0=3`，可補課；`max_students=10`、active 8 人、0 請假、已 2 人補進 → 剩餘 `10−8+0−2=0`，409。

**併發防護**：同一交易內以 `FOR UPDATE` 先鎖假單列（防**同一張假單**重複預約補課），再於名額計數前鎖**目標場次列**（序列化**不同假單**搶同一場次名額——最後一席只會有恰好一人成功，其餘 409「該場次名額已滿」）。

錯誤：404（假單或目標場次不存在）；403（非本人）；409（假單非 `approved`、或已預約過補課、或名額已滿）；422（目標場次跨課程、或目標場次已開始）。

---

### 3.21 Messages（訊息中心）

`conversations`/`messages`：教練與會員之間的一對一對話。**每對使用者僅有一個對話（無序對唯一）**——DB 以 `UNIQUE (LEAST(member_id, coach_id), GREATEST(member_id, coach_id))` 強制，無論由哪一方發起、雙方各自持有哪些角色（含同時具 coach+member 的使用者），同一對使用者的 `POST /conversations` 都 get-or-create 回同一筆，不會重複建立。**v1 不支援檔案附件**——前端 mock 資料中的 `sharedFiles` 欄位本 API 不提供，前端須自行處理（例如隱藏該區塊或留待後續版本）。

角色規則：對話的兩端需一端具 `coach` 角色、另一端具 `member` 角色；呼叫者可為任一端（`user_id` 帶對方即可，順序無關——先前已建立的對話無論由哪一方再次呼叫都會 get-or-create 回同一筆）。違反回 422「僅支援教練與會員間的對話」，涵蓋：兩端角色相同（皆 member 或皆 coach）、任一端不具 `coach`/`member` 任一角色（例如純 admin）、或 `user_id` 等於呼叫者自己。回應正規化為 `member_id`/`coach_id`（不是「呼叫者/對方」）。

#### `POST /conversations` — 需登入（member 或 coach）
Body：`{ user_id: "uuid" }`（對方的 user id）。回應（`ConversationResponse`）：

```jsonc
{
  "id": "uuid", "member_id": "uuid", "coach_id": "uuid",
  "created_at": "ISO8601", "last_message_at": "ISO8601|null"
}
```

錯誤：422（角色驗證失敗，見上方「角色規則」）。

#### `GET /conversations/me` — 需登入
回應：`ConversationSummaryResponse[]`（**純陣列，不分頁**），依 `last_message_at DESC NULLS LAST, created_at DESC` 排序（尚無訊息的對話排最後；同刻/皆無訊息時以建立時間新到舊穩定排序）：

```jsonc
{
  "id": "uuid", "peer_id": "uuid", "peer_name": "string",
  "last_message_body": "string|null", "last_message_at": "ISO8601|null",
  "unread_count": "number"
}
```

`peer_id`/`peer_name` 是「對方」——呼叫者是 member 就回 coach 那端，反之亦然；`peer_name` 取自 `users.name`。`last_message_body` 是該對話最新一則訊息內容，截斷至 100 字（尚無訊息則為 `null`）。`unread_count` 為「對方寄出、且尚未讀取」的訊息數（`sender_id <> 呼叫者 AND read_at IS NULL`）——呼叫者自己寄出的訊息永遠不計入自己的 `unread_count`。單一查詢聚合完成（`last_message_body`/`unread_count` 皆為 correlated subquery），無 N+1。

#### `GET /conversations/{id}/messages?page=&per_page=` — 需登入（僅參與者）
分頁列表，`created_at DESC`（新到舊）。回應（`MessageListResponse`）：

```jsonc
{
  "messages": [
    { "id": "uuid", "sender_id": "uuid", "body": "string",
      "created_at": "ISO8601", "read_at": "ISO8601|null" }
  ],
  "total": "number", "page": "number", "per_page": "number"
}
```

錯誤：404（對話不存在）；403（呼叫者非該對話 member/coach 任一方）。

#### `POST /conversations/{id}/messages` — 需登入（僅參與者）
Body：`{ body: "string" }`（長度需 1 到 2000 字，DB CHECK 與 API validator 兩層皆驗證）。**同一交易**內寫入訊息並更新該對話的 `last_message_at` 為當下時間。回應：新訊息（`MessageResponse`，形狀同上方列表項目；`sender_id` 為呼叫者自己，`read_at` 必為 `null`）。

錯誤：404（對話不存在）；403（非參與者）；422（`body` 長度不在 1–2000）。

#### `PATCH /conversations/{id}/read` — 需登入（僅參與者）
無 body。將該對話中「對方寄出、且尚未讀取」的訊息全數標記為已讀（`read_at = now()`）——**只影響對方寄出的訊息，呼叫者自己寄出的訊息不受影響**。回應：`{ "updated": "number" }`（本次標記已讀的訊息數）。

錯誤：404（對話不存在）；403（非參與者）。

---

### 3.22 Report Cards & Certificates（成績單/證書）

`report_cards`/`certificates`：教練發放給學員的期別成績單與證書。**兩者皆為純 metadata，無 PDF/檔案儲存**（裁決 6，任務規格原文）——沒有上傳、下載或任何檔案欄位。

**report_cards**：教練對學員「單一 enrolment」在某期別（`term_label`）的評語/評分。`UNIQUE(enrolment_id, term_label)`——同一筆 enrolment 同一期別僅能建立一次成績單，重複回 409。`rating` 選填，範圍 1–5（DB CHECK 與 API `validator` 皆驗證，0 或 6 皆回 422）。

**certificates**：學員獲頒的證書，`course_id` 選填（可為 `NULL`，不綁定特定課程）。

角色規則：兩端點皆僅 `coach`/`admin` 可呼叫（純 member 一律 403）：
- `POST /report-cards`：admin 皆可；coach 僅限**自己課程**的 enrolment（`courses.coach_id` 對應的 `coaches.user_id` = 呼叫者），否則 403「非本課教練」。
- `POST /certificates`：admin 皆可；coach 僅限「曾是或現是自己課程學員」的使用者——`user_id` 需在呼叫者任一課程有 enrolment（`active` 或 `cancelled` 皆可，歷史學員也可領證），否則 403「僅能發給自己課程的學員」。此檢查與 request body 的 `course_id` 無關——即使 `course_id` 留空或指向其他課程，只要該學員曾在教練任一課程報名即可核發。

證書發放成功會對該學員寫入一筆 `system` 類型 notification（見 §3.15），文案：「你獲得了新證書：{title}」。

#### `POST /report-cards` — admin 或該課教練
Body：`{ enrolment_id: "uuid", term_label: "string", comment?: "string", rating?: number }`（`term_label` 1–100 字；`rating` 選填，1–5）。回應（`ReportCardResponse`）：

```jsonc
{
  "id": "uuid", "course_id": "uuid", "course_name": "string",
  "term_label": "string", "comment": "string|null", "rating": "number|null",
  "created_by_name": "string", "created_at": "ISO8601"
}
```

錯誤：404（`enrolment_id` 不存在，訊息「報名紀錄不存在」）；403（coach 並非該 enrolment 所屬課程的教練，訊息「非本課教練」）；409（`(enrolment_id, term_label)` 已存在，訊息「此期別已建立過成績單」）；422（`rating` 不在 1–5、或 `term_label` 長度不符）。

#### `GET /report-cards/me` — 需登入
回應：`ReportCardResponse[]`（**純陣列，不分頁**），新到舊。僅回傳呼叫者自己（透過其 enrolments）的成績單。

#### `POST /certificates` — admin 或教練（限自己課程學員）
Body：`{ user_id: "uuid", course_id?: "uuid", title: "string", level?: "string", issued_on: "YYYY-MM-DD", note?: "string" }`（`title` 1–200 字；`level` 選填，至多 100 字）。回應（`CertificateResponse`）：

```jsonc
{
  "id": "uuid", "course_id": "uuid|null", "course_name": "string|null",
  "title": "string", "level": "string|null", "issued_on": "YYYY-MM-DD",
  "note": "string|null", "created_at": "ISO8601"
}
```

錯誤：403（coach 且該學員不具呼叫者任一課程的 enrolment，訊息「僅能發給自己課程的學員」）；422（`title`/`level` 長度不符）。

#### `GET /certificates/me` — 需登入
回應：`CertificateResponse[]`（**純陣列，不分頁**），新到舊。僅回傳呼叫者自己的證書。

---

### 3.23 Rewards（點數兌換）

`rewards`/`reward_redemptions`：可用點數兌換的品項目錄與兌換紀錄。**點數扣減沿用既有 `point_ledger` + `users.points_balance` 機制（裁決 7）**——兌換不是第二套點數系統，只是 `point_ledger` 多一種 `reason`（`"redeem"`，見 §1.6）。

`stock`：`null` = 不限量；有限量的品項兌換完（`stock` 降到 `0`）後，後續兌換一律 409。`is_active = false` 的品項對 member 如同不存在（列表濾除、兌換回 404）。

**`POST /rewards/{id}/redeem` 為單一交易，依序**：鎖品項列（`FOR UPDATE`）→ 檢查 `is_active`（否則 404）→ 檢查 `stock`（`null` 略過；`0` → 409）→ 鎖並檢查呼叫者 `users.points_balance`（不足 `points_cost` → 409）→ 寫入 `point_ledger`（`delta = -points_cost`，`reason = "redeem"`）並同步 `users.points_balance` → `stock` 非 `null` 才 `-1` → 插入 `reward_redemptions` 紀錄。**併發防護**：兩筆兌換搶同一品項最後一件庫存時，品項列的 `FOR UPDATE` 序列化兩者的庫存檢查，恰好一筆成功，另一筆回 409「已兌換完畢」。

#### `GET /rewards?all=` — 需登入
Member（未帶 `all` 或 `all=false`）：僅回傳 `is_active = true` 的品項，依 `display_order` 排序。`all=true` 需 admin，回傳含 inactive 在內的全部品項（排序不變）；非 admin 帶 `all=true` 回 403。回應（`RewardListResponse`，**純陣列，不分頁**）：

```jsonc
{
  "rewards": [
    { "id": "uuid", "name": "string", "description": "string|null",
      "points_cost": "number", "stock": "number|null", "is_active": "boolean",
      "display_order": "number", "created_at": "ISO8601", "updated_at": "ISO8601" }
  ]
}
```

錯誤：403（非 admin 帶 `all=true`）。

#### `POST /rewards/{id}/redeem` — 需登入
無 body。成功回應：

```jsonc
{ "redemption_id": "uuid", "points_spent": "number", "balance_after": "number" }
```

錯誤：404（品項不存在或 `is_active = false`，訊息「獎勵不存在」）；409（庫存為 `0`，訊息「已兌換完畢」；或點數餘額低於 `points_cost`，訊息「點數不足」）。

#### `GET /rewards/redemptions/me?page=&per_page=` — 需登入
回應（`RedemptionListResponse`）：

```jsonc
{
  "redemptions": [
    { "id": "uuid", "reward_id": "uuid", "reward_name": "string",
      "points_spent": "number", "created_at": "ISO8601" }
  ],
  "total": "number", "page": "number", "per_page": "number"
}
```

新到舊，僅回傳呼叫者自己的兌換紀錄。`reward_name` 為即時 join 目前的品項名稱（非兌換當下快照）——品項改名後，舊兌換紀錄顯示的名稱會跟著變動。

#### `POST /rewards` / `PATCH /rewards/{id}` — admin
Create body：`{ name, description?, points_cost, stock?, display_order? }`（`name` 1–200 字；`points_cost` 需 > 0；`stock` 選填且 >= 0，留空即不限量；`display_order` 選填，預設 `0`）。新建品項一律 `is_active = true`。回應：建立後的品項（形狀同 `GET /rewards` 陣列中的單筆）。

Update 為對應欄位皆選填的 PATCH：`{ name?, description?, points_cost?, stock?, is_active?, display_order? }`。`description`/`stock` 可明確傳 `null` 清空（`description` 清為 `NULL`；`stock` 清為 `NULL` 即改為不限量），欄位不帶則維持原值不動。

錯誤：404（`PATCH` 對象不存在，訊息「獎勵不存在」）；422（`name`/`points_cost` 不符驗證範圍）。

---

### 3.24 Reports（報表）

三個彙總報表端點（admin/coach/member），**純聚合查詢，無新增資料表**。裁決 9：實作前先讀前端三個 surface 既有的 mock 報表形狀（`admin/api.ts`+`data.ts` 的 `getReports()`/`ReportsData`、`coach/api.ts` 的 `getDashboard()`、`member/api.ts` 的 `getReports()`/`REPORTS`），盤點出「有真實資料源」的欄位才收進本契約；沒有資料源的維持既有 mock（見本節末「mock 有但契約無」清單）。

**裁決**：
1. 三端點皆為**單一物件**回應——不是陣列，也不分頁（與本文件其餘大多數 GET 端點不同）。
2. 「今日」與月份邊界一律採 `studio_timezone`（見 §3.18 裁決 2）的當地時間；`revenue.trend`/`members.new_this_month` 的月份切分也依此換算，而非 DB session 所在時區。
3. `attendance_rate`（member）與 `attendance_rate_30d`（coach）定義相同：`present / (present + absent)`；`leave` 不計入分子、也不計入分母；無出勤資料時回 `null`（不是 `0`）。
4. `fill_rate`（admin `courses[]`）定義為 `enrolled / max_students`。`max_students` 現有 `CHECK (max_students > 0)` 保證恆為正，但計算仍防禦性地在分母為 `0` 時回 `null`，不產生除以零（`NaN`/`Infinity` 無法序列化為合法 JSON）。
5. `GET /reports/coach` 用 `require_role("coach")`——**單一角色檢查，無 admin 例外**（與部分教練資源端點如 `GET /sessions/today` 的「admin 或 coach」不同：admin 若未同時掛 `coach` 角色，呼叫本端點一律 403）。呼叫者掛 `coach` 角色但查無對應 `coaches` 資料列 → **404**（訊息「coach not found」，比照 `coaches` 模組本身查無資料列時的既有慣例）——這點與 `GET /sessions/today`／`GET /coaches/me/students` 遇到同一資料異常時「降級回空陣列」不同：本端點回傳單一物件而非列表，沒有自然的「空」值可用，零值/null 會與「有效教練但剛好沒有學員」混淆，故改用 404 明確表達「找不到教練身分」。
6. `GET /reports/admin/activity`（Round 4 Task B8 新增，見下）是本節唯一的例外：回應為 `{ "items": [...] }` 陣列包裝，不是單一物件；僅 admin（不是 admin/coach/member 三選一）。

#### `GET /reports/admin` — admin

```jsonc
{
  "revenue": {
    "this_month_cents": "number",
    "last_month_cents": "number",
    "trend": [
      { "month": "YYYY-MM", "revenue_cents": "number" }
    ]
  },
  "kpis": {
    "new_members":       { "this_month": "number", "last_month": "number" },
    "new_enrolments":    { "this_month": "number", "last_month": "number" },
    "paid_orders_count": { "this_month": "number", "last_month": "number" },
    "attendance_rate":   { "this_month": "number|null", "last_month": "number|null" }
  },
  "revenue_breakdown": [
    { "source": "course|ticket|membership|course_package|merchandise|venue_rental",
      "gross_cents": "number", "orders_count": "number", "units": "number" }
  ],
  "income_sources_12m": [
    { "month": "YYYY-MM", "source": "…（同上 6 值）",
      "gross_cents": "number", "orders_count": "number", "units": "number" }
  ],
  "category_split": [
    { "source": "course|ticket|membership|course_package|merchandise",
      "gross_cents": "number", "ratio": "number|null" }
  ],
  "payment_split": [ { "method": "string", "count": "number" } ],
  "attendance_distribution": [ { "bucket": "gte_95|85_94|75_84|lt_75", "count": "number" } ],
  "age_distribution":        [ { "bucket": "0-6|7-12|13-17|18-25|26-40|41+", "count": "number" } ],
  "tier_distribution":       [ { "bucket": "regular|bronze|silver|gold", "count": "number" } ],
  "retention": [
    { "month": "YYYY-MM", "new_count": "number", "returning_count": "number", "rate": "number|null" }
  ],
  "funnel": { "trial_inquiries": "number", "new_enrolments": "number" },
  "weekday_load": [ { "weekday": "number（0=週日..6=週六）", "present_count": "number" } ],
  "venue_usage": [ { "venue": "string", "minutes": "number" } ],
  "members": { "total": "number", "new_this_month": "number", "active": "number" },
  "courses": [
    { "course_id": "uuid", "name": "string", "enrolled": "number",
      "max_students": "number", "fill_rate": "number|null", "waitlist_count": "number" }
  ],
  "coaches": [
    { "coach_id": "uuid", "name": "string", "course_count": "number", "student_count": "number",
      "revenue_cents_12m": "number", "attendance_rate": "number|null" }
  ]
}
```

Round 4 Phase 4 分兩批擴充本端點（皆為 additive，不新增端點也不新增資料表）：金流組（Task P4-B4a）加 `kpis`/`revenue_breakdown`/`income_sources_12m`/`category_split`/`payment_split` 與 `coaches[].revenue_cents_12m`；人流組（Task P4-B4b）加 `attendance_distribution`/`age_distribution`/`tier_distribution`/`retention`/`funnel`/`weekday_load`/`venue_usage` 與 `coaches[].attendance_rate`。共通口徑：**月界與「今日」一律 studio 時區**（§3.18 裁決 2；`AT TIME ZONE` + `date_trunc`）；**金額聚合一律折扣前毛額**（order line `unit_price_cents × quantity`，order 層 `discount` 不攤分，與 `revenue` 的「實收」`total_cents` 口徑不同），且**排除 `pending`/`refunded`**（`status ∈ REVENUE_STATUSES`，非 `paid_at IS NOT NULL`）；比率一律 `0–1`，分母為 0 → `null`（非 `0`/`NaN`）。

- `revenue`：僅計 orders **paid 家族**（`status IN ('paid','processing','completed')`）；`refunded`/`cancelled`/`pending` 一律不計（退款訂單即使 `paid_at` 仍留有原值也不計，見 §1.8）。以 `paid_at` 歸月。`trend` 固定 **12 筆**，由舊到新，缺資料月份補 `0`（非省略該月）；`this_month_cents`/`last_month_cents` 即 `trend` 最後兩筆。
- `kpis`：四組 this/last studio-月對。`new_members`=`users` created（不分角色，同 `members.new_this_month`）；`new_enrolments`=`enrolments` created 且 `status <> 'cancelled'`；`paid_orders_count`=`REVENUE_STATUSES` 訂單依 `paid_at` 歸月；`attendance_rate`=`present/(present+absent)`（`leave` 不入分母，無資料月 → `null`）。環比成長 % 由前端算。
- `revenue_breakdown`：本月 **6 source** 折扣前毛額（`course`=item_type=course 的 line；`ticket`/`membership`/`course_package`/`merchandise` 依 `products.product_type`；**`venue_rental`=`confirmed`/`completed` bookings 的 `price_cents` 快照、歸屬 slot 使用日（非下訂日）**）。固定 6 桶零填 canonical 序。`orders_count`=觸及該 source 的訂單數（場租為 booking 數）；`units`=line quantity 合計（一筆 booking=1）。
- `income_sources_12m`：同上口徑的**近 12 studio 月 × 6 source**＝72 列，零填、由舊到新，與 `revenue.trend` 同窗（`revenue_breakdown` 即其本月切片）。
- `category_split`：本月 order-line 毛額五桶（**不含 `venue_rental`**——場租非 order line）之占比；`ratio` 分母為五桶合計，合計為 `0` → `null`。與 `revenue_breakdown` 同一次聚合派生。
- `payment_split`：本月 `REVENUE_STATUSES` 訂單**筆數** by `payment_method`；`NULL` → `"unknown"` 鍵原樣輸出（前端顯示「其他」）；零筆的 method 不出列（付款方式為應用層值域，非 DB enum，無固定桶可零填）。占比與環比由前端算。
- `attendance_distribution`：每會員 `present/(present+absent)`（`leave` 不入分母；**未點名或僅請假（分母為 0）的會員不入分布**）分入固定 **4 桶** `gte_95`（95–100%）/ `85_94` / `75_84` / `lt_75`（低於 75%），零填。中文標籤前端配。
- `age_distribution`：`users.birth_date` 相對 studio 今日算足歲，分入固定 **6 桶** `0-6`/`7-12`/`13-17`/`18-25`/`26-40`/`41+`，**排除 `birth_date` NULL**，零填。
- `tier_distribution`：`users.points_balance` 分入固定 **4 桶** `regular`（<500）/ `bronze`（500–1999）/ `silver`（2000–4999）/ `gold`（≥5000）；全體 users（同 `members.total` 口徑），零填。中文標籤前端配。
- `retention`：**近 6 studio 月**出席 cohort（由舊到新，6 桶零填）。會員某月有 ≥1 筆 `present` 即「該月活躍」；`new_count`=首次活躍月落在該月者、`returning_count`=該月活躍且此前已有活躍月者；`rate`=`|上月活躍 ∩ 本月活躍| / |上月活躍|`，**上月為空集合 → `null`**。首次活躍判定掃全期歷史（非僅 6 月窗）。
- `funnel`：誠實 **2 段**、近 **90 studio 天**：`trial_inquiries`（`contact_inquiries` 之 `inquiry_type='trial'` 計數）→ `new_enrolments`（`enrolments` created 且 `status <> 'cancelled'`）。不造中間段。
- `weekday_load`：近 **30 天**已物化場次的 `present` **出席人次**按星期分 **7 桶**（`weekday` `0=週日`..`6=週六`，§3.18 慣例），零填。
- `venue_usage`：**本月**（呼叫時先冪等物化本月場次）已物化場次 JOIN `course_schedule_slots`（`course_id`+DOW+`start_time` 可逆鍵）取 `venue`、SUM 場次分鐘數；**`venue` 為 NULL（或無對應 slot）的場次不入**。非固定桶——無場次的場地不出列。此為**整月投影口徑**（先冪等物化整月場次、含未來場次），與其他段落「月初至今」的實績計算不同。
- `members`：`total`/`new_this_month` 為 `users` 全體計數（不分角色）；`active` 為擁有至少一筆 `active` enrolment 的 distinct 使用者數。
- `courses`：全部課程（不篩 `is_active`），依名稱排序；`enrolled` 為該課程 `active` enrolments 數；`waitlist_count` 為 `waiting` 筆數。
- `coaches`：全部教練（不篩 `is_active`），依姓名排序；`course_count` 為其 `courses.coach_id` 對應課程數；`student_count` 為其課程 active enrolments 之 distinct 學員數（同一學員修該教練多堂課只算一次）；`revenue_cents_12m`=**course 類** order-line 毛額歸 `courses.coach_id`（票券/裝備/場租不歸因），近 12 studio 月（與 `revenue.trend` 同窗）；`attendance_rate`=該教練課程 `present/(present+absent)`（`leave` 不入分母，全期；無資料 → `null`）。

空庫（無任何 orders/users/enrolments/courses/coaches）：`revenue` 全 `0`（`trend` 12 筆皆 `0`）、`members` 全 `0`、`courses`/`coaches`/`payment_split`/`venue_usage` 皆為 `[]`；`kpis` 全 `0`（`attendance_rate` 兩欄 `null`）、`funnel` 兩欄 `0`；固定桶各段一律零填其固定桶數（`revenue_breakdown` 6、`income_sources_12m` 72、`category_split` 5、`attendance_distribution` 4、`age_distribution` 6、`tier_distribution` 4、`retention` 6、`weekday_load` 7）——皆不會是 500。

#### `GET /reports/coach` — coach

物化「今日」場次後彙總（同 `GET /sessions/today` 的物化時機）：

```jsonc
{
  "today_sessions": "number",
  "pending_attendance": "number",
  "unread_messages": "number",
  "student_count": "number",
  "attendance_rate_30d": "number|null"
}
```

- `today_sessions`：呼叫者名下課程今日場次數（studio 當地日期）。
- `pending_attendance`：今日場次中「尚無任何一筆 `attendance_records`」者的數量（只要有任一筆紀錄即不算 pending，不要求全班點完）。
- `unread_messages`：呼叫者參與的所有對話中，對方尚未讀訊息總數（跨對話加總，定義同 §3.21 的 `unread_count`）。
- `student_count`：呼叫者 active 課程之 active enrolments 之 distinct 學員數（口徑同 `GET /coaches/me/students`，這裡只回總數）。
- `attendance_rate_30d`：呼叫者名下課程、場次日期落在「今日往前 30 天（含）」內的出勤紀錄，`present/(present+absent)`，`leave` 不計；無資料回 `null`。

錯誤：404（呼叫者掛 `coach` 角色但查無 `coaches` 資料列，見上方裁決 5）。空域（有教練身分但無任何課程/學員/訊息）：`today_sessions`/`pending_attendance`/`unread_messages`/`student_count` 皆 `0`，`attendance_rate_30d` 為 `null`——不會是 500。

#### `GET /reports/me` — 需登入

物化「今日起 7 天」場次後彙總（`from=今天`、`to=今天+7 天`，與 §3.18 `GET /courses/{id}/sessions` 的預設範圍算法一致，即 8 個曆日的區間）：

```jsonc
{
  "attended_total": "number",
  "attendance_rate": "number|null",
  "points_balance": "number",
  "active_enrolments": "number",
  "upcoming_sessions_7d": "number"
}
```

- `attended_total`/`attendance_rate`：呼叫者**所有**報名（不論 enrolment 現在是否仍 `active`——已取消的報名不會抹除已發生的出勤歷史）之出勤紀錄；`attended_total` 為 `present` 筆數，`attendance_rate` 為 `present/(present+absent)`，`leave` 不計；無資料時 `attendance_rate` 回 `null`。
- `points_balance`：即時讀 `users.points_balance`。
- `active_enrolments`：呼叫者 `active` enrolments 數。
- `upcoming_sessions_7d`：呼叫者 active enrolments 對應課程，物化後落在上述 8 天區間內的場次數。

空庫（呼叫者無任何報名/出勤紀錄）：`attended_total`/`active_enrolments`/`upcoming_sessions_7d` 皆 `0`，`attendance_rate` 為 `null`，`points_balance` 為使用者當前餘額（通常 `0`）——不會是 500。

#### `GET /reports/admin/activity` — admin

Admin 桌面「最新動態」面板的資料源（Round 4 Task B8）。UNION 四來源，各自取最近 20 筆再合併依 `occurred_at` 倒序取 20：新註冊會員（`users.created_at`）、新付款訂單（`orders`，見下方裁決）、新報名（`enrolments.created_at`）、新洽詢（`contact_inquiries.created_at`，含 §3.17 的 `inquiry_type`）。

```jsonc
{
  "items": [
    { "kind": "user|order|enrolment|inquiry", "label": "string", "occurred_at": "ISO8601" }
  ]
}
```

- `label` 由後端組成的繁體中文人讀字串，四種 `kind` 各自的模板：
  - `user`：「新會員註冊:{name}」
  - `order`：「訂單 {order_number} 已付款:NT${金額}」——金額為 `total_cents / 100`（整數元，無小數）；這是本端點唯一嵌入金額的欄位，回應形狀沒有另外的數值欄位可放格式化前的 cents。
  - `enrolment`：「新報名:{course_name}」
  - `inquiry`：「新洽詢({inquiry_type}):{subject 或 name}」（`subject` 為空字串時退回 `name`，但目前寫入路徑保證 `subject` 恆非空）
- `kind` 供前端配對應圖示，不做其他語意保證。
- 「已付款」訂單採 `status IN ('paid','processing','completed')`（`orders::model::REVENUE_STATUSES`，與 `GET /reports/admin` 的 `revenue` 計算同一組狀態），而非單看 `paid_at IS NOT NULL`——已退款訂單雖仍保有原始 `paid_at`（見 §1.8），但已離開「已付」狀態家族，不應再被當成一筆新的付款動態呈現。
- 空庫（四張表皆無資料）：`items` 為 `[]`，不是 500。

#### mock 有但契約無（無對應資料源，前端可視情況移除或維持既有 P2 標記）

**Admin**（`admin/api.ts` 的 `getReports()`／`admin/data.ts`）。Round 4 Phase 4（Tasks P4-B4a/P4-B4b）後，`ReportsData` 的多數欄位已有對應資料源（前端形狀對照，非一比一 key 名）：`kpis`→`kpis`、`revenueBreakdown`/`incomeSources`→`revenue_breakdown`/`income_sources_12m`、`categorySplit`→`category_split`、`paymentSplit`→`payment_split`、`venueUsage`→`venue_usage`、`attDist`→`attendance_distribution`、`retention`→`retention`、`ageDist`→`age_distribution`、`tierDist`→`tier_distribution`、`funnel`→`funnel`、`weekdayLoad`→`weekday_load`。仍**無對應資料源**、維持既有 mock 的僅剩：
- `campusRevenue`（分校營收——venues/courses 無分校維度）
- `topCourses`（`{rank,name,count}`——`name`/`count` 可由本契約 `courses[]` 依 `enrolled` 排序近似推導，但 `rank` 純為陣列位置、非資料欄位，形狀也不同，非一比一替換）
- `coachPerf` 的 `revPct`（教練營收占比 %——`students`/`revenue`/`att` 已由本契約 `coaches[].student_count`/`revenue_cents_12m`/`attendance_rate` 取代，僅「個人營收占全館 %」這格為前端由 `revenue_cents_12m` 自行換算，後端不出佔比）

**Coach**（`coach/api.ts` 的 `getDashboard()`）：
- `conversations`（訊息中心完整內容——`getMessages()` 仍為 mock；本任務僅提供 `unread_messages` 總數，對話列表本身走既有 §3.21 `GET /conversations/me`）
- 原三個 P2 佔位欄位 `pendingClasses`/`attendanceRate`/`pendingReplies` 由本契約 `pending_attendance`/`attendance_rate_30d`/`unread_messages` 取代，前端接上後可移除佔位邏輯。

**Member**（`member/api.ts` 的 `getReports()`／`REPORTS`）：
- `getReports()` 整體（`courses`/`reports`/`certs`）是「成績單」（term report + 教練評語 + 技巧評分）功能，與本任務的 `/reports/me` 是完全不同 domain，無對應資料源，維持既有 P2 mock，不受本任務影響。
- 語意上真正對應 `/reports/me` 的其實是首頁 `STATS`（`Stat[]`：報名課程數/本月出席率/會員點數，目前 `getDashboard()` 也還沒串接，仍為 mock）；未來若要串接，`active_enrolments`/`attendance_rate`/`points_balance` 對應到那三張卡，`attended_total`/`upcoming_sessions_7d` 則是 `STATS` 目前沒有的新欄位。

---

### 3.25 Settings（系統設定，全域 key-value）

Admin 桌面「系統設定」頁與 mobile-admin 設定畫面的後端存放層（Round 4 Task B6）。裁決：**最簡 key-value 全域表**，不做細粒度 schema——`key` 為自由字串，不受後端 enum 約束；`value` 為任意合法 JSON（`serde` 自然保證，不逐欄驗證）。「登入裝置清單」**不在本任務範圍**（需 session 管理，另案處理）。

**裁決**：
1. `PUT /settings` 為**部分更新**：僅 upsert body 帶的 key，未帶的 key 維持原值不動；同一交易內完成（`INSERT ... ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = now()`，逐 key 執行後一次 commit）。
2. `settings` 為空物件（`{}`）時視為 **no-op**（200，不寫入任何資料），不是 400——「只 upsert 送來的 key」的自然推論是零 key 送來就零寫入，故不另加特判。
3. 兩端點回應形狀相同：扁平 `{ "settings": { "<key>": <value>, ... } }`（非陣列包裝）；空表回 `{ "settings": {} }`，不是 500。`PUT` 回應為**更新後全量**（整張表的目前狀態），不是只回傳有變動的 key。
4. 本輪前端會用到的慣例 key（**僅文件性列舉，後端不驗證其形狀，也不限制其他 key 名稱**）：
   - `studio_profile`：場館基本資料物件，例如 `{ name, phone, address, default_ratio, max_class_size }`（場館名稱/電話/地址/預設師生比/每班人數上限）。
   - `notification_flags`：通知開關布林物件，例如 `{ email, sms, lowAtt, autoWait }`。
   - `security`：安全性設定布林物件，例如 `{ twoFA }`。

#### `GET /settings` — admin
回應（`SettingsResponse`）：`{ "settings": { "<key>": <value>, ... } }`。空表回 `{ "settings": {} }`。

#### `PUT /settings` — admin
Body（`UpdateSettingsRequest`）：`{ "settings": { "<key>": <value>, ... } }`。逐 key upsert（新 key 建立、既有 key 覆寫且 `updated_at` 更新為當下時間），`value` 可為任意合法 JSON（含巢狀物件/陣列），原樣存取。空 `settings` 物件視為 no-op（200，回傳目前全量狀態，不寫入、`updated_at` 不變）。回應同 `GET /settings`。

---

## 4. 附註

- 所有 `POST`/`PATCH` 成功回應狀態碼皆為 **200**（本專案沒有任何端點回 201 Created）；`DELETE` 與 `POST /cart` 的清空動作回 **204 No Content**（無 body）。
- Enum 型欄位（`level`、`product_type`、`status` 等）在 JSON 中一律是小寫 `snake_case` 字串（例：`course_package`），與 DB enum label 一致。
- `TEXT[]` 欄位（`features`、`specialties`、`certifications`）序列化為 JSON 字串陣列。
- 時間戳一律 `TIMESTAMPTZ` → ISO8601（含時區，UTC）；`date`/`time` 型欄位（schedule 相關）為不含時區的 `YYYY-MM-DD` / `HH:MM:SS`。
