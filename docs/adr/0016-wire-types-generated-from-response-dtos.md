# ADR-0016: Wire 型別由 response DTO 產生(ts-rs → committed `bindings/`)

## Context

前端的 API 型別是手寫的 TypeScript,與後端 response DTO 各寫一份。兩份之間沒有任何機制保證一致:
後端改欄位(加 `paid_at`、`earned_this_month`、把狀態欄位收成 enum)時,前端只能靠人記得同步,
漂移要到執行期才露出來(欄位 `undefined`、拼錯的狀態字串永遠比不中)。

W-1 已把 18 個 wire enum 的 serde 拼字對齊 `as_str`(`tests/wire_serde.rs` 釘住),W-2 把 response
DTO 的狀態類欄位從 `String` 換成這些 enum。Rust DTO 因此已經能完整描述 wire 形狀;缺的是把它變成
前端編譯器看得到的型別。

## Decision

**Response DTO 是 wire 形狀的唯一來源:以 ts-rs 12 產生 TypeScript,輸出 commit 進後端
`bindings/`,前端同步這個目錄(Controller 裁決 8)。**

- 依賴:`ts-rs = { version = "12", features = ["chrono-impl", "uuid-impl", "serde-json-impl"] }`;
  dev-dependency 另開 `format`(輸出經 dprint 排版,`bindings/` 的 diff 好讀)。
- 範圍:所有 response DTO(各模組 `dto.rs` 中 `Serialize` 的回應型別)、它們引用的
  `PageMeta`、`OrderItemBrief`、`StudentCourseBrief`、錯誤模組的 `MessageResponse`、18 個 wire enum,
  以及 `serde_json::Value`(`NotificationResponse.metadata`,輸出為 `serde_json/JsonValue.ts`)。
  共 113 個型別。只 derive `ts_rs::TS`,不加 `#[ts(export)]`——匯出只有一個入口(下一點)。
- **清單 + 檢查模式**:`tests/wire_types.rs` 的 `wire_types![…]` 是匯出清單。測試以
  `Config::new().with_large_int("number")` 匯出到 `$CARGO_TARGET_TMPDIR/wire`,然後:
  1. 兩個清單型別寫到同一個 `.ts` 檔 → 失敗(撞名);
  2. 清單型別引用了未列入清單的型別(依 Rust `TypeId` 比對,未列入但與清單型別同 TS 名的也抓得到)
     → 失敗(漏列);
  3. 匯出的檔案集合必須等於清單;
  4. 產生排序的 `index.ts`(`export type { X } from "./X";`);
  5. 與 committed `bindings/` 逐檔比對,不符即失敗並列出 missing/stale/extra,提示
     `WIRE_BINDINGS=write cargo test --test wire_types` 重新產生。
  6. 另一個測試掃 DTO 原始碼,抓「derive `Serialize` 卻不在清單」的型別(見 Consequences)。
  `cargo test` 因此保證 `bindings/` 永遠等於當下 DTO 的輸出;改 DTO 不重新產生,gate 不會綠。
- **`i64` → `number` 不變量**:`with_large_int("number")` 把 `i64`/`u64` 輸出為 `number`。這成立的前提是
  wire 上的整數(金額 cents、點數、計數)永遠 ≤ 2^53;超過的值 JSON.parse 會失真。若出現可能超過的欄位,
  它必須以字串上 wire,不得沿用 `i64`。
- **enum 拼字**:TS 的字串聯集由 serde `rename_all` 產生;serde 拼字 == `as_str` == PG label 由
  `tests/wire_serde.rs` 釘住。三者一致,前端型別才等於 DB 值域。
- **改名(只改 TS 名,不改 Rust struct)**:兩組 Rust 同名型別會寫同一個檔案——auth 的
  `UserResponse` 以 `#[ts(rename = "AuthUserResponse")]`、錯誤模組的 `MessageResponse` 以
  `#[ts(rename = "MessageAck")]` 輸出。
- **欄位標註**:`CouponValidateResponse.applied_discount_cents`(`skip_serializing_if = "Option::is_none"`)
  加 `#[ts(optional)]`,輸出 `applied_discount_cents?: number`,與「缺鍵」的 wire 一致;
  `InquiryResponse.inquiry_type` 存為文字、寫入時驗證,以 `#[ts(as = "InquiryType")]` 輸出為 enum 聯集。
- **只含 response**:request DTO 不 derive `TS`。request 端的雙層 `Option`(`deserialize_some`:
  缺鍵 = 不改、`null` = 清空)ts-rs 無法如實表達,遞延到需要時另議。

## 落選方案

- **specta**:目前仍是 rc 版本,且沒有全域「bigint → number」設定,`i64` 欄位得逐一標註。
- **utoipa(OpenAPI)→ openapi-typescript**:要為每個 handler 寫路由註解、產生並維護整份 OpenAPI 文件,
  只為取得型別太重;本庫的端點文件已由 `docs/api/integration-contract.md` 承擔。
- **`#[ts(export)]` 讓 ts-rs 自動產生測試**:每個型別各自匯出,沒有清單,無法檢查漏列/撞名,也無法與
  committed 目錄比對。
- **不 commit `bindings/`、由前端建置時呼叫 cargo**:前端建置需要 Rust 工具鏈,且 DTO 變更在後端 PR
  的 diff 中看不到 wire 形狀的變化。

## Consequences

- 新增 response DTO:derive `ts_rs::TS`、加進 `wire_types!` 清單、`WIRE_BINDINGS=write cargo test --test
  wire_types`,commit `bindings/` 的變更。忘了列入清單而被其他清單型別引用 → 依賴檢查失敗並指出型別;
  沒被引用的頂層型別由 `every_serialize_dto_is_listed` 抓:它掃 `src/modules/*/dto.rs`、
  `src/error/mod.rs`、`src/extractors/pagination.rs`,每個 derive `Serialize` 的 struct/enum 必須在
  `wire_types!` 清單,或列入 `NOT_WIRE_TYPES`(附一行理由;目前是三個 request entry:`ScheduleEntry`、
  `SlotEntry`、`CourseScheduleSlotEntry`)。放在其他檔案的回應型別不在掃描範圍,仍靠 review。
- 新的 Rust 同名回應型別需要 `#[ts(rename = "…")]`,否則撞名檢查失敗。
- DTO 欄位的 doc comment 會成為 TS 的 JSDoc,一併出現在 `bindings/`。
- 前端由 `scripts/wire.mjs` 同步 `bindings/` 到 `src/lib/api/generated/`(前端任務),漂移成為前端
  `npm run check` 的編譯錯誤。
- 回 `Json<serde_json::Value>` 的端點沒有具名 DTO,不在產生範圍內。
- ADR-0005「不做泛型 `Paginated<T>`」不變:具名信封各自產生自己的 TS 型別,`PageMeta` 經
  `#[serde(flatten)]` 內嵌成 `total`/`page`/`per_page` 欄位。
