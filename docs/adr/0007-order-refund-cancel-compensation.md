# ADR-0007: 訂單退款/取消補償——單一入口、痕跡導向、User-First 鎖序

## Context

結帳（`orders::service::checkout`）在一個交易內產生一叢耦合的副作用：依購物車行項扣減商品庫存
（`products::repository::try_decrement_stock_tx`）、寫入點數 ledger（`checkout_earn`/
`checkout_redeem`）、建立報名（`enrolments`）與訂閱（`subscriptions`）。訂單狀態機
（`OrderStatus::can_transition_to`）在本輪之前就已經允許 `paid|processing|completed →
cancelled|refunded` 這幾條邊，但 `PATCH /orders/{id}/status` 走到這幾條邊時只翻轉 `status`
欄位——結帳當下建立的庫存扣減、點數異動、報名、訂閱全部原封不動留著。換言之，「退款/取消」這個
詞在 API 層面早就存在，但它的補償語意（該撤銷什麼、撤銷多少、怎麼撤銷才不會跟結帳本身互相踩
踏）一直是空白。

本輪補這塊空白：設計先由 Plan 代理對照 repo 現況驗證，再經 codex review 兩輪硬化（codex 抓到：
user 鎖僅無條件化不夠、必須前移到購物車讀取之前；庫存回補不能用退款當下的現況推測、須結帳當下
的快照；補償幅度要讀 ledger 實錄而非訂單彙總欄；補點端點需要防重複套用的機制）。收斂下來有十個
邊界決策需要留下權威記錄——多數決策背後都有一個「看起來更直覺但實際上有陷阱」的替代方案被否
決，這份 ADR 記錄的正是「為什麼不是那個更直覺的做法」。

## Decision

### 1. 補償的唯一入口是 `update_order_status`，Cancelled 與 Refunded 是同一語意的兩個終態標籤

不新增獨立的 public `refund_order` 進入點——補償邏輯（私有函式 `compensate_order_artifacts_tx`）
是 `orders::service::update_order_status` 內部的一個步驟，只在 `refund::compensation_required`
判斷「需要補償」時才被呼叫。`target = Cancelled` 與 `target = Refunded` 觸發完全相同的補償路徑
（同一個謂詞、同一個函式、同一組 ledger reason）——補償引擎不區分「取消」與「退款」，兩者只是同
一件事（「錢/資源要吐回去」）的兩種終態命名。

否決的替代方案：獨立 `refund_order(db, order_id)` 進入點。代價是要重新實作 `update_order_status`
已經有的五件事——parse 目標狀態、鎖序（order row lock 開啟後續 user/products 鎖）、
`can_transition_to` 合法性檢查、outbox 事件插入、通知——兩份程式碼各自維護一份，遲早漂移。

後果：目前沒有任何欄位或訊號區分「這筆補償是因為 admin 選了 cancelled 還是 refunded」——
`point_ledger` 的 `refund_restore`/`refund_clawback` reason、outbox 的
`OrderStatusChangedPayload` 都只帶最終 `status` 字串本身。若未來需要「取消」與「退款」有不同的
補償內容（例如退款要扣手續費、取消不用），需要在 `compensate_order_artifacts_tx` 或其呼叫處新
增分支，現狀是兩者刻意不可區分。

### 2. `compensation_required` 消費 `REVENUE_STATUSES`，刻意耦合、附帶漂移風險

`OrderStatus::is_revenue()` 直接讀 `REVENUE_STATUSES.contains(&self.as_str())`，不另建一份
match；`refund::compensation_required(current, target) = current.is_revenue() &&
matches!(target, Cancelled | Refunded)`。選擇耦合到既有常數（而非獨立定義「哪些狀態算已成
交」），理由是這條規則本來就該只有一個 owner：`REVENUE_STATUSES` 已經是
`products::repository::find_sold_counts`（售出計數）與報表營收聚合的單一事實來源（CONTEXT.md
「營收狀態集」詞條），退款補償問的正是同一個問題的鏡像——「這筆訂單現在算不算已經計入營
收」。兩份獨立 match 遲早會在新增狀態時漂移（一份加了、另一份忘記加），用同一個常數消滅這個風
險類別。

**耦合本身帶進來的漂移風險**：`REVENUE_STATUSES` 是執行期 `[&str; 3]` 陣列，不是對
`OrderStatus` 變體的窮盡 match——編譯器不會在新增 `OrderStatus` 變體時強迫檢查它有沒有被放進這
個陣列。若未來新增一個「已成交」性質的狀態（例如某種介於 paid 與 processing 之間的中介態）卻忘
記加進 `REVENUE_STATUSES`，`is_revenue()` 會靜默回傳 `false`——若同時該狀態有一條合法轉往
`Cancelled`/`Refunded` 的邊，`compensation_required` 也會靜默回傳 `false`，`update_order_status`
就只翻轉 `status` 欄位，庫存/點數/報名/訂閱全部原封不動留著，不會有任何錯誤或警告。這個風險本
ADR 不消除（消除需要把 `REVENUE_STATUSES` 改成對 `OrderStatus` 窮盡 match 的型別，是比本輪更大
的改動），只記錄：未來新增 `OrderStatus` 變體時，`REVENUE_STATUSES` 與 `can_transition_to` 的新
邊都要一併檢查是否該同步。

### 3. 整單退款/取消，不看核銷/使用進度

`compensate_order_artifacts_tx` 撤銷的是整筆訂單結帳當下的全部點數流（讀 ledger
`checkout_earn`/`checkout_redeem` 加總，`points::repository::find_order_flow_sums_tx`）與全部
報名/訂閱（`enrolments`/`subscriptions` 各自的 `cancel_by_order_tx`，依 `order_id` 整批
UPDATE），不論這筆訂單此刻已經被核銷/使用了多少——
`cancel_by_order_tx` 的 WHERE 子句只看 `order_id` 與現有 `status`，從不讀
`subscriptions.remaining_sessions` 或出席紀錄；`plan_refund` 算的點數幅度來自 ledger 實錄，不
會因為會員後來透過 `POST /subscriptions/{id}/redeem` 核銷過幾堂課而打折。測試
`refund_after_member_self_cancel_still_succeeds` 是這個決策的具體見證：會員自助取消一筆報名之
後，admin 才整單退款，點數仍然 100% 反轉回原始餘額。

這是「整單」語意的延伸，不是遺漏：這個系統本來就沒有「訂單部分退款」的模型（狀態機是整筆
`orders.status` 翻轉，不是逐行），要做到「核銷過幾堂就少退幾堂」需要一個全新的計量單位（退款算
在 session 上而非訂單上），不在本輪範圍。後果：admin 對一個幾乎已經用完的方案整單退款，買家仍
拿回全額點數——這是接受的行為，不是 bug；若未來要支援按使用比例退款，需要重開設計輪。

### 4. 點數反轉順序 RESTORE 先、CLAWBACK 後；餘額不足 409、不 clamp

`compensate_order_artifacts_tx` 先呼叫 `points_service::apply_delta_tx` 套用
`plan.restore_points`（正數，沖回 `checkout_redeem` 扣掉的點數），再套用
`plan.clawback_points`（負數，沖回 `checkout_earn` 賺到的點數）。這個順序不是隨意選的：
`users_points_balance_check` 這條 CHECK 在同一交易內逐語句評估，先加後扣讓「扣款那一步」的成功
門檻從嚴格反序（`balance ≥ earned`）放寬成 `balance + restored ≥ earned`——買家結帳當下賺的點數
如果後來被花掉一部分，先把折抵的點數還回去，能覆蓋的 clawback 案例更多，是刻意偏離嚴格反序的
選擇。

餘額仍然不足時（`apply_delta_tx` 撞上 `users_points_balance_check` 違規）→
`AppError::Conflict("點數不足")` → `?` 直接上拋 → 整個 `update_order_status` 交易回滾，狀態翻
轉、庫存回補、報名/訂閱取消全部一起撤銷，不會有「補償做一半、狀態沒翻」的中間態（見測試
`refund_clawback_insufficient_balance_conflicts_and_rolls_back_all`）。刻意不做兩件「看起來能繞
過 409」的事：不 clamp clawback 幅度到餘額上限（ledger 要誠實反映「這筆訂單原始賺了多少」，
clamp 會讓沖銷金額與原始賺點金額對不上，之後無法從 ledger 重建真實歷史）、不放寬
`users_points_balance_check` 這條全系統 invariant（那條 CHECK 保護的是所有寫路徑，不是退款一
個人的問題）。這個 409 是決策 10 的補點端點存在的理由——admin 手動幫買家補點之後，再重試同一筆
退款。

### 5. 鎖序統一為 user-first；既有的 SHARE→UPDATE 死鎖風險記錄但不修

checkout 與退款現在共用同一條鎖序骨架的前半段：

- checkout：`users`（無條件、購物車讀取之前）→ `cart_items`/`products`/`courses`（SHARE，購物
  車讀取本身帶的鎖）→ `products`（UPDATE，`product_id` 升序）→ `courses`（升序）→ `enrolments`
  → `subscriptions`
- 退款：`orders`（`update_order_status` 的 `FOR UPDATE` 讀取）→ `users`（顯式無條件
  `lock_balance_tx`）→ `products`（UPDATE，升序）→ `enrolments` → `subscriptions`

兩個關鍵字是「無條件」與「前移」。**無條件**：即使 `use_points=false` 或這筆訂單結帳當下點數流
是 0，`lock_balance_tx` 仍然執行——否則「同一買家的 checkout 與 refund 全程互斥」這句宣稱在零
點數流的情境下不成立（退款端若只靠 `apply_delta_tx` 的隱式 UPDATE 鎖，零點數流時那個 UPDATE 根
本不會發生，users 列完全沒被鎖到）。**前移**：users 鎖必須排在購物車讀取*之前*，不能只是「反正
兩邊都會鎖 users，誰先誰後無所謂」——`cart_service::find_cart_items_for_checkout_tx` 本身在讀取
階段就對 products/courses 取 `FOR SHARE`；如果 users 鎖排在這之後，checkout 會先拿到
products-SHARE 再等 users，而退款先拿到 users 再等 products-UPDATE，兩條路徑互相等待對方已持
有的鎖——一個死鎖環。把 users 排到兩條路徑最前面，同一時刻只有一個買家的操作能往下走，環無法
形成。回歸測試：`concurrent_checkout_last_unit_only_succeeds_once`。

**既有風險，本輪不修**：兩個併發的 checkout 對同一商品從 SHARE 升級到 UPDATE 的死鎖拓撲，是本
輪之前就存在的既有狀態（PostgreSQL 的死鎖偵測器會擇一中止並回傳錯誤，不會真的卡死）——本輪的
鎖序前移解決的是「checkout 與 refund 之間」的死鎖環，不觸碰「兩個 checkout 之間」這個獨立的既
有風險，兩者是不同的鎖圖。

**併發安全論證（退款 vs 自助操作）**：`subscriptions::repository::redeem_one_session` 與
`enrolments::repository::cancel_if_active_tx` 都不參與上述鎖序——各自是單一原子
`UPDATE ... WHERE ... RETURNING`（前者的 WHERE 查
`subscription_derived_status(...) = 'active' AND remaining_sessions > 0`，後者查
`status <> 'cancelled'`），沒有「先鎖列、再檢查、再寫」的兩步式 TOCTOU 空窗。這讓它們與退款的
交錯天然安全，不需要被納入上面的鎖序骨架：任一方先取得該列的隱式 UPDATE 鎖，另一方阻塞等待時
不持有其他任何鎖（它就是一條單陳述句交易，等待時手上空無一物），不可能與退款持有的多鎖鏈形成
循環——它只會是單純的等待者。等到先手交易 commit，後手交易的 UPDATE 重新對已提交的新值求值
WHERE 子句，自然得到 0 rows，對應服務層映成衝突（`redeem_one_session` 回 `None` 後再查明確原
因、`cancel_if_active_tx` 回 `None` 映成 `Conflict("enrolment already cancelled")`）。三種交錯
結果都安全：退款先 commit，自助操作重新評估後 0 rows、409；自助操作先 commit，退款讀到已經是
cancelled 的報名列，`cancel_by_order_tx` 的 `status <> 'cancelled'` 讓那一列自然跳過（0 rows
affected，非錯誤）。測試 `redeem_after_refund_is_conflict` 覆蓋其中一種交錯（循序構造，非真正
併發壓力——見 Consequences）。

### 6. 優惠券不參與反轉；`paid_at` 保留

`Coupon` model（`coupons::model::Coupon`）沒有任何使用次數/核銷計數欄位（`id`/`code`/
`discount_cents`/`is_active`/`expires_at`/`created_at`）——優惠券本身不是「每張券消耗一次」的資
源，退款/取消沒有東西可以「還給」優惠券。訂單自己的 `discount_cents`/`coupon_code` 也不被補償
邏輯觸碰，維持結帳當下套用了什麼折扣的歷史紀錄。

`paid_at` 同樣被保留，不清空：`orders::repository::update_status_and_paid_at_tx` 的 `CASE` 只
在轉入 `paid` 且原本是 `NULL` 時才蓋 `NOW()`，轉往 cancelled/refunded 走 `ELSE paid_at` 分支，
原始付款時間原封不動留著——退款後仍能查出「這筆單原本是什麼時候付的錢」，是審計用途的歷史紀
錄；因為報表營收聚合本來就已經用 `REVENUE_STATUSES` 過濾掉 cancelled/refunded 狀態的訂單，保
留 `paid_at` 不會造成重複計入營收。

### 7. `uniq_point_ledger_refund_once` 是點數流有落地訂單的後盾，不是普遍補償標記

`uniq_point_ledger_refund_once`（migration `20260717000003_point_ledger_refund_once.sql`，
partial unique index on `point_ledger(order_id, reason) WHERE reason IN ('refund_restore',
'refund_clawback')`）把「每張訂單每個退款方向至多一列反轉」下沉為 DB 層 invariant，是防止重複
補償的第二道防線。

**侷限**：這個 index 只保護「結帳當下真的寫過 `checkout_earn`/`checkout_redeem` ledger 列」的
訂單。全額優惠券折抵、`use_points=false`、或純商品且未涉及點數的訂單，結帳當下就不會寫任何點
數 ledger 列；退款時 `find_order_flow_sums_tx` 讀回 `(0, 0)`，`plan_refund` 算出的幅度是 0，
`apply_delta_tx` 對零 delta 直接拒絕（見其 doc comment），整個點數反轉步驟被跳過——沒有 ledger
insert 嘗試，這個 unique index 完全沒有機會介入。它是「本來就有點數流的單」的後盾，不能當作
「這張單是否已經補償過」的通用判準。

**主防線始終是狀態機**：`OrderStatus::can_transition_to` 的終態拓撲（`Cancelled`/`Refunded`
沒有任何出邊，除了同狀態自環）加上 `compensation_required` 的 `is_revenue()` 閘門——只要 API
呼叫都經過 `update_order_status`，同一筆訂單不可能被補償兩次，不論它有沒有點數流。這也是為什
麼場景測試矩陣裡沒有一條測試直接命中這個 unique index 的 violation 路徑：`FOR UPDATE` 序列化
+ 狀態機終態結構已經讓重複補償在正常 API 路徑下不可達，這個 index 是 belt-and-suspenders，防
的是繞過 `update_order_status`、直接對 `orders.status` 做 out-of-band SQL 寫入的殘餘風險——那
種寫入方式仍然可能讓同一張訂單被補償兩次，這個 index 至少讓第二次的點數列寫入失敗，而不是靜默
疊加。

### 8. 補償只讀「結帳痕跡」，不用現況推測；遺留資料天然免疫

`compensate_order_artifacts_tx` 讀的一律是結帳當下落地的痕跡，不是重新用現況資料推算：
`order_items.stock_decremented`（每一行商品在結帳當下是否真的扣過庫存的快照，migration
`20260717000004_order_items_stock_decremented.sql`）、`point_ledger` 的 `checkout_earn`/
`checkout_redeem` 實錄（`find_order_flow_sums_tx`，不是 `orders.points_earned`/`points_used`
彙總欄）、以及 artifacts 表自身的 `order_id` 關聯。

拒絕「用現況推測」的具體理由：商品的庫存模式（`stock IS NULL` = 無限庫存）是可變的現況欄
位——admin 事後可以把一個結帳當下無限庫存的商品改成有限庫存，若退款時只看「現在 `stock` 是不
是 `NULL`」來判斷該不該回補，會對一個從未被預留過的庫存誤判成「當初有扣」，回補出一批憑空多出
來的庫存（見測試 `refund_skips_restock_when_sold_unlimited_then_stock_set`）。點數同理：直接抄
`orders.points_earned`/`points_used` 彙總欄，對 seed/歷史直接建構的訂單（從未真正跑過
`checkout` service、沒有對應的 ledger 列）會沖銷「從未發生過的點數流」。

**遺留資料政策的自然推論**：seed fixture 或任何繞過 `checkout` service 直接寫入
`orders`/`order_items` 的歷史資料，三種痕跡（`stock_decremented`、ledger 列、artifacts 的
`order_id` 關聯）全部缺席——`plan_refund` 對這種訂單自然算出全零的 `RefundPlan`，整個補償流程
no-op（只翻轉 `status`），不需要為「這是不是一張 legacy 單」寫任何特判邏輯。測試
`refund_of_directly_built_paid_order_is_pure_status_flip` 是這個推論的直接見證。

### 9. Same-status PATCH 早退，讓「retry 冪等」成為可觀測事實

`update_order_status` 在讀到 `current.status == target` 時直接回傳既有訂單（200），不執行
UPDATE、不插入 outbox、不發通知、不嘗試補償——在此之前，同狀態的 PATCH 仍然會重新 UPDATE + 重
新排 outbox + 重新通知，`OrderStatus::can_transition_to` 那句「Idempotent no-op」的程式碼註解
只是理論上「不會被拒絕（400）」，不是真正對外可觀測的冪等（重複呼叫會產生重複的通知與 outbox
列）。早退之後兩者一致：重試同一個狀態不但不報錯，也不會產生任何可觀測的副作用。防止重複補償
本身不靠這道早退——`compensation_required` 對任何同狀態對恆為 false（決策 2 的
`is_revenue()` 閘；見 refund.rs 的謂詞單元測試）——早退真正貢獻的是 UPDATE/outbox/通知這三件
副作用的可觀測冪等。

早退分支刻意先 `drop(tx)` 才呼叫 `assemble_response`——`assemble_response`（組裝 items +
enrolments/subscriptions）走的是 pool 查詢（`fetch_artifacts`），交易仍持有連線時發出 pool 查
詢，在低連線數的 pool 下會自己等自己（self-deadlock）；這個紀律與 `checkout` 空車重放路徑（見
其 idempotency 分支）的 `drop(tx)` 用法一致，同一顆連線池下的同一類風險。測試
`refunded_same_status_noop_does_not_compensate_twice` 斷言 ledger 仍恰好兩列、outbox 與通知都
沒有新增。

### 10. `POST /points/adjustments` 是 CAS，不是嚴格冪等

`points::service::adjust_points` 用 `expected_balance` 做樂觀鎖（compare-and-swap）：鎖列讀出
目前餘額，與呼叫方帶的 `expected_balance` 不符即 409，相符才真正套用 `delta`。這道閘門防的是
「同一筆調整被重複套用」，不是「同一個 request 重放會得到同一個結果」——這兩件事經常被混為一
談，但並不相同。

一個逾時重試的具體情境：呼叫方送出調整、伺服器成功套用但回應在網路上遺失、呼叫方逾時後帶著同
一個 body 重試——此時餘額已經因為第一次呼叫而改變，重試讀到的「目前餘額」不再等於它帶的
`expected_balance`，得到的是 409，**不是**原本那次成功的重放結果。也就是說重試在這裡的語意是
「被拒絕」而非「被安全地重放」——呼叫方（admin）看到 409 不能假設「一定是別人把餘額改了」，必
須重新查詢該使用者當下的 `points_balance`（`GET /users/{id}`，`GET /points/me` 僅回呼叫者本人
餘額查不到別人）才能判斷第一次呼叫究竟有沒有成功套用。

殘餘風險是 **ABA**：如果第三方（另一個 admin，或另一筆退款補償的 clawback）恰好在重試視窗內把
餘額改回精確等於 `expected_balance` 的值，這次重試會被誤判為「餘額仍是預期值」而通過，悄悄套
用了一次不該重放的調整。這個殘餘風險在目前狀態下被接受，理由是這個端點是 admin 手動觸發、低頻
（關閉退款補償 409 的修復迴路，不是高頻自動化路徑），且每一次套用都有 `AdminAdjust` 這筆
ledger 列可稽核——出問題時可以人工逐列核對，不是無跡可尋。**若未來這個端點被自動化呼叫端（非
人類操作）使用，屆時需要升級成 request-id 去重鍵或另一個 partial unique index**；現在不預先加
這層 schema，因為目前唯一呼叫端是人工低頻操作，提前加會是投機性複雜度。
（`points::service::adjust_points` 的 doc comment 是這條決策的原始出處，本節是它的完整版。）

## Consequences

- 十個決策點共同的方向是：**能力範圍收斂在「整單、痕跡導向、單一入口」這條線上**——不支援部分
  退款、不支援按核銷比例退款、不支援跳過 `update_order_status` 直接呼叫補償。任何超出這條線的
  未來需求（部分退款、按 session 計量的退款、取消與退款要有不同的補償內容）都需要重開設計輪，
  不是在現有函式上加參數就能長出來的。
- 兩個記錄在案但本輪不解決的殘餘風險需要未來留意：(1) 決策 2 的 `REVENUE_STATUSES` 手動同步義
  務——新增 `OrderStatus` 變體時必須人工檢查是否要加進這個常數與 `can_transition_to` 的邊，編
  譯器不會提醒；(2) 決策 5 的既有 SHARE→UPDATE 死鎖拓撲（兩個併發 checkout 對同一商品）未被本
  輪觸碰，仍然依賴 PostgreSQL 死鎖偵測器擇一中止。
- 決策 10 的 CAS 侷限是本輪唯一明確標了「條件觸發式」升級路徑的項目——這個端點目前只有人工呼
  叫端，一旦出現自動化呼叫端（例如某個排程任務或另一個服務直接打這支 API），必須先補
  request-id 去重或 partial unique index，否則 ABA 殘餘風險的機率會從「可忽略」變成「值得認真
  考慮」。
- 決策 5 的併發安全論證（退款 vs `redeem_one_session`/`cancel_if_active_tx`）目前是程式碼閱讀
  + 這份 ADR 的論證，沒有專門的併發整合測試直接施加真實的競態壓力（測試
  `redeem_after_refund_is_conflict` 驗證的是循序交錯，不是真正並發）；若這條路徑未來要改動，
  建議先用雙連線手動驗證或補一個真正並發的整合測試再動工。
- `subscriptions.status` 轉為 `cancelled` 目前唯一的寫入路徑就是決策 3 的 `cancel_by_order_tx`
  ——本模組沒有任何使用者可觸發的「取消訂閱」端點（`routes.rs` 只有 `/subscriptions/me` 與
  `/subscriptions/{id}/redeem` 兩條路由）。這代表訂閱的 `cancelled` 狀態現在完全依賴訂單退款/
  取消才會出現，前端若要呈現「已取消的訂閱」，唯一成因就是所屬訂單被退款或取消。
