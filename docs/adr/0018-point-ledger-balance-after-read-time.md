# ADR-0018: 點數帳 `balance_after` 讀時推導;刪除 `point_ledger.balance_after`

## Context

`point_ledger.balance_after` 是寫入時落地的「這一筆之後的餘額」。`apply_delta_tx` 把
`UPDATE users SET points_balance = points_balance + $2 … RETURNING points_balance` 的回傳值寫進去,所以它依
`users` 列鎖的取得順序(也就是寫入順序)累加。`GET /points/me` 的明細卻依 `created_at` 列出,而 `created_at`
是呼叫端綁進來的業務時間(ADR-0017 W7-5),不是寫入順序。兩個順序不一致時,列出的明細就串不起來:某一列的
`balance_after − delta` 不等於下一列(較舊那列)的 `balance_after`,第一列也不一定等於現在的餘額。

- seed 撞過(`b879720`):退款的 `refund_clawback` 先寫,付款時間更早的購買後寫,seed-member-09 的明細
  8/30 顯示 210、8/31 顯示 0。當時的修法在 seed 端:用 `walk_due` 加最小堆排程,讓寫入順序跟著時間走,
  再由 `seed_points_history_chains_in_time_order` 釘住(同一刻的列要靠排列搜尋 `chain_end`)。這條規則因此
  落在 seed,而不是點數帳的 owner。
- runtime 也有窗口:handler 先取樣 `now`,交易內稍後才取 `users` 列鎖。同一位使用者的兩個請求,可以取樣
  與取鎖的先後相反(亞秒級)。
- 明細的 `ORDER BY created_at DESC` 沒有 tiebreaker。同一刻的列(結帳的 `checkout_redeem` 與 `checkout_earn`、
  退款的 `refund_restore` 與 `refund_clawback`)依掃描順序回傳,跨頁可能重複或漏列。

`balance_after` 和 ADR-0015 的 `time_slots.booked` 同一種形狀:由寫入端維護的衍生值,是第二份事實。

## Decision

**`balance_after` 不落地,由明細查詢在讀取時推導;刪除 `point_ledger.balance_after`。**

- 明細(`points::repository::find_ledger_by_user`)依 `(created_at DESC, id DESC)` 排序。`id` 是主鍵,所以
  這是全序:同一刻的列依 `id` 排,翻頁穩定。
- 錨點是讀取當下的 `users.points_balance`。每一列的 `balance_after` = 錨點 − 排在它前面(較新)所有列的
  `delta` 總和,即 `SUM(delta) OVER (ORDER BY created_at DESC, id DESC ROWS BETWEEN UNBOUNDED PRECEDING AND
  1 PRECEDING)`。window 在 `LIMIT`/`OFFSET` 之前算,所以每一頁都接得上前一頁。錨點是同一條陳述句裡的純量
  子查詢,和明細列讀的是同一個快照。
- 因此不論寫入順序與寫入時刻:第 0 列等於餘額;每一列等於上一列的 `balance_after − delta`;最舊一列的
  `balance_after − delta` 等於「餘額 − 全部 `delta` 總和」,餘額全由點數帳維護時就是 0。
  `tests/service_points.rs::ledger_lists_a_chain_from_the_balance_whatever_the_write_order` 釘住這件事:
  寫入順序刻意與業務時間錯開,含同一刻的三列,`per_page` 2 翻三頁串起來等於全表。
- Migration `20261015000001_point_ledger_balance_after_read_time`:`DROP COLUMN balance_after`;
  `idx_point_ledger_user` 由 `(user_id, created_at DESC)` 重建為 `(user_id, created_at DESC, id DESC)`,與明細
  的排序一致。
- 寫入端:`insert_ledger_tx` 不再收 `balance_after`,也不再 `RETURNING` 整列(唯一的呼叫端 `apply_delta_tx`
  本來就丟掉回傳值)。ADR-0007 2026-08-03 Addendum (c) 記的簽章因此少了 `balance_after`,其餘論證不變。
  `apply_delta_tx` 照舊回傳 UPDATE 之後的餘額:`RedeemResponse.balance_after` 與
  `PointsAdjustmentResponse.balance` 用的是「現在的餘額」,不受影響。
- 讀取端:`PointLedgerEntry.balance_after` 保留,由查詢填入;`LedgerEntryResponse` 與 wire 形狀不變。契約
  §3.14 補上排序與 `balance_after` 的語意。
- 測試 fixture `seed_point_ledger_entry` 去掉 `balance_after` 參數。seed 的
  `seed_points_history_chains_in_time_order` 與 `chain_end` 刪除:鏈由查詢本身擔保,期初為 0 則由 seed
  不變量②的 `points_balance == SUM(point_ledger.delta)` 擔保。seed 的 `walk_due` 排程不再是明細串得起來的前提。

## 落選方案

- **維持現狀**(寫入時落地,要求呼叫端依業務時間的順序寫):規則落在每一個寫點數帳的呼叫端。seed 要靠排程
  加排列搜尋測試才守得住;runtime 取樣與取鎖之間的窗口沒有任何東西擋;同一刻的列仍然沒有順序。
- **保留欄位,補寫較早的列時改寫其後所有列**:每次寫入都要鎖住並 UPDATE 一段區間,成本隨歷史長度增加。
  衍生值仍是第二份事實,fixture 與 seed 的直寫都得跟著維護(與 ADR-0015 否決「保留欄位、補 trigger」的理由
  相同)。同一刻的列的順序也還要另外定義。
- **把 `created_at` 夾到不早於該使用者最新的一列**(讓寫入順序等於時間順序):違反 ADR-0017,業務時間必須是
  handler 取樣的 `now`。被夾過的時間可能跨進下個月,`earned_this_month` 的分桶跟著錯;seed 的「點數帳時間 =
  `paid_at`」(不變量④)與 checkout 的購買時刻(`checkout_stamps_every_purchase_artifact_with_the_sampled_now`)
  也都會壞。
- **依 `id`(寫入順序)列出**:鏈串得起來,但明細不再依業務時間排列。會員看到的日期前後跳動,seed 補寫的
  歷史列會排在最上面。
- **從 0 往前累加**(從最舊一列起算 `SUM(delta)`):錨點變成點數帳的總和,而不是餘額。餘額與點數帳對不上時
  (測試 harness 的 `set_points_balance`、點數帳出現之前的舊資料、開發者手改的餘額),第 0 列就不等於同一個
  回應裡的 `balance`,會員看到的最新一筆和餘額對不上。以餘額為錨,最新一列永遠對得上,不一致只會落在「期初
  不是 0」。

## Consequences

已知缺口(接受):

- **中間值可能為負**:業務時間早於其資金來源的扣點(扣點列的 `created_at` 早於支付它的那筆賺點),明細中間
  某一列的 `balance_after` 可能是負數。真實餘額仍受 `users_points_balance_check` 保護;前端不顯示明細的
  `balance_after`;seed 不會產生這種情況。
- **不是寫入當時的快照**:補寫一筆業務時間較早的列後,比它新的每一列的 `balance_after` 都會變成「含這一筆」的
  值。這正是讀時推導的用意。
- **繞過點數帳改餘額,整段明細一起位移**:`set_points_balance` 或手寫 SQL 只改 `points_balance`、不寫點數帳,
  明細每一列都差同一個值,期初不是 0。只有 GLOSSARY「給點」記錄在案的測試 bypass 會這樣做。
- **`/points/me` 的 `balance` 與明細是兩條陳述句**:兩者之間有寫入時,第 0 列可能不等於回應裡的 `balance`。
  把四個讀取收進同一個快照另案處理(R21-10)。

其他後果:

- 每讀一頁都要對該使用者的全部點數帳算 window,成本與該使用者的列數成正比。單一會員的點數帳是百筆量級,
  新索引依明細的排序順序掃描。
- migration 之後,舊版 binary 寫不了點數帳:它的 `INSERT` 指名的 `balance_after` 欄位已不存在。
- 新的點數帳明細讀取必須經 `find_ledger_by_user`,或沿用同一個 window。欄位已不存在,手寫 `balance_after` 會在
  執行期失敗。
