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
  - W7-5 `point_ledger.created_at`(本輪後續步驟);
  - W7-6 `users.created_at`(本輪後續步驟);
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
