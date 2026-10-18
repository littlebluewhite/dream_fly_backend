# ADR-0019: 知識只住 owner module;GLOSSARY 只放定義

## Context

GLOSSARY 原本是詞彙表,後來變成第二份實作說明。2026-10-18 架構審查時它有 121 KB、58 條、1,882 個 code
span、296 個函式或測試名,最長的「Seed 資料集」一條 9 KB。條目列出私有函式名、每一個 interface 測試的名字、
修正用的 commit、參數順序與錯誤優先序。同樣的規則也寫在 owner module 的 module doc 和 ADR 增補裡:點數級距
的同一套規則寫在四個地方。方向也反了:seed 子 module 的檔頭寫「規則在 GLOSSARY 的某條」,GLOSSARY 在當
module 的 interface 文件用。

代價落在每一次改動:

- **改一條規則要改四處**:code、module doc、GLOSSARY、ADR 增補。最近 300 個 commit 有 95 個動到 GLOSSARY;
  抽查 17 個 code commit,13 個也得改它,連只動一個檔的 seed 修正也是。純文件對齊的 commit 一再出現。
- **ADR 也成了變更紀錄**:ADR-0007 有 74 KB、17 則增補,其中 9 則是逐一列出呼叫點的「遷移登記」;ADR-0010
  的增補約佔全文 85%,其中一則是在更正前幾則寫過時的函式名。
- **讀的人付最多**:AGENTS.md 要每個 agent 做領域工作前先讀 GLOSSARY 和相關 ADR,也就是 121 KB 再加上最多
  74 KB。真正的領域語言(「給點」是什麼)被埋在呼叫圖底下。

測試有同一種問題:有些斷言直接數資料表的列,而不是問 owner。token family 從「標記撤銷」改成「刪掉列」
之後,三個測試仍在數 `revoked = false` 的列,照樣會過,描述的卻是已經不存在的模型。

## Decision

**每一種知識只有一個家:GLOSSARY 拿詞,owner module 的 doc 拿規則,ADR 拿為什麼,integration contract 是
對前端的約定。**

- **GLOSSARY 只放定義**:每條是一兩句「它是什麼」加一行 `_Avoid_`。定義可以用 code span 點名 owner module,
  可以引用 ADR 編號;不放檔案路徑、行號、commit hash、輪次編號,也不列函式清單或測試名。只有新增、改名一個詞,
  或詞的意思變了,才動 GLOSSARY。
- **規則只寫在 owner module 的 doc**(`//!` 或檔頭):不變量、protocol、鎖順序、錯誤優先序、寫入路徑。module
  doc 可以點名少數釘住不明顯規則的測試;其餘測試名不列,測試檔本身就是清單。改一條規則只改 code 和這份
  doc。
- **方向是 GLOSSARY 指向 module**:module doc 自己寫規則,不寫「規則見 GLOSSARY」。
- **縮短時搬,不要丟**:縮一條之前先比對 owner module 的 doc,GLOSSARY 獨有的規則搬進 doc;純歷史(哪一輪、
  哪個 commit、已刪的函式名)直接刪,留在 git log 與 ADR。
- **ADR 只記決定和理由。增補從此只記「決定改了什麼、為什麼」**:不寫遷移登記,不列呼叫點,不列 commit hash。
  呼叫點在 code 裡,commit 在 git log 裡。
- **測試問 owner module**:關於領域事實的斷言經 owner 的讀法或 HTTP;raw SQL 只留給本來就是資料表層級的性質;
  bypass 準備資料說明它繞過哪個 owner。細則在 coding-standards 的「How tests assert」。
- **有人守著**:`tests/glossary.rs` 逐條掃描 GLOSSARY(定義 ≤ 200 字元、每條 ≤ 2 個 code span、要有
  `_Avoid_`、不出現路徑、行號、commit、輪次),沒有豁免清單:寫不下就拆詞,或把內容搬回 module doc。
  coding-standards 加兩條 review 依據:GLOSSARY 的 diff 只改定義和 `_Avoid_`;規則的改動要出現在 module doc。

## 不做

- 全面把測試裡的 raw SQL(審查時 241 處)換成 owner 讀法。測試因別的原因被改到時再順手換。
- 為了測試新增 owner 讀法,例如列出 session、依 id 讀請假單。
- 改 tests/common 裡本身就是 raw SQL 的 helper(最新通知、訂單數、點數餘額、庫存、場租佔位)。
- 改寫舊 ADR 的原文或增補,或把 ADR-0007、ADR-0010 縮短。
- integration contract 的文字小漂移,例如清空購物車那句寫成 `POST /cart`(其實是 `DELETE /cart`)、§1.4 的
  列表漏了幾個端點。留給下一個動到該節的人。
- fixtures 兩段過時的 doc(`seed_booking`、`set_birth_date`)、重複的測試小 helper(`t(h, m)` 等)、
  `checkout(...)` 的重複呼叫形狀。
- 「使用者+角色」回應的 6 處手組(觀察名單)。
- 任何 DTO、route、schema、migration 變更。
- 改使用者層級(`~/.claude` 下)的 skill。

## 落選方案

- **維持現狀,GLOSSARY 當整個 repo 的地圖**:一處就能讀到全部規則,但它離 code 最遠。改 code 的人看不到它,
  所以每次都要記得同步;同一條規則連同 code、module doc、ADR 共有四份,只能靠人工對齊。
- **掃描測試留一份豁免清單**:寫不下的條目永遠可以掛上清單,上限就只是建議,細節會再長回去。
- **把舊 ADR 的遷移登記與過時函式名一起刪掉**:ADR 是決定當下的紀錄,改寫它會失去「當時為什麼這樣想」。

## Consequences

- 改一條規則只動 code 和 owner module 的 doc;GLOSSARY 和 ADR 不必跟著同步。GLOSSARY 縮到約 20 KB、62 條。
- 開工前的讀法改變:AGENTS.md 與 `docs/agents/domain.md` 寫明 GLOSSARY 拿詞、owner module doc 拿規則、ADR
  拿為什麼。
- 舊 ADR 仍帶遷移登記、已經過時的函式名,有幾則還說規則在 GLOSSARY 的某條。讀現況以 module doc 和 code 為準,
  讀 ADR 只拿理由。
- 機器只檢查 GLOSSARY。module doc 是否寫了新規則、ADR 增補是否只記決定,靠 review 與 coding-standards。
