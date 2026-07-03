# ADR-0003: 完整 Entitlement 的 Subscriptions，而非純購買紀錄

## Context

購買 `ticket`（堂票，如十堂票）或 `membership`（月票/季票/年卡）之後，系統需要知道「這個人現在還能不能上課」——不能只知道「他買過這個方案」。有兩種做法：

1. **純購買紀錄**：`orders`/`order_items` 已經記錄了誰買過什麼、什麼時候買的；上課權限判斷交給前端或另一套邏輯，在需要時（如報到、教練核銷）重新用「距離購買日期是否超過 N 天」「已用堂數」等規則即時算。
2. **完整 entitlement 記錄（`subscriptions` 表）**：結帳當下就依商品的 `session_count`/`valid_days` 產生一筆帶有 `expires_at`、`remaining_sessions` 的 entitlement row，並提供 `redeem` 端點原子核銷堂數。

## Decision

採用 **方案 2**：新增 `subscriptions` 表，結帳時由 `grant_from_purchase_tx` 依商品設定產生規則明確的 entitlement：

- `session_count` 有值 → `total_sessions = remaining_sessions = session_count * quantity`；若商品同時設了 `valid_days`，`expires_at` 也一併寫入（堂數與效期可以同時存在）。
- 只有 `valid_days`（無 `session_count`）→ 純效期方案，`expires_at = now + valid_days`，`quantity` 必須為 1（時間制方案不能「疊買」成一筆多份）。
- 兩者都沒有 → 視為無限期方案（理論情境，目前 seed 資料的 5 個方案都至少設了一項）。

`GET /subscriptions/me` 回傳的 `status` 是**讀取當下即時計算**（`derive_status`）：DB 存的 `status` 欄位只有 `active`/`cancelled` 兩種寫入時機，`expired` 是讀取時依「`expires_at` 是否已過」或「`remaining_sessions == 0`」動態算出——DB 本身不會有背景任務去改寫已過期的列。`POST /subscriptions/{id}/redeem`（admin/coach 專用）用單一原子 `UPDATE ... RETURNING` 核銷一堂，避免併發核銷產生競態。

## Consequences

- 換來的好處：前端（會員中心、教練工作台）不需要自己重算「這張月票還剩幾天」「這張十堂票還剩幾堂」——`GET /subscriptions/me` 直接給出 `derived status`/`remaining_sessions`/`expires_at`，邏輯單一入口在後端。核銷（教練幫學員報到扣一堂）也有明確、原子的 API 可用，不必自己在前端拼湊「purchase 時間 + 已上課次數」的推算。
- 換來的代價：結帳交易變重——每個商品行都要多判斷是否 entitlement-eligible（`ticket`/`membership`）並多寫一張 `subscriptions` row，交易涉及的表變多（見 ADR-0002 的鎖定順序考量）。
- `subscriptions` 的 `status` 欄位與「實際可用性」是兩個概念（存的是 `active`/`cancelled`，讀出來的是三態），任何直接查 DB（而非透過 API）的維運/報表工具都要記得套用同一個 `derive_status` 規則，否則會誤判已過期的方案為「active」。
- 若未來要支援「同一張月票可以被多人共用」「entitlement 可轉讓」等更複雜規則，現有的 `subscriptions` 表結構（一筆對應一個 `user_id`）需要另外擴充，不在本 ADR 範圍內。
