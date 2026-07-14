# ADR-0002: Cart/Orders 擴充 `item_type` 支援課程項目

## Context

上線前的商業模式：使用者除了買方案/票券（`products` 表）之外，也要能直接把「一門課」加進購物車、和其他商品一起結帳。原本 `cart_items`/`order_items` 只認 `product_id`。需要決定怎麼讓課程進到同一個購物車/結帳流程，有三個選項：

1. **把課程包成一種 `product`**（例如 `product_type = 'course_package'`，額外存一個 `course_id` 外鍵欄位或走 metadata JSONB）。
2. **課程走獨立的報名端點**（例如 `POST /courses/{id}/enrol`），完全繞開 cart/orders，各自處理付款與確認。
3. **擴充 `cart_items`/`order_items` 為 discriminated union**：新增 `item_type`（`product`|`course`）+ 對應的 `course_id`，兩者互斥，讓課程與商品在同一張購物車、同一次結帳內共存。

## Decision

採用 **方案 3**：`cart_items`/`order_items` 都擴充 `item_type` + 互斥的 `product_id`/`course_id`（DB CHECK constraint 保證恰好一個非空），前端契約對應為 `{ item_type, item_id }`（見 `docs/api/integration-contract.md` §3.8）。

結帳（`POST /orders`）在同一個資料庫交易內，依購物車行的 `item_type` 分別產生對應的 artifact：

- 課程行 → 呼叫 `enrolments_service::enrol_from_purchase_tx`，建立一筆 `enrolments`（報名成功，course 額滿或重複報名會讓整筆結帳回滾）。
- 商品行（`ticket`/`membership` 類）→ 呼叫 `subscriptions_service::grant_from_purchase_tx`，建立一筆 `subscriptions`（entitlement，見 ADR-0003）。

`OrderResponse` 因此多了 `enrolments[]`/`subscriptions[]` 兩個欄位，回傳「這筆訂單」產生的 artifacts。

沒有選方案 1（課程包成 product）：課程有自己的欄位形狀（`coach_id`、`schedule_text`、`max_students`、`min_age`/`max_age`），硬塞進 `products` 表會產生大量無意義的 nullable 欄位，且「這是課程還是商品」的判斷邏輯會散落到每個消費端。也沒有選方案 2（獨立報名端點）：那樣使用者沒辦法「課程 + 商品一次結帳、一次付款、一次套用同一張優惠券/點數」，體驗上是兩次割裂的付款流程。

## Consequences

- `cart_items`/`order_items` 的唯一性約束從單一 `UNIQUE(user_id, product_id)` 拆成兩條 partial unique index（`uniq_cart_items_product`/`uniq_cart_items_course`），且新增 `cart_items_one_target`/`order_items_one_target` CHECK constraint 保證資料完整性在 DB 層即被強制，不只靠應用層邏輯。
- 課程行的數量恆為 1（`cart_items_course_qty` CHECK）——課程沒有「買 3 份同一門課」的概念，商品才有數量。
- 結帳交易變複雜：需要對商品行與課程行分別排序（依 `product_id`/`course_id`）才鎖表，避免兩個並發結帳因鎖定順序不同而死鎖（見 `orders::service::checkout` 內的排序註解）。
- 前端購物車/結帳頁必須同時處理兩種 item_type 的顯示與互動（無法假設購物車只有商品），這是 Task 15（購物車 UUID 改版）與 Task 16（結帳接線）要處理的核心改動。
- 課程報名失敗（額滿/重複報名）現在會讓**整筆訂單**（含其他商品行）一起回滾，而不是部分成功——這是刻意的設計（不允許「買了方案但課程報名失敗」的不一致狀態），前端需要把結帳的 409 錯誤處理成「請調整購物車內容再試」，而非局部重試。

## Addendum（2026-07-14）：課程行排序 owner 遷至 enrolments::enrol_batch_from_purchase_tx

Decision「課程行 → 呼叫 `enrolments_service::enrol_from_purchase_tx`」與 Consequences 第三條「結帳交易…需要對商品行與課程行分別排序（依 `product_id`/`course_id`）才鎖表…（見 `orders::service::checkout` 內的排序註解）」自此更新：checkout 不再自己內聯 `.filter(matches!)` 篩課程行、`.sort_by_key` 排序、逐行迴圈呼叫 `enrol_from_purchase_tx`。item_type 分派收進純函式 `orders::fulfilment::plan`（對 `CartItemType` 一處 exhaustive match），課程行的排序（`course_id` 序）連同其死鎖防護紀律遷入 `enrolments::service::enrol_batch_from_purchase_tx`——商品行早有的 `products::service::reserve_stock_tx` 對應物，課程行的批次深函式 owner，在拿寫鎖之前排序自己的副本。checkout 改為單次呼叫 `enrol_batch_from_purchase_tx`（內部複製、`sort()`、逐一委派仍 public 的 `enrol_from_purchase_tx`——座位鎖協定的文件化 owner）。此為函式體重接，wire format 與回滾語意不變。本檔其餘敘述維持決策當下狀態。
