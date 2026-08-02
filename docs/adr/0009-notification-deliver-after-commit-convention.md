# ADR-0009: Notification 交付時機維持位置慣例,不做 witness 化

## Context

`notifications::service::PendingNotification`(`src/modules/notifications/service.rs:48-77`)是建構完成、尚未寫入資料庫的通知:`#[must_use]` 型別,`deliver(self, db: &PgPool)`(:74)消費自身,是唯一的 IO 入口。`#[must_use]` 只擋一種疏漏——建構了卻整個忘記呼叫 `.deliver`,`cargo build` 時跳警告(型別自身的 doc comment 已誠實記載這個警告的邊界:只在回傳值被直接當陳述式捨棄時觸發,不是強制 lint,也不是編譯錯誤)。它擋不住另一種疏漏:「commit 之後才 deliver」是位置慣例,不是型別層擔保——沒有任何東西阻止呼叫端在 `tx.commit()` 之前就呼叫 `.deliver(db)`;若真的那樣寫,一筆交易稍後回滾,使用者仍會收到一則指向不存在資料的幽靈通知。

這條慣例目前有 8 個站點,逐站查證無例外,寫進本 ADR 作為封存時點的盤點:`auth/service.rs:116`、`:313`;`bookings/service.rs:86`、`:164`;`orders/service.rs:410`、`:635`;`certificates/service.rs:121`;`leave/service.rs:227`。全數在交易 commit 之後(`tx.commit()`,或 orders 兩站的 `TxReleased::commit`)才呼叫 `.deliver(db)`。

`orders` 的兩個站點另有一層細節值得記錄。`checkout` 與 `update_order_status` 各自在 commit 後拿到一個 `tx_witness::TxReleased` witness(`orders::service` 內的私有 `mod tx_witness`),但這個 witness 實際 gate 的是 `assemble_response`——其簽章收 `_released: tx_witness::TxReleased` 作為值參數(`orders/service.rs:496`),沒有這個值就無法呼叫。`.deliver(db)` 呼叫排在 `TxReleased::commit`(`orders/service.rs:404`、`:625`)之後、`assemble_response` 呼叫之前,只是搭 witness 已經強制出來的執行順序的便車——它自己並沒有被那個 witness 擋住:`deliver` 的簽章只收 `self` 與 `db: &PgPool`(`notifications/service.rs:74`),搬到任何位置呼叫都一樣通過型別檢查。

## Decision

**不 witness 化,維持位置慣例**:「commit 之後才呼叫 `.deliver(db)`」不收進型別系統,繼續由呼叫端遵守慣例,`#[must_use]` 繼續只負責擋「忘了送」這一種疏漏。兩個曾被評估的候選形狀,與各自的否決理由:

### 1. 否決候選:witness token

讓 `.deliver(db)` 改收一個由 commit 端建構的「tx 已 commit」證明值,型別層強制沒有這個值就無法呼叫。否決理由:這個 token 能證明的只有「某個 tx commit 過」——建構它的呼叫端可以是程式裡任何一段已經 commit 的程式碼,不必是這筆通知實際依附的那個 tx。要讓 token 真正證明「這一筆資料所在的那個 tx」,token 必須攜帶與被寫入列綁定的識別,而目前沒有一種自然存在的資料能扮演這個角色——退化下來只是「commit 曾經發生過」的空證明,買不到真正想要的保證。這與既有 `orders::service` 私有 witness `TxReleased` 的 `no_open_tx()` 建構子(`orders/service.rs:466-468`)殘留的弱點同型:`TxReleased` 的 doc comment(`orders/service.rs:449-454`)自陳 `no_open_tx` 是「caller-attested assertion, not a machine-checked fact」——呼叫端口頭保證「這條路徑沒有開過 tx」,型別本身不驗證這句話真假。花一個新型別的建構與穿線成本,換到的保證強度跟現有殘餘縫隙一樣,不成立。

### 2. 否決候選:`deliver_after(tx)` combinator

讓 commit 本身的呼叫順帶消費 `PendingNotification`,一次呼叫做完「commit 交易 + 送通知」兩件事,型別上不可能「commit 了卻沒送」,也不可能「commit 前送」。否決理由:與 orders 衝突。`checkout`(`orders/service.rs:404-414`)與 `update_order_status`(`orders/service.rs:625-638`)在 commit 之後、deliver 之後,還要呼叫 `assemble_response` 組裝回應——deliver 不是「commit 之後的最後一步」,是中間一步。一個把 commit 與 deliver 綁成單一動作的 combinator,沒有自然的地方安放 `assemble_response` 這第三步,combinator 不是通用形。

**佐證先例**:`points::service` 已有「刻意不套更強型別保證」的在案裁決——ADR-0007 Addendum(2026-08-03)`LedgerDelta` 的意識性排除(b):四個幅度來源(`PricingOutcome`、`RefundPlan`、`rewards.points_cost`、seed 字面值)已各自有 owner 級保證(純函式核測試、DB CHECK)兜底,選擇維持 `debug_assert` 而不是升級成 `Result`/`u64` 型別強制——對一個已有 owner 兜底的量再加一層型別強制,是不必要的重複成本。這裡的判準相同:8 個站點均一形(`tx 內完成領域寫入 → commit → deliver`)、`#[must_use]` 擋住「忘了送」、CONTEXT.md 明文承認「時機本身仍是位置慣例」,三者合起來已經是這條慣例現有的 owner 級保證。witness 化是逐案裁決,不是教條——points 那一案選擇不加型別強制,notification 這一案依同一判準,也選擇不加。8 站均一形 + `#[must_use]` + CONTEXT 明文承認,是合理停損。

## Consequences

- 未來的架構審查若再考慮「要不要把 `.deliver(db)` 的呼叫時機收進型別系統」,直接視為本 ADR 已決,不必逐輪重新論證,除非命中下方重開條件。
- 8 個呼叫站點的現況(`tx 內完成領域寫入 → commit → deliver`)與 `PendingNotification` 的型別本身皆不因本 ADR 變動;CONTEXT.md「Notification」詞條指向本 ADR 作為裁決封存的記錄點。
- **重開條件**(明文,符合任一才重開設計輪,不是自動觸發改動):
  1. 真實發生過一次 pre-commit deliver 的 bug——某個呼叫端在 `tx.commit()` 之前呼叫了 `.deliver(db)`,產生指向已回滾資料的幽靈通知。這代表位置慣例在實務上已經不足以防呆,需要重新評估型別化的成本效益。
  2. notification 交付模型改為 tx 內 outbox(領域寫入與通知紀錄同一交易落地,由背景 dispatcher 之後才真正對外送達)。這種模型下「commit 之後才 deliver」這條規則本身被取代,不是被加強;整個位置慣例連同本 ADR 兩個候選的否決理由都需要重新評估,不是在現有 `PendingNotification` 上修補。
