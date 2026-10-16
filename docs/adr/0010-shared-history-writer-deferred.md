# ADR-0010: seed/fixtures 共用「歷史寫入」module 延後

## Context

Task 6 修 seed 與測試 fixture 時,兩邊各自累積了「造出一筆已完成歷史」的程式碼:seed 為報表會員寫
`checkout_earn`/`refund_clawback` 結算餘額(`bin/seed`),測試 fixture 有 `fixtures::seed_coach` 走
`coaches::repository::insert_tx` + 角色指派(`tests/common`)。兩者形狀相近——都是「繞過真實 API 流程,直接
造出一筆看起來像是走過真實流程的歷史資料」——曾評估要不要抽一個共用的「歷史寫入」module 給 seed 與測試
fixture 共用。

Task 6 明確**沒有**做這件事:目前只有 seed 這一個真正的消費者在寫「歷史訂單」這類資料,測試 fixture 端
(`seed_coach`)寫的是「當下狀態」(一個 coach 帳號),不是「歷史」,兩者不是同一形狀的重複。抽出來的共用
module 現階段只有一個真實使用者,是**假設的 seam**——為尚不存在的第二個消費者先建介面,是投機性抽象。

## Decision

**延後**,不建共用的「歷史寫入」module。seed 與測試 fixture 各自維持現狀寫法。

理由:

- **只有一個真實消費者**。目前需要造「歷史訂單」的只有 seed 的報表資料集;測試 fixture 沒有對等需求。一
  個 adapter = 假設的 seam,在真正出現第二個消費者之前抽象化,買到的是猜測的靈活性,不是已驗證的重複。
- **會改動 seed 報名資料與報表數字**。seed 目前的寫法(`checkout_earn`/`refund_clawback` 結算 + 12 個月
  報表資料集)已經是 Task 6 剛穩定下來的行為;抽共用 module 勢必牽動 seed 產生的報名資料形狀與報表數字,
  這輪(docs-only)不做這個風險。

## Consequences

- seed(`bin/seed`)與測試 fixture(`tests/common::fixtures`)的歷史/狀態造資料邏輯繼續各自維護,不共用。
- **重開條件**(明文,符合任一才重開設計輪):
  1. 出現第二個需要造「歷史訂單」的測試族群——即某個測試模組也需要像 seed 一樣造出「已結帳、已結算」的
     歷史訂單資料,而不只是像 `seed_coach` 這樣造「當下狀態」。
  2. seed 需要走真實結帳(改成呼叫 `orders::service::checkout` 而非直接寫入歷史列),屆時「歷史寫入」這
     個 seam 的必要性由結帳路徑本身取代或重新定義。

## Addendum (2026-09-28)

R15 Phase 2 把 `bin/seed.rs` 改成目錄 bin(`bin/seed/main.rs` 入口 + `bin/seed/dataset.rs::run(db,
at: StudioNow) -> SeedReport`),只是路徑搬遷與型別化取樣時鐘,不是重開本 ADR:`run` 依舊直寫歷史列、不
經 `orders::service::checkout`。兩個重開條件都未成立——沒有出現第二個需要造「歷史訂單」的測試族群
(`tests/common::fixtures` 仍只造「當下狀態」),`dataset::run` 也沒有改走真實結帳。

## Addendum (2026-10-03)

前提「fixture 只建當前狀態」收窄,兩處:

- `OrderSeed` 已能寫回溯訂單(`paid_at` 可指定),但仍是直寫列、不產 points ledger——不是
  seed 那種「已結帳、已結算」的歷史,故重開條件 1 未成立;共用 history writer 仍遞延。
- 假單 fixture 不再只建當前狀態:`seed_leave_request` 的 Approved/Rejected 先插 pending,再走 production
  `leave::service::decide_tx`(`decide_leave_request` 也用它),所以 Approved 假單帶有與真實核准相同的出勤
  `leave` 投影。這是借用既有 production seam,不是新建共用 history writer;Pending 照舊直接 INSERT,
  Cancelled 也直接 INSERT(取消走 `cancel_if_pending_tx`,無投影可帶)。

## Addendum (2026-10-09)

部分重開:已付訂單的寫入改由 checkout 與 seed 共用 `orders::paid_order::record_paid_order_tx`(訂單列+訂單行+
結帳點數帳,同一個 `paid_at`)。兩個 adapter——`orders::service::checkout` 與 `bin/seed/dataset.rs` 的
`insert_order_if_absent`——所以這個 seam 是真的,不是假設的。起因是 seed 手抄的那份抄歪了:點數帳蓋執行當下
`at.now`,`paid_at` 卻是歷史時間,seed 會員 `GET /points/me` 的 `earned_this_month` 因此灌水。seed 的退款訂單
也改走 `points::service::reverse_order_tx`。上列兩個重開條件其實都沒成立;仍稱「部分重開」,是因為觸發的是第三個
理由——seed 手抄的已付訂單寫入漂移了(點數帳蓋 `at.now`,`paid_at` 是歷史時間)——而且共用寫入的是 checkout +
seed,不是本 ADR 原本討論的 seed + 測試 fixtures 這一對。

- `tests/common/fixtures.rs` 的 `OrderSeed` 不變:仍直寫列、不產 points ledger,重開條件 1 仍未成立。
- seed 沒有改走 `orders::service::checkout`(重開條件 2 未成立):待付/取消訂單照舊直寫訂單列,報名與訂閱授權
  也不經 checkout 編排(見 GLOSSARY「Seed 資料集」的已知缺口)。

## Addendum (2026-10-10)

seed 已付訂單的訂閱授權改走 checkout 的 `subscriptions::service::grant_from_purchase_tx`(起始時間 = `paid_at`,
退款訂單再以 `cancel_by_order_tx` 取消)——同上一則,是借用既有 production seam,不是新建共用 history writer。
上一則「報名與訂閱授權也不經 checkout 編排」只剩報名一半成立(見 GLOSSARY「Seed 資料集」的已知缺口)。

## Addendum (2026-10-11)

重開條件 2 已達成:seed 改當真實結帳的呼叫者。已付/完成/退款種子訂單都是一筆 `SeedPurchase`,經
`cart::service::add_item` → `orders::service::checkout`(`Idempotency-Key` 為 `DF-SEED-…`,`now` 為該筆的歷史
付款時間)下單,再經 `orders::service::update_order_status` 推到目標狀態。報名資料集也改為 39 筆課程購買:每筆
種子報名都是 checkout 為課程行建立的那筆,帶著自己的 `order_id`。定價、訂單行、點數帳、訂閱授權、報名與退款
補償全由 checkout 與訂單狀態機產生,seed 不再抄任何一步——上兩則增補裡 seed 對 `record_paid_order_tx`、
`grant_from_purchase_tx`、`reverse_order_tx` 的直呼隨之移除,`record_paid_order_tx` 回到只有 checkout 一個呼叫端。

- **仍保留的 bypass**(seed 直寫,不經 runtime 流程):使用者(`upsert_user`/`upsert_seed_member`)、目錄(課程、
  商品、優惠碼、獎勵、公告、場館)、時段(課程週課表時段、場租 `time_slots`)、出勤(`attendance_records` 批次
  寫入)、場次 backfill(`sessions::calendar::backfill_for_seed`)、場租預約(`bookings`)、洽詢
  (`contact_inquiries`)、待付款訂單(`insert_pending_order_if_absent`——checkout 只建已付訂單)。
- **副作用**:checkout 與狀態機照常寫 outbox 事件與站內通知,dev 資料庫因此多出 `events_outbox`/`notifications`
  列(DB 時鐘,稽核欄位,ADR-0017)。Kafka 預設關閉;若設 `APP__KAFKA__ENABLED=true`,下一次 `cargo run` 會把約 250 筆歷史種子 outbox 事件
  送上 Kafka,行程內 consumer 也會寫入對應的 `audit_log` 列。舊 seed 直寫的資料——種子會員沒有訂單的報名、付過款(非待付、非取消)的
  `DF-SEED-…` 訂單——沒有 idempotency key,會被再買一次,所以 `dataset::run` 在任何寫入前拒絕這種資料庫,要先重置 dev DB。
- **共用「歷史寫入」module 仍然延後**:重開條件 1 未達成——`tests/common/fixtures.rs` 的 `OrderSeed` 照舊直寫
  列、不產 points ledger,沒有第二個需要造「已結帳、已結算」歷史訂單的測試族群。條件 2 則由結帳路徑本身取代了
  這個 seam 的必要性,不必另建。

## Addendum (2026-10-15)

已付訂單落帳併回 checkout(R21):`orders::paid_order` 刪除,`record_paid_order_tx` 的三步——
`repository::create_order`、`create_order_items`、`PricingOutcome::ledger_deltas` 逐筆 `apply_delta_tx`——寫回
`orders::service::checkout` 本體,位置不變(`reserve_stock_tx` 之後、報名之前)。Addendum 2026-10-09 立這個 seam
的理由是兩個 adapter;seed 那一個在 2026-10-11 改走真實結帳後退場,只剩 checkout 一個呼叫端。之後它的介面(7 個
參數:交易、`BalanceLock`、訂單編號、付款方式、訂單行、定價結果、付款時間)跟本體一樣大,唯一的直接測試得重演
checkout 的內部編排(取鎖 → 讀購物車 → 上架檢查 → 定價 → 訂單行)才呼叫得到它;一次購買的時間也一半在 module
裡(訂單、訂單行、點數帳)、一半在 checkout 的區域變數裡(報名、訂閱)。

- checkout 直接擁有購買時刻(GLOSSARY「購買時刻」):訂單、訂單行、結帳點數帳、報名與訂閱都蓋同一個取樣 `now`,
  由 `checkout_stamps_every_purchase_artifact_with_the_sampled_now`(`tests/service_orders.rs`)在 checkout 介面
  一次釘住;原本三個 checkout 時刻測試與打在 `record_paid_order_tx` 上的測試併入它。
- 「只替已鎖的買家記帳」不再由 `record_paid_order_tx` 收 `&BalanceLock` 的型別擔保,改由 checkout 的呼叫順序擔保:
  `acquire_checkout_locks` 先鎖買家的 `users` 列,點數帳寫在其後、同一交易內。
- 若再出現第二個寫已付訂單列的 runtime 路徑(例如待付訂單的付款轉移),再評估抽回共用寫入;在那之前,在 checkout
  外寫已付訂單列是 GLOSSARY「購買時刻」的 _Avoid_。
- 本 ADR 的決策不變:共用「歷史寫入」module 仍延後,重開條件 1 仍未成立(`tests/common/fixtures.rs` 的
  `OrderSeed` 照舊直寫列、不產 points ledger)。

## Addendum (2026-10-15): 種子購買時間線

Addendum 2026-10-11 讓 seed 當真實結帳的呼叫者;買與推狀態的規則原本散在 `dataset::run` 的資料集公式之間,只能用
整份資料集測。R21 把這一步收成 `bin/seed/dataset/timeline.rs`(GLOSSARY「種子購買時間線」),介面只有
`buy_and_walk(db, at, Vec<SeedPurchase>) -> Vec<Walked>`:依 `ts` 穩定排序,每筆先以 `idempotency::replay` 預查
key、未命中才經購物車與 `checkout` 買下,買完立刻經 `update_order_status` 推到目標狀態,依購買順序回傳。

- **排程刪除**:原本完成/退款訂單要等購買進行到它第一步的時間才推(`walk_due` 加最小堆),好讓每位會員的
  `point_ledger.balance_after` 依 `created_at` 往前走。ADR-0018 把 `balance_after` 改成讀時推導後,明細不依賴
  寫入順序,排程失去理由。改成買完即推仍安全:退款的 `refund_clawback` 一定寫在自己的 `checkout_earn` 之後,seed
  結帳不用點數,沖銷不會遇到餘額不足;每步時刻仍是 `min(paid_at + 位移, at.now)`,種子時戳不變量照舊成立。保留
  `ts` 排序,讓 outbox 事件與站內通知依購買順序產生。
- **保留 `replay` 預查**:改成「清空 → 加入 → 帶 key 結帳 → 再清空」雖然少一次查詢,但開發者下架某個種子商品後
  重跑會在加入購物車時失敗,而且每次重跑都改寫所有種子會員的購物車。
- `refuse_legacy_seed_rows`、`insert_pending_order_if_absent` 與點數結算留在 `run`;介面測試在 `timeline.rs`,用
  最小目錄,不跑整份資料集。
- 本 ADR 的決策不變:共用「歷史寫入」module 仍延後,重開條件 1 仍未成立。

## Addendum (2026-10-16): 訂單列只有一個寫入者

R22 讓 `orders::repository::create_order(tx, &NewOrder)` 成為 `orders` 列唯一的寫入者:checkout(`status: Paid`)、
seed 的待付款對照單(`insert_pending_order_if_absent`,`status: Pending`)與測試 fixture `OrderSeed`(`insert_tx`
= `create_order` + `create_order_items`,`insert` 包成一個交易)都經它;另外三份 `INSERT INTO orders` 與報名、訂閱
測試各自複製的 `seed_order` helper 刪除。

- 這不是本 ADR 延後的共用「歷史寫入」module:它只擁有一張表的一列,不寫訂單行以外的購買產物。`OrderSeed` 照舊
  不寫點數帳、不走 checkout,也不造「已結帳、已結算」的歷史。
- 重開條件 1 照字面仍不成立:沒有第二個需要造那種歷史訂單的測試族群。裁決不變,共用「歷史寫入」module 仍延後。

## Addendum (2026-10-16): seed 規則各有 interface,run 只留組合

R22 把 seed 每條有自己規則的寫入收成 `bin/seed/dataset/` 下的子 module,各有一個窄介面與只用最小目錄的
`#[sqlx::test]`,不跑整份資料集;`dataset::run` 只組資料集公式、依序呼叫它們。本則更正上兩則的過時說法:
Addendum 2026-10-15「種子購買時間線」說 `insert_pending_order_if_absent` 與點數結算留在 `run`,Addendum
2026-10-16「訂單列只有一個寫入者」把待付款對照單記成 `insert_pending_order_if_absent`——兩者都已不在 `run`,
該函式也已不存在。Addendum 2026-10-11 的 bypass 清單裡 `upsert_user`/`upsert_seed_member` 與
`insert_pending_order_if_absent` 兩個名字同理過時;那幾則照當時狀態保留,現況以本則為準。

- **子 module**(GLOSSARY 各詞條是規則 owner):
  - `accounts::upsert_account`——帳號誕生,seed 唯一一份 `INSERT INTO users`,兩份 users upsert 合一(GLOSSARY
    「帳號誕生」)。
  - `settlement::settle_members`——每位報表會員結算到點數級距目標(GLOSSARY「點數級距」)。
  - `pending::insert_pending_orders`——待付款對照單,經 `orders::repository::create_order` 與
    `create_order_items`(GLOSSARY「Seed 資料集」的「待付款對照單」)。
  - `attendance::mark_past_sessions`——種子點名,批次寫入是私有的(GLOSSARY「Seed 資料集」的「點名」)。
  - `rental::seed_rentals`——場租時段與過去時段的預約;「是否已有預約」只看 `time_slot_id`(R22-15,
    256bbc5)(GLOSSARY「場租佔位」)。
  - `timeline::buy_and_walk`——種子購買時間線,不變(Addendum 2026-10-15)。
- **留在 `run` 的,與原因**:
  - 目錄(教練、課程與週課表時段、商品、優惠碼、獎勵、公告、場館):每個 helper 只是一張表的冪等寫入,
    沒有跨表規則,值是資料不是規則;抽成 module 只是把資料搬家,介面跟呼叫端一樣大。冪等由 `tests.rs`
    的 `same_instant_rerun_is_noop`(`row_counts` 不變)涵蓋。
  - 洽詢(`contact_inquiries`):同上,一個月份公式寫一張表;時戳上界由 `seeded_timestamps_never_exceed_run_instant`
    涵蓋。
  - `refuse_legacy_seed_rows`:守的是 `run` 自己的前置條件(任何寫入前),只有對 `run` 測才有意義
    (`run_refuses_a_database_seeded_by_an_older_seed`)。
  - 場次回填:規則已有 production owner `sessions::calendar::backfill_for_seed`,`run` 只給課程與日期範圍。
  - `assign_role`:規則 owner 是 `permissions::repository::assign_role`,`run` 這層只是包一個連線、以
    `assume_uncached` 消掉 `AccessDirty`;誰拿哪個角色是組合決定,所以 `accounts` 不收角色。
  - 資料集公式本身:月度商品訂單、39 筆課程購買、待付單取哪幾筆、場館與價格,組好交給上列子 module。
- **`bin/seed/tests.rs` 的搬遷**:只留對整份 `run` 才成立的不變量。
  - 測試 1 `same_instant_rerun_is_noop_and_settles_drift` 改名 `same_instant_rerun_is_noop`;它後半的 +500 漂移收斂
    搬到 `settlement.rs`(`drift_written_out_of_time_order_settles_into_one_chain`、
    `settles_each_member_to_its_target_once`)(27c425c)。
  - 測試 1 的 `occupying_bookings` 計數不變搬到 `rental.rs` 的 `same_instant_rerun_adds_nothing`;
    `later_run_keeps_invariants` 的容量不變量 ④ 搬到 `rental.rs` 的 `never_exceeds_capacity`(4a14489)。
  - helper `user_id_by_email`、`points_balance`、`total_occupying_bookings` 隨之刪除。
  - `accounts`、`pending`、`attendance` 的介面測試是新寫的,`tests.rs` 沒有對應區塊搬走(50c6bc1、cd84173、
    27e548a)。
- 本 ADR 的決策不變:這些子 module 是 seed 內部的 owner,不是共用「歷史寫入」module;`tests/common/fixtures.rs`
  沒有呼叫它們,`OrderSeed` 經 `create_order` / `create_order_items` 寫訂單列,但照舊不走 checkout、
  不產 points ledger,重開條件 1 仍未成立。共用「歷史寫入」module 仍延後。
