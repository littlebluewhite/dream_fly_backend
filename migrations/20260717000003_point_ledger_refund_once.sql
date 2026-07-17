-- =============================================================================
-- Step 10a(arch-deepening-r5)— point_ledger 退款方向唯一性下沉為 DB invariant。
--
-- 「每張訂單每個退款方向(restore/clawback)至多一列反轉」原是編排端(10e)
-- 要自行維持的規則;此 partial unique index 把它下沉成 DB 層保證——同一
-- (order_id, reason) 組合第二次寫入直接違反 constraint,而非讓重試/併發
-- PATCH 悄悄疊加第二筆沖銷。409 造成的交易回滾不會留下違規列(ROLLBACK
-- 撤銷 INSERT),不影響之後的重試。
--
-- 侷限(codex 抓到):全額折抵、結帳當下零點數流的訂單本來就不寫任何
-- checkout_earn/checkout_redeem 列,退款時 flow sums 讀回 0、對 0 幅度的
-- 反轉略過 ledger 寫入——這種單退款完全不會經過這個 index,沒有列可比
-- 對、也沒有列可護。此 index 只是「本來就有點數流的單」的後盾,不是普遍
-- 適用的補償標記(不能拿它當作「這張單是否已補償過」的通用判準)。
-- out-of-band 狀態回寫(繞過 service 直接改 DB)的殘餘風險記入 ADR-0007
-- (10h)——主防線始終是 `update_order_status` 的狀態機謂詞
-- (`can_transition_to`)+ orders.status 終態結構,這個 index 只是點數流
-- 有落地時的第二道防線。
-- =============================================================================

CREATE UNIQUE INDEX uniq_point_ledger_refund_once ON point_ledger(order_id, reason)
    WHERE reason IN ('refund_restore', 'refund_clawback');
