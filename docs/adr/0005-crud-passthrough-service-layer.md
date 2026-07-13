# ADR-0005: 純 CRUD 轉手 Service 層，接受為慣例成本

## Context

2026-07 一次架構審查針對 27 個模組跑了一輪淺模組（shallow module）普查，鎖定 service 層裡對
repository 呼叫幾乎零加值的轉手函式，統計結果：

- `coupons::service` 5 個函式裡有 4 個（`create_coupon`/`list_coupons`/`update_coupon`/
  `delete_coupon`）body 只做「呼叫 repository → 包進 Response DTO → 把 `None`/唯一鍵衝突映成
  `AppError::NotFound`/`AppError::Conflict`」，沒有任何校驗或業務判斷；同一支檔案裡唯一有實質
  邏輯的是 `validate_coupon`（negative subtotal 檢查 + `clamp_coupon_discount`）——4/5 轉手不是
  特例，是這個模組的常態。
- 7 個一行式 `list_my_*` 函式，分布在 `certificates`（`list_my_report_cards`/
  `list_my_certificates` 兩個）、`messages`（`list_my_conversations`）、`leave`（
  `list_my_leave_requests`）、`enrolments`（`list_my_enrolments`）、`subscriptions`（
  `list_my_subscriptions`）、`waitlist`（`list_my_waitlist`）六個模組，body 固定兩步：呼叫對應
  的 repository 查詢、把結果 map 進 Response DTO，無一例外多帶校驗或分支。
- 14 個契約形『`XListResponse`』信封（X 泛指各模組自己的具名前綴，如 `CouponListResponse`、
  `OrderListResponse`/`AdminOrderListResponse`、`PaginatedBookingsResponse`、`PointsMeResponse`
  等），各自帶一個具名複數鍵，加上從 `PaginationParams::meta()` flatten 出的 `total`/`page`/
  `per_page`。
- 約 12 份 repository 層「`count_*` 算總數 + `find_*` 撈當頁」的三段式儀式，逐模組各寫一份，結
  構相同但沒有共用實作。

這批函式共同特徵：body 是純轉手（repository 呼叫 → DTO 包裝 → 錯誤映射），不含校驗、交易或跨
表協調，乍看是「可以刪掉的樣板」。審查逐一評估三條收攏路徑，並做成本分析：

1. **砍掉這批函式的 service 層，handler 直接呼叫 repository。**——不成立。真的執行下去，DTO
   轉換與 `NotFound`/`Conflict` 映射得找新家，最終只是搬進 handler，不是刪除；而 27 個模組裡不
   少模組同一支 `service.rs` 內轉手函式（如 `delete_coupon`）與有實質邏輯的函式（如
   `validate_coupon`）並存，沒有乾淨的模組層級切割線。逐函式判斷「這支能砍、那支不能」，換來的
   是同一批模組裡有的走五檔剖面、有的走四檔剖面，讀者得先確認這個模組屬於哪一種才知道邏輯該去
   哪裡找——這筆認知成本比多讀幾支轉手函式更貴。
2. **14 個信封改用泛型 `Paginated<T>`。**——做不到。`docs/api/integration-contract.md` §1.4 已
   明訂分頁回應形狀是 `{ "<items_key>": [...], "total", "page", "per_page" }`，`<items_key>` 是
   隨端點而異的具名複數鍵（`coupons`/`courses`/`orders`……），這是**已對外公告的 wire format**，
   不是內部隨意選的欄位名。泛型 `Paginated<T>` 若要序列化成這個形狀，呼叫端得額外提供鍵名（額外
   的 trait/attribute/wrapper），複雜度不比現在 14 個各自十來行的 struct 低，只是把「具名」這件
   事從欄位定義搬去某個泛型參數，可讀性反而更差。
3. **抽一個 `paginate()` helper 收攏 `count_*`/`find_*`/組 `PageMeta` 的三段式。**——helper 本身
   仍是 shallow。每個模組的 `count_*`/`find_*` 簽名、篩選條件、是否限定 `user_id` 範圍都不同，
   helper 若要通用，要嘛接兩個閉包（呼叫端樣板行數不會比現在的三行少，只是換了個型式），要嘛吃
   一個過度泛型化的 query builder（等於自己刻一個 mini-ORM）。無論哪種，helper 都沒有為呼叫端擋
   掉任何決策或錯誤情境，只是把三行搬進另一個函式。

## Decision

**接受**純 CRUD 模組（或模組內純 CRUD 的個別函式）的轉手 service 層為慣例成本，不收攏、不砍
層。維持 27 模組統一的六檔剖面（`model.rs`/`dto.rs`/`repository.rs`/`service.rs`/
`handlers.rs`/`routes.rs`）在結構上的均一性——不論某個模組的業務邏輯厚薄，六個檔案永遠都在，
永遠各自負責同一件事。

判定依據是**導航價值**：任何人（人類或未來的架構審查）打開任一模組，不需要先判斷「這個模組屬
於哪一種剖面」就知道去哪支檔案找什麼——CRUD 校驗與錯誤映射永遠在 `service.rs`，不會因為某支
函式現在沒有業務邏輯就被搬去 `handlers.rs`、或讓 handler 跳過 service 層直達 repository。這個
零認知切換的價值，判定高於刪掉這 4+7+14+12 份轉手/儀式樣板換來的行數縮減。

## Consequences

- 未來架構審查看到 `coupons::service` 這類「多數函式是轉手」的模組、或任何新模組首版 service
  層全是轉手 CRUD，**不再重新提案收攏**——本 ADR 是既有裁決，除非符合下方「重開」條件，否則視
  為已決事項，不必逐輪重新論證。
- 新增 CRUD 模組時，即使規劃階段就能預見 service 層首版 100% 是轉手（無校驗、無交易），仍照六
  檔剖面建立完整的 `service.rs`，不因為「反正只是轉手」讓 handler 直接呼叫 repository，也不省
  略這一層。
- 14 個 `XListResponse` 信封、約 12 份 `count+find+meta` 儀式維持逐模組各自實作，不引入泛型分
  頁抽象層；新模組新增分頁端點時，比照現有模組的寫法（獨立 `count_*`/`find_*` + 具名信封
  struct）複製慣例，不必去找共用 helper。
- 此裁決建立在現有 REST JSON 剖面（六檔模組結構 + §1.4 具名複數鍵分頁）之上；若未來出現第三種
  剖面需求（例如 GraphQL 的欄位選擇式查詢，或另一種要求泛型分頁的 client），代表消費端形狀已經
  超出本 ADR 的假設範圍，應重開本 ADR 重新評估，而非在現有剖面上硬套變通。
