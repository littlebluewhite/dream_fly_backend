# ADR-0004: Admin 報表聚合口徑與場租計收模型

## Context

Round 4 Phase 4（Task P4-B3 seed、P4-B4a 金流、P4-B4b 人流）把 admin/mobile-admin 桌面「報表」頁大部分還是
前端 mock（`admin/api.ts`+`data.ts` 的 `ReportsData`）的欄位，逐一換成 `GET /reports/admin` 的真實聚合查
詢。這批欄位涉及金額、比率、分桶、時間邊界等一連串「同一個詞可以有好幾種合理算法」的選擇——例如「營收」算
折扣前還是折扣後、「出席率」請假算不算、教練的「營收」該不該把票券/場租也算給他、星期幾的第一天是週日還是
週一。每一個都不影響「能不能動」，但會直接改變數字，前端卡片、未來任何人重看這段程式碼或直接查 DB 的維運
腳本，都需要一份寫死在文件裡的單一口徑，而不是各自猜一個「看起來合理」的算法。

同時，這批報表第一次需要把「場租」（`time_slots`/`bookings` 這組表）也算進營收——但這兩張表在 Round 4 之
前完全沒有金額欄位，`bookings` 也沒有任何「取消退款」的資料模型。要讓場租能出現在 `revenue_breakdown`/
`income_sources_12m` 裡，得先決定這筆錢從哪裡來、算在哪個月、取消了算不算。

最後，前端既有 mock `ReportsData` 裡還有一個 `campusRevenue`（分校營收）欄位，本任務盤點資料源時發現整個
schema（`venues`/`courses`）從來沒有「分校」這個維度，需要明確決定要不要為了湊這個欄位新增一個分校概念，
還是承認它是原型階段的遺留假設、直接放棄。

以及一個架構層面的問題：這批新資料要開新端點承載，還是擠進既有的三個報表端點？擠進去的話，實作要不要一開
始就做平行查詢／快取，還是先求對、簡單、可讀，效能問題留給有真實資料量壓力時再解？

## Decision

**採用「擴充既有端點 + 循序查詢 + 明確口徑」**：不開新端點，`GET /reports/admin` 內每個新 section 都用獨
立的循序 SQL 查詢組成，口徑逐一寫死如下（詳細算法見 `reports::dto`/`reports::repository` 的 doc comment，
本 ADR 只記錄「為什麼選這個」，不重複貼 SQL）。

### 1. 報表口徑

- **毛額 vs 實收**：`revenue_breakdown`/`income_sources_12m`/`category_split` 一律是**折扣前毛額**（
  `order_items` 的 `unit_price_cents × quantity` 行小計加總，order 層的 `discount_cents` 不往下攤分到各
  行）；既有的 `revenue` section（`this_month_cents`/`last_month_cents`/`trend`）維持原本「實收」口徑
  （`orders.total_cents`，已扣折扣的訂單總額）。兩者刻意並存、刻意不同口徑——「這個月商品線各賣了多少」跟
  「這個月實際收了多少錢」是兩個不同的營運問題，分開呈現比硬湊成同一個數字更誠實。前端串接時不能假設
  這兩組數字加總會對得起來。
- **出席率排除請假**：所有 `present/(present+absent)` 計算（`kpis.attendance_rate`、`coaches[].
  attendance_rate`、`attendance_distribution`、member/coach 報表的 `attendance_rate*`）一律**`leave` 不
  計入分子也不計入分母**——請假是「這堂課本來就不算你來不來」，不是「該來沒來」，混進分母會系統性拉低每個
  人的出席率。無出勤資料時回 `null`（`service::safe_ratio`），不是 `0`：兩者語意不同，`0` 暗示「有資料、
  且是 0%」，`null` 才是誠實的「沒有可供計算的資料」。
- **retention 是月出席 cohort，不是續訂 cohort**：`retention` 量的是「這個會員這個月有沒有實際來上課」
  （`attendance_records` 有 ≥1 筆 `present`），不是「訂閱/月票有沒有續買」——後者是 `subscriptions` 模組
  的概念（見 ADR-0003），兩者故意分開，混用會把「有來上課但沒續買方案」跟「有續買方案但沒來上課」這兩種
  完全不同的營運訊號混成一團。`new_count`/`returning_count` 依「這是否為該會員首次出現活躍月」判定，掃描
  全期歷史（不只 6 個月窗），避免第 1 個月的窗內看似「新」其實是老會員剛好那幾個月沒來。
- **tier points 分桶固定 4 桶**：`regular`(<500) / `bronze`(500–1999) / `silver`(2000–4999) /
  `gold`(≥5000)，讀 `users.points_balance` 的**即時值**，不是歷史最高點數或某個時間點快照——分桶邊界寫死
  在 `repository::tier_distribution` 的 SQL 裡，不是可設定值（跟前端顯示用的中文標籤一樣，都不透過
  `settings` 表管理，改邊界要改程式碼，這是刻意的：分桶邊界屬於產品決策，不該讓 admin 在設定頁誤觸）。
- **weekday 0 = 週日**：`weekday_load` 的 `weekday` 欄位沿用 §3.18 既有裁決的慣例（`0=週日..6=週六`，
  PostgreSQL `EXTRACT(DOW FROM ...)` 的原生編號），不是 ISO 8601 的 `1=週一..7=週日`——跟這份契約其他所有
  「星期」欄位保持同一套編號，前端不需要為了這一個端點另外轉換。
- **場租計收歸屬使用日**：`revenue_breakdown`/`income_sources_12m` 的 `venue_rental` source 只計
  `confirmed`/`completed` 狀態的 bookings，金額取 `bookings.price_cents` 快照，**歸屬「時段使用日」**
  （`time_slots` 對應時段的日期），不是「下訂日」（`bookings.created_at`）——詳見下方第 2 點。
- **coach 營收歸因僅 course 類 line**：`coaches[].revenue_cents_12m` 只把 **course 類 order line** 的
  毛額歸到 `courses.coach_id`；票券/裝備/場租一律不歸因給任何教練，即使該教練當天代訂了場地、即使某張月
  票剛好是這位教練賣出的。教練的「業績」只反映他開的課有多少人報名付錢，其他收入來源沒有一個「這筆錢算誰
  的」的自然歸屬規則，勉強分攤只會製造一個看似精確、實則武斷的數字。
- **payment_method 為 NULL 時輸出字串 `"unknown"`**：`payment_split` 遇到 `orders.payment_method IS NULL`
  （Round 4 之前的訂單、或未來任何允許此欄位為空的路徑）時，鍵名原樣輸出 `"unknown"` 而非略過該筆或回
  500——前端據此顯示「其他」，讓總筆數對得起來（所有 `payment_split` 條目的 `count` 加總必須等於當月
  `REVENUE_STATUSES` 訂單總數，不能因為擋掉 NULL 而少算）。

### 2. 場租金額模型

`time_slots.price_cents`/`bookings.price_cents`（migration `20260708000006_venue_rental_pricing.sql`，
Task P4-B2）採**建立時快照**：一筆 booking 誕生的當下，把當時 `time_slots.price_cents` 的值複製一份存進
`bookings.price_cents`；之後管理者調整該時段的定價，**不回溯**修改已存在的 booking——使用者訂的時候看到
多少錢、將來報表算的就是那個數字，不會因為後來調價而讓歷史帳「無中生有」地改變。

`bookings` 沒有任何「退款」欄位或沖銷分錄。取消一筆 booking（`repository::cancel_if_active_tx`）只是把
`status` 改成 `cancelled`，`price_cents` 維持原值、不清零、也不新增一筆負數調整列——因為報表聚合是用
`status ∈ (confirmed, completed)` 過濾要不要計入這筆錢（`VENUE_REVENUE_STATUSES`），不是看
`price_cents` 是否為零/NULL 來判斷，所以取消當下沒有必要、也不應該去動這個欄位；動了反而會讓「這筆錢在
被取消前是多少」這個歷史事實消失。換句話說：**取消不寫沖銷分錄，靠狀態過濾自然把它排除在計收之外**，這是
比「新增一筆退款分錄」更簡單、且此刻業務需求（場租沒有實際金流退款流程要對帳）也不需要更複雜模型的選擇。

## Consequences

- **好處**：口徑集中寫在 `reports::dto`/`reports::repository` 的 doc comment 上，任何人要改算法都有明確
  的單一事實來源可看，不必反推 SQL 猜原意；前端一次 `GET /reports/admin` 就拿到所有 dashboard 需要的數
  據，不用自己組裝、也不用擔心多個 fetch 之間的資料不一致（同一次 request、同一個交易外的多筆循序查詢，
  彼此讀到的是同一個大致時間點的資料庫狀態）。
- **代價（效能）**：`admin_report`（`reports::service`）現在對 13 個獨立 repository 查詢逐一 `.await`，
  外加 `venue_usage` 之前的 idempotent session 物化——全部**循序執行**，沒有用 `try_join!` 平行、也沒有
  對整份回應加 Redis cache。這是刻意的簡化：目前 dev seed 12 個月的資料量下，這支端點的延遲可以接受，先
  求口徑正確、程式碼可讀（每個 section 一個獨立函式、獨立測試），不提前為了效能做微優化。**升級路徑**（
  記錄在案、非本輪實作）：查詢彼此獨立、沒有相依關係，未來若延遲變成真實問題，可以把這 13 支查詢改成
  `try_join!` 平行送出；若聚合的計算成本本身變高（而非查詢本身慢），可以比照 `user_roles:{id}` 的 Redis
  快取模式（TTL + 寫入路徑 explicit invalidate 或直接讓它在資料變動後自然過期）對整份 `AdminReportResponse`
  加一層快取。這兩條路都尚未動工，此 ADR 只記錄方向，不代表現在就該做。
- **代價（口徑一致性責任）**：`safe_ratio`「分母為 0 → `null`」的規則貫穿 `fill_rate`/所有出席率/
  `retention.rate`/`category_split.ratio`；未來新增任何比率欄位都應該延續這個慣例，不要在同一份 API 回
  應裡混用「無資料 → 0」跟「無資料 → null」兩種表達，否則前端無法用同一套邏輯處理所有比率欄位。
- **代價（場租收入與現金流脫鉤）**：場租收入用「時段使用日」而非「下訂日」入帳，代表提前訂未來時段的收
  入會延後反映在報表——例如 1 月訂了 3 月的場地，這筆錢在報表上算在 3 月，即使錢是 1 月收的。這是刻意接
  受的取捨：報表要回答的是「這個月場地實際被用了多少」這個營運問題，不是單純的現金收付表；如果未來需要
  現金流視角，需要另開欄位，不能直接拿 `revenue_breakdown.venue_rental` 當現金流用。
- **campusRevenue 不做**：現有 `venues`/`courses` schema 沒有分校/campus 維度（本系統目前只有單一場館的
  多個「場地」，`venues` 表存的是場地不是分校），`campusRevenue` 判定為前端 mock 原型階段的遺留假設欄
  位，本輪不新增假的分校維度去湊這個欄位，於契約文件（`docs/api/integration-contract.md` §3.24「mock
  有但契約無」清單）明列維持既有 mock。若未來真的要支援多分校，需要在 `venues`（或更上層）新增
  campus/branch 概念，不是能在現有結構上逆向拼湊出來的，不在本 ADR 範圍內。
