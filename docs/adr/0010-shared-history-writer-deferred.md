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
