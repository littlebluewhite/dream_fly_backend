# ADR-0013: Kafka 重試政策：發件匣不放棄、消費端原地有限重試

## Context

Kafka 管線兩端各有一個重試迴圈,失敗時的去留要分別裁決:

- **發件匣派送(outbox dispatch)**:`kafka::outbox::drain_once` 把 `events_outbox` 的列送上 Kafka。事件列是在
  業務交易內與業務寫入一起 commit 的(CONTEXT「Event」),除了這一列之外,事件在系統裡沒有第二份——放棄
  發送就等於事件遺失。R16 B2 之前,`drain_once` 在一個交易裡逐筆 await publish,失敗只記 `attempts + 1`、
  `last_error`,下一個 tick(500ms 後)立刻重發,既沒有退避也沒有上限;publish 一慢,交易閒置超過
  `idle_in_transaction_session_timeout`(`main.rs` 設 30s)就被 Postgres 砍掉,連記帳都落不了地。
- **稽核消費端(audit consumer)**:`kafka::consumer::run` 把 6 個 topic 的事件寫進 `audit_log`(CONTEXT
  「消費迴圈」)。Kafka 的 committed offset 是 per-partition 的單一游標:一則訊息沒解決,同 partition 之後的
  訊息都不能 commit。R16 B1 之前,`Transient` 只是「不 commit、讀下一則」,下一則一 commit,失敗那則就被
  offset 越過、默默丟掉。

兩端都要回答同一個問題——「一直失敗的訊息,要重試到什麼時候?」——但兩端丟掉一則訊息的代價、以及卡住
時擋住的範圍都不同。

## Decision

**發件匣永不放棄,指數退避上限 1 小時;消費端對同一則訊息原地有限重試,滿 5 次後 error log 並 commit 丟棄。**
兩端刻意不對稱。

- **發件匣**(`src/kafka/outbox.rs`,R16 B2):
  - 失敗 → `attempts + 1`、`last_error`、`next_attempt_at = NOW() + retry_delay(attempts)`,
    `retry_delay(n) = min(2^n 秒, 1 小時)`;每次失敗 `tracing::error!`。
  - **沒有終止狀態**:列永遠留在 `published_at IS NULL`,退避到頂後每小時重試一次,直到成功。
  - 認領用 autocommit `UPDATE … SET next_attempt_at = NOW() + LEASE(60s)` 的租約,發送在交易外、受
    `PUBLISH_BUDGET`(15s,同時是 producer 的 `message.timeout.ms`)約束,記帳用短交易——失敗列退避中不擋
    其他列(`SKIP LOCKED` + `next_attempt_at <= NOW()`),代價是沒有 per-key 順序保證。
  - 不清理已發送列。
- **消費端**(`src/kafka/consumer.rs`,R16 B1):
  - `Poison`(JSON 壞、缺必要欄位)、非 UTF-8 payload → error log,立刻 commit。
  - `Transient` → 對**同一則**原地重試,呼叫間退避 1/2/4/8 s;呼叫滿 `MAX_ATTEMPTS = 5` 次(約 15 s)仍
    `Transient` → error log + commit(丟棄)。訊息一次只解決一則,後續 offset 絕不越過尚未解決的訊息。
  - 退避中收到 shutdown → 不 commit 直接結束,重啟後重送。
  - `From<sqlx::Error>` 仍把所有 DB 錯誤(含 FK 違例 23503)判為 `Transient`,不改。

**不對稱的理由**:

- **發件匣是事件唯一的來源**:放棄一列,事件就從系統裡消失,下游(audit、外部整合)永遠補不回來。發件匣
  的失敗多半是 broker 端問題(broker 掛了、topic 不存在、訊息過大),運維修好之後,未放棄的列會自己送出,
  不需要人工重排。而一列失敗的成本很低:退避到頂後每小時一次 publish 嘗試加一行 error log,其他列照發。
- **稽核消費端卡住會擋住整個 partition**:committed offset 是 per-partition 的,一則過不去的訊息會讓同
  partition 後面所有事件都進不了 `audit_log`。單一消費迴圈依序處理本實例分到的所有 partition(跨全部 topic),
  原地重試期間全部暫停(每則最多約 15 s)。`audit_log` 是衍生的紀錄(consumer 是 audit-only,見
  `consumer.rs` 模組文件),丟一筆稽核紀錄的代價遠小於整個 partition 停擺;而事件本身仍留在 Kafka(retention
  內)與 `events_outbox`(不清理已發送列),要補可以重放。有限重試吸收真正的暫時性錯誤(Postgres 重連之類),
  上限避免毒訊息永久卡住 partition。

## 落選方案

- **發件匣加死信/放棄狀態(例如 `attempts` 滿 N 次就標記 `dead`,不再重試)**:放棄就是遺失——事件沒有第二份,
  標成 dead 之後要靠人去發現、排除原因、再手動改回可派送,這條人工路徑比「修好 broker 就自動送出」更容易漏。
  一列毒訊息在發件匣裡不擋別人(退避 + `SKIP LOCKED`),留著它的成本只是每小時一次嘗試與一行 error log。
  若日後真的出現「永遠送不出去」的列,要處理的是它的成因,不是替它設終點。
- **消費端永不放棄(`Transient` 無上限原地重試)**:一則永遠失敗的訊息(例如 `audit_log.user_id` 指向已刪除的
  使用者)會讓該 partition 永久停擺,之後同 partition 的所有稽核事件都寫不進去——用一筆稽核紀錄換整個
  partition,划不來。
- **FK 違例(23503)改判 `Poison`,立刻 commit**:handler 分不出 23503 是永久的(參照列已被刪)還是暫時的,
  而在有限重試之下,把永久性 FK 失敗當 `Transient` 的代價只是約 15 s 的原地重試,之後照樣 error log 丟棄;
  改判換來的只有這 15 s,卻要在 `From<sqlx::Error>` 裡為單一錯誤碼開特例,不值得。分類維持不變。

## Consequences

- 發件匣裡的毒訊息不會自己消失:它會一直留在 `published_at IS NULL`,每小時一次 error log。監控應以「outbox
  error log 持續出現」或「`published_at IS NULL AND attempts > 0` 的列數」告警,並由人排除成因。
- 發件匣沒有 per-key 順序保證:失敗列退避時,同 `kafka_key` 的後續列照發。消費端本就要求冪等
  (`audit_log.id = event_id` + `ON CONFLICT DO NOTHING`),不依賴順序。
- 逾時(`PUBLISH_BUDGET`)的那列算一次失敗並停止這一批,沒輪到的列留著租約,60s 後重領——一列持續逾時的毒訊息
  最多讓一個 tick 停 15s,退避到頂後每小時一次。
- Postgres 中斷超過約 15 s 時(所有 DB 錯誤都判為 `Transient`),中斷期間消費到的每一則都會在 5 次後被 commit
  丟棄(每 15 s 一則);復原後需從 Kafka(retention 內)或 `events_outbox` 重放該時段。告警應以
  `transient handler failure exceeded retry cap; dropping` error log 的計數為準。
- 消費端丟棄的訊息只有 error log 留痕,沒有 DLQ;要補稽核紀錄得從 Kafka(retention 內)或 `events_outbox` 重放。
- `events_outbox` 無限成長(不清理已發送列);清理政策留待日後另行裁決。
