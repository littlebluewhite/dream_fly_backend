-- =============================================================================
-- Phase 5a: reports 時間軸算式收斂為 SQL function 單一真相。
--
-- `reports::repository` 有 16 個查詢站點各自內嵌同一組「先把 UTC 的 now 換
-- 成 studio 牆鐘、再截斷/轉型」算式——`date_trunc('month', $1::timestamptz
-- AT TIME ZONE $2)` 這個「本月 anchor」idiom 出現 11 次、`($1::timestamptz
-- AT TIME ZONE $2)::date` 這個「今天」idiom 出現 5 次——與 `utils::
-- studio_clock::{month_key, today}` 是同一條規則的兩份手抄副本(Rust 一份、
-- 16 處 SQL 內嵌各一份),任何一邊漂移都不會被編譯器或型別系統擋下。這兩
-- 個 function 把 SQL 那一側收斂成單一定義,`reports::repository` 的 16 個
-- 站點改呼叫 `studio_month_anchor($1, $2)` / `studio_today($1, $2)`,不再
-- 各自內嵌算式(row 側「事件歸哪個月」的 `AT TIME ZONE` 站點——如
-- `o.paid_at AT TIME ZONE $2`——是不同問題,刻意不動,仍各自內嵌)。
--
-- STABLE(而非 IMMUTABLE)——理由是 tzdata 本身可變(`AT TIME ZONE` 依賴的
-- IANA 時區規則會隨系統 tzdata 更新而改變,同一組輸入在不同 tzdata 版本下
-- 可能得到不同結果),不是因為呼叫了 NOW() 之類的易變函式——這兩個
-- function 完全不讀取當下時間,`now`/`tz` 都是呼叫端傳入的參數,語意上是
-- 純函式,只是「純」到 tzdata 版本層級為止。函式本體就是原本內嵌的運算
-- 式,STABLE 的單一 SELECT 會被 planner inline,不會比原本內嵌寫法多一次
-- 函式呼叫開銷。呼叫端原本顯式的 `$1::timestamptz` cast 由函式簽章的
-- TIMESTAMPTZ 參數型別取代,不需要在呼叫處再寫一次。
--
-- 窗長(11 個月、30 天、90 天……)不在這兩個 function 之內——interval 減
-- 法沒有「先換時區再截斷」那種順序陷阱,窗長維持 Rust/呼叫端常數,不做第
-- 三個 offset function。
-- =============================================================================

CREATE FUNCTION studio_month_anchor(now TIMESTAMPTZ, tz TEXT)
RETURNS TIMESTAMP LANGUAGE sql STABLE AS $$
  SELECT date_trunc('month', now AT TIME ZONE tz)
$$;

CREATE FUNCTION studio_today(now TIMESTAMPTZ, tz TEXT)
RETURNS DATE LANGUAGE sql STABLE AS $$
  SELECT (now AT TIME ZONE tz)::date
$$;
