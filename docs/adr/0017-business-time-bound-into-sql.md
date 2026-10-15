# ADR-0017: 業務時間以 handler 取樣值綁進 SQL;稽核時間留給 `NOW()`

## Context

時鐘 seam(`utils::clock`)讓 handler 取樣一次 `StudioNow`,往下傳給 service,牆鐘邏輯因此可以對固定
時刻測試。但 seam 只管到 Rust 這一層:repository 的 SQL 仍有一批欄位與判斷直接寫 PostgreSQL 的
`NOW()`,取的是資料庫伺服器自己的時鐘。`src/utils/clock.rs` 模組文件把這件事列成「未覆蓋清單」。

這些 `NOW()` 並不都無害。其中一部分是報表與規則實際拿來用的時間:

- `orders.paid_at` / `orders.created_at`:營收趨勢、KPI、動態流依 `paid_at` 的 studio 月份分桶;
- `enrolments.enrolled_at` / `enrolments.created_at`:新報名數依 `created_at` 分桶、留存窗口以它比較;
- `contact_inquiries.created_at`:漏斗的「90 天窗」拿它跟 `today - 90` 比;
- 優惠券的 `expires_at > now()`:結帳與驗證端直接拿它判斷有效性。

同一筆結帳,`order_number` 的日期取自 handler 的 `now`,`paid_at` 卻取自 DB 時鐘——兩者在正式環境只差
毫秒,在固定時鐘的測試裡卻可以差好幾年。結果是:這些規則無法用 `MockClock` 測(在 2020 年結帳、
優惠券 2021 年到期,現在會被當成過期回 422),而 seed 的「時戳恆不晚於 `at.now`」不變量只能靠 seed
自己直寫歷史列來維持。

## Decision

**業務時間 = handler 取樣到的 `StudioNow.now`,以 bind 參數帶進 SQL;稽核時間可以繼續用 `NOW()`。**

- **判準**:只要有任何讀取會把這個欄位拿去分桶、或拿去跟取樣時刻比較,它就是業務時間,寫入與比較
  都必須用綁進來的 `now`(`$n`),不得寫 `NOW()`。其餘只記錄「這列何時被動過」的欄位(`updated_at`、
  不被任何讀取分桶或比較的 `created_at`、排序用的 `order_items.created_at`)是稽核時間,維持 `NOW()`。
- **這輪一律當稽核時間**(分不清楚、且目前沒有讀取依賴它的取樣時刻):attendance `marked_at`、leave
  `decided_at`、`clock_records`、messages、waitlist `created_at`。日後若有讀取開始拿它們分桶或比較,
  依上面的判準改綁,並更新本 ADR。
- **呼叫慣例**:需要 tz 的 service 收 `at: StudioNow`,只需要瞬間的收 `now: DateTime<Utc>`(CONTEXT
  「時鐘 seam」詞條);repository 一律收 `now: DateTime<Utc>` 並 `.bind(now)`。seed 的唯一取樣點是
  `bin/seed/main.rs` 的 `at`,呼叫同一批 repository 時傳 `at.now`。
- **步驟**(每步:紅燈測試 → 改綁 → 從 `clock.rs` 未覆蓋清單刪掉該項):
  - W7-1 `orders` 的 `paid_at`、`created_at`;
  - W7-2 `enrolments` 的 `enrolled_at`(連同被報表分桶的 `created_at`);
  - W7-3 `contact_inquiries.created_at`;
  - W7-4 優惠券有效性判斷改成 `expires_at > $2`(呼叫端:`GET /coupons/{code}/validate`、結帳、seed);
  - W7-5 `point_ledger.created_at`(`apply_delta_tx`／`try_spend_tx`／`reverse_order_tx`／`adjust_points`／`rewards::redeem`／`update_order_status` 一併改收 `now`);
  - W7-6 `users.created_at`(`create_account`／`create_user_tx`／`create_or_update_google_user_tx` 改收 `now`;`register`／`google_auth` 一併改收);
  - W7-7 `subscription_derived_status` 讀時狀態函式內的 `now()`:**延後**,等到有測試需要在固定時鐘下
    判斷訂閱狀態時再做。

## 落選方案

- **全部改綁,連稽核欄位一起**:每個 `updated_at` 都要多一個參數,呼叫鏈全線加寬,換來的只是稽核時戳
  與 handler 取樣值一致——沒有任何讀取依賴這件事。
- **在資料庫層注入時鐘**(例如每個連線 `SET` 一個自訂 GUC,SQL 改讀它):`NOW()` 本身不能被覆寫,
  改讀 GUC 一樣要逐一改 SQL,還多了「每條連線都要先設定」這個容易漏掉的協定,且跨 pool/tx 難保證。
- **維持現狀、只在文件記錄**:業務規則仍無法以固定時鐘測試,seed 不變量仍靠 bypass 維持。

## Consequences

- 業務時間欄位的寫入時刻與 handler 取樣值完全一致;以 `MockClock` / `studio_now_utc(t)` 固定時鐘的測試
  可以直接斷言 `paid_at == t`、`enrolled_at == t` 等。
- 同一列的業務時間與稽核時間可以不一致(固定時鐘測試裡 `created_at` 是 2020、`updated_at` 是當下)。
  這是刻意的:稽核欄位回答「資料庫何時動過這列」,不參與任何業務判斷。
- 新增寫入業務時間欄位、或新增拿時間欄位分桶/比較的讀取時,必須依判準決定綁 `now` 還是用 `NOW()`;
  把既有稽核欄位拿去分桶,等於把它升格成業務時間,寫入端要一併改綁。
- `src/utils/clock.rs` 的未覆蓋清單隨每一步縮短;清單剩下的是尚未處理的業務時間站點與本 ADR 刻意留給
  `NOW()` 的稽核時間。

## Addendum（2026-10-09）：歷史列綁自己的業務時間

兩點,原決策敘述維持當時快照、不改寫:

- **seed 寫歷史列時,傳那一列自己的時間,不傳 `at.now`。** 上面「呼叫慣例」說 seed 呼叫 repository 傳
  `at.now`,那只適用於「現在發生」的列;歷史訂單的 `paid_at` 是過去的某一刻,其 order_items、結帳點數帳也
  必須蓋同一刻。蓋 `at.now` 會讓歷史列的點數帳落進當月,灌水 `GET /points/me` 的 `earned_this_month`。
- **`order_items.created_at` 由「稽核時間」改成綁付款時間。** 決策的判準段把它列為「排序用」的稽核欄位、
  維持 `NOW()`;自此 `create_order_items` 收 `created_at` 並綁定,結帳傳取樣的 `now`,與 `paid_at` 同一刻。
  它仍然只用在同一張單內的排序(`ORDER BY oi.created_at`),同一張單的行時間本來就相同,所以行為不變。

## Addendum（2026-10-10）：訂閱的起始時間綁授權的 `now`

- **`subscriptions.started_at`/`created_at` 由 `NOW()` 改成綁 `grant_from_purchase_tx` 收到的 `now`。** `started_at`
  是業務時間:它是訂閱的起點,和由同一個 `now` 算出的 `expires_at` 成對,也經 API 回給會員;`created_at` 是
  `GET /subscriptions/me` 的排序鍵。結帳傳取樣的 `now`,行為不變;seed 的已付訂單改走 checkout 的授權函式後傳
  訂單的 `paid_at`,歷史訂閱才不會「今天才開始、卻早已到期」。`updated_at` 仍是稽核時間,維持 `NOW()`。

## Addendum（2026-10-11）：稽核欄位清單改由源碼掃描持有

- **「哪些欄位可以寫 DB 時鐘」改由 `tests/business_time.rs` 的 `AUDIT_COLUMNS` 持有**,取代 Consequences 所說、
  隨每一步縮短的 `src/utils/clock.rs` 未覆蓋清單。判準不變,仍是判斷;清單是判斷的結果,每筆附理由。
  `*.updated_at` 是一筆通配:`set_updated_at` trigger 在 UPDATE 時一律覆寫成 `NOW()`,綁值也沒用(`settings` 除外——
  它沒有該 trigger,upsert 自己寫 `now()`;INSERT 也不經 trigger)。
- **掃描範圍**:`src/**` 與 `tests/common/**` 的每個字串 literal(raw string、`format!`、`QueryBuilder` 片段都算;
  attribute 與 doc comment 略過),解跳脫後找 DB 時鐘字樣(`now()`、`current_timestamp`、`current_date`、
  `localtimestamp`、`clock_timestamp()`、`statement_timestamp()`、`transaction_timestamp()`),以括號深度對應到
  `table.column`:`INSERT` 欄位清單對 `VALUES` 位置(含 CTE 內的 `INSERT`),或 `UPDATE … SET col =`、
  `ON CONFLICT … DO UPDATE SET col =`。runtime、seed、測試 fixture 一視同仁;寫進清單外的欄位,測試失敗並附
  `file:line`。
- **`UNMAPPED` 的用途**:對應不到欄位的站點——`WHERE` 裡比較 `NOW()`(refresh token 清理、outbox 認領)、
  `INSERT … SELECT` 的來源(`UNNEST` 批次寫入、seed 出勤)——逐筆登記,以「檔案 + SQL 片段」辨識、不靠行號,
  各附理由與它涵蓋的命中數(在已豁免的語句裡再加一個 DB 時鐘,命中數對不上即失敗,不會搭豁免的便車)。新出現
  對應不到的站點沒登記、或任一清單有已不存在的項目,測試都失敗。
- **升級一個欄位 = 從 `AUDIT_COLUMNS` 刪掉它。** 新的讀取開始拿某個稽核欄位分桶或比較時,刪掉那一筆,掃描就列出
  所有仍寫 DB 時鐘的寫入端,逐一改綁 `now`;不必再等 seed 資料看起來不對才發現。
- **掃描不涵蓋**:
  - migrations 的 `DEFAULT now()` 與 trigger;
  - W7-7:`subscription_derived_status` 內的 `now()` 在 migration 裡,仍列在 `clock.rs` 模組文件;
  - `INSERT` 省略時間欄位、由 `DEFAULT` 填值的情況(例如 runtime 的 `notifications`);
  - 綁了值但綁錯時刻的情況(例如歷史列傳 `at.now`,見 2026-10-09 Addendum)——仍靠逐欄釘時鐘的測試與 seed 不變量。

## Addendum（2026-10-15）：測試場景時刻

- **測試 harness 守同一個判準:業務時間由呼叫端給。** `tests/common` 的「現在」只從場景時刻來(GLOSSARY「場景時刻」):
  `scene_now()` = `studio_now_utc(Utc::now().trunc_subsecs(6))`;`TestApp` 開場把 `MockClock` 釘在 `scene_now().now`,
  `MockClock` 一律釘住、不讀系統時鐘。
- **會寫業務時間的 fixture 收呼叫端的時刻**:`seed_user_with_roles_at(.., now)`、`seed_leave_scene(.., at: StudioNow)`、
  `seed_marked_attendance(.., now)`、`seed_full_course(.., now)`、`OrderSeed::created_at(t)`(沒有 `paid_at` 時必給,
  漏給在測試執行時 panic,不是編譯期)。`TestApp` 測試傳 `app.studio_now()`;service 測試傳手上已有的 `now`,沒有就傳
  `common::scene_now().now`。
- **由 `tests/business_time.rs` 的 `harness_reads_the_wall_clock_only_at_listed_sites` 持有白名單**:`tests/common/**` 出現
  `Utc::now`、`Local::now`、`SystemTime::now`,只准在 `WALL_CLOCK_SITES` 列出的函式裡,以「檔案 + 所在 fn」辨識、不靠行號,
  各附理由與命中數(同一個 fn 多讀一次也失敗)。現列三處:`mod.rs` 的 `scene_now`(場景時刻本身)與 `seed_member`
  (「剛加入」預設)、`google.rs` 的 `sign_id_token_with`(id_token 的 `iat`/`exp`,app 驗 `exp` 用的是真實時鐘)。
  掃描以 token 為單位,註解、doc comment、字串 literal 都不算;`Instant::now`(TTL、輪詢期限)量的是經過時間、不是業務
  時間,不掃。
- **刻意不做**:測試本體的 `Utc::now()`(約 463 處)、`studio_now_utc(Utc::now())`(約 131 處)、`seed_member` 的呼叫點
  (`users.created_at` 只影響新會員數與列表順序)。規則只管 harness,不管測試本體。
- **釘住的代價**:同一測試裡連續請求寫出的業務時間相同,要靠時間分出先後的斷言在請求之間 `app.clock.advance(…)`。
