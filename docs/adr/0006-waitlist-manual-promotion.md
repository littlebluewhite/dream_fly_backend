# ADR-0006: 候補定案為人工遞補的諮詢名單，不做自動遞補

## Context

取消報名（`PATCH /enrolments/{id}/cancel`，`enrolments::service::cancel_enrolment`）釋出座位後，系統目前不
會發出任何後續信號——它只改自己的狀態，完全不觸碰 `waitlist_entries` 表。

`waitlist` 模組本身只有三個使用者可見動作：加入（`POST /waitlist`）、查自己的名單（`GET /waitlist/me`）、
取消（`DELETE /waitlist/{id}`，本人或 admin），外加一個 admin-only 的名單查詢（`GET /waitlist?course_id=`，
依加入序舊到新，`waitlist::service::list_for_course`）。沒有任何一個端點做「遞補」這件事，契約 §3.13 從一
開始也只描述這四個端點，從未承諾過任何形式的遞補。

repo 裡唯一出現「Promotion」字樣的地方是 `notifications::model::NotificationType::Promotion` 與
`posts::model::PostCategory::Promotion`，兩者都是「行銷促銷內容」分類，跟「候補遞補座位」是不同語意、只是
恰好同名——這代表「候補遞補」在這個 codebase 裡目前是**語意空洞**：沒有任何實作，也沒有既有命名可以類
比，是一段尚待決定「要不要做、怎麼做」的空白，不是遺漏或漏寫。這個空白需要一次決定填掉，讓未來的讀者不
必每次重新猜「候補現在到底會不會被系統動」。

## Decision

候補定案為**諮詢名單（advisory list）**：名額釋出後，遞補由 admin 依名單（`GET /waitlist?course_id=`，舊
到新）人工聯絡候補者；系統不做任何自動遞補、不自動通知。

關鍵理由：

- 報名的唯一入口是結帳（`POST /orders` → `orders::service::checkout`，ADR-0002）——自動遞補系統沒有辦法
  代使用者完成付款，「系統自動把候補第一名報成正式名額」這件事在這個模型下不成立。
- 更進一步的「通知第一名、保留座位到期後才輪下一位」模型，需要一個保留位（reservation）資料模型與到期
  釋放策略；現階段（單一場館）的營運量不需要這一層複雜度。

**兩個本決策必須明文的現實**：

1. **名單目前不含聯絡身分欄位**。`WaitlistResponse`（`waitlist::dto`）只有
   `{id, course_id, course_name, status, created_at}`，不含 `user_id`、姓名或任何聯絡方式。admin 依名單
   人工聯絡時，「這一列是誰、怎麼聯絡他」目前必須另行對照（直接查 `waitlist_entries`/`users` 表，API 不
   提供這層關聯）。這是本 ADR 記錄的 **known gap**：補上聯絡欄位屬於未來的一次小改動，不在本輪
   （docs-only）範圍內。
2. **結帳不會清掉候補列**。候補者完成結帳、拿到正式名額後，他在 `waitlist_entries` 裡的 `waiting` 列**不
   會**被自動取消——`orders::service::checkout` 完全不觸碰 `waitlist_entries` 表。清理定為人工：admin 代
   為呼叫 `DELETE /waitlist/{id}`（現行即 `owns_or_admin` 授權，見 `waitlist::service::cancel_waitlist_entry`）
   或請會員自行取消。名單因此允許**暫時**同時存在已經報名成功的人，這是刻意接受的狀態，不是 bug。

## Consequences

- 取消報名（`cancel_enrolment`）維持靜默，不新增任何對 `waitlist` 模組的呼叫、事件或副作用。
- 「名單無聯絡身分欄位」與「遞補後需人工清理」是本決策換來的持續營運成本——admin 每次遞補都多一步查詢對
  照，每次遞補成功都多一步手動清單。這個成本被判定低於現階段建置自動化（通知＋保留位＋到期釋放）所需的
  複雜度。
- 若未來要自動化遞補，或只是先補上聯絡欄位，都需要重開本 ADR、另立設計輪，至少涵蓋：通知策略（站內
  in-app 或外部 email/SMS）、座位保留與到期模型、以及與 `courses::seats` 鎖協定（`lock_course_seats_tx`
  ／無鎖 `course_seats`）的互動——這些不是在現有結構上加一個欄位就能拼湊出來的，需要一次完整的設計輪。
