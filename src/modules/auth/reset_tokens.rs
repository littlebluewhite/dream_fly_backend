//! 密碼重設 token 的簽發/消費協定——`service::forgot_password` /
//! `service::reset_password` 下面那層純 Redis 讀寫:產生亂數 token、作廢該
//! 使用者前一個尚未過期的 token(讓「只有最新一封信裡的連結有效」這件事成
//! 立),以及單次消費用的原子 `GETDEL`。
//!
//! **明文留在 `service.rs`,不內化到這裡**:列舉防護的 early-return(找不
//! 到使用者仍回成功訊息)、`forgot_rate` 每帳號請求頻率檢查
//! (`rate_limit.rs` 才是那個原語的 owner,key 字串也由呼叫端持有)、SMTP
//! 背景派送(必須在 handler 返回前同步註冊進 `TaskTracker`,不能被搬進更
//! 深一層的 async 呼叫裡)、以及密碼更新 + refresh token family 撤銷那個交
//! 易。這個模組只回答「這個 token 是誰的」或「這個使用者的新 token 是什
//! 麼」,從不碰資料庫。

use redis::AsyncCommands;
use uuid::Uuid;

use crate::error::AppError;

/// 密碼重設 token 的存活秒數;需與信件文案裡的「15 minutes」一致。
const PASSWORD_RESET_TTL_SECONDS: i64 = 900;

/// `"password_reset:{token}"` —— 重設 token 本身的 Redis key。
fn token_key(token: &str) -> String {
    format!("password_reset:{token}")
}

/// `"password_reset_current:{user_id}"` —— 該使用者目前有效 token 的索引 key。
fn index_key(user_id: Uuid) -> String {
    format!("password_reset_current:{user_id}")
}

/// 為 `user_id` 簽發一個新的重設 token,回傳明文 token 供呼叫端組信件連
/// 結。該使用者前一個尚未過期的 token(如果有)會先被作廢——寄出第二封信
/// 會讓第一封信裡的連結立即失效。
pub(super) async fn issue(
    redis: &mut redis::aio::ConnectionManager,
    user_id: Uuid,
) -> Result<String, AppError> {
    // 1. 產生 URL-safe 亂數 token。URL_SAFE_NO_PAD 避免 `=`/`+`/`/` 被各家
    //    email client 不一致地做 URL 編碼。
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use rand::Rng;

    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    let token = URL_SAFE_NO_PAD.encode(bytes);

    // 2. 作廢該使用者前一個尚未過期的重設 token,確保只有最新連結有效。
    let idx_key = index_key(user_id);
    let previous: Option<String> = redis.get(&idx_key).await?;
    if let Some(prev_token) = previous {
        let _: () = redis.del(token_key(&prev_token)).await?;
    }

    // 3. 存入 token -> user_id,15 分鐘 TTL。
    let key = token_key(&token);
    redis::cmd("SET")
        .arg(&key)
        .arg(user_id.to_string())
        .arg("EX")
        .arg(PASSWORD_RESET_TTL_SECONDS)
        .query_async::<()>(redis)
        .await?;

    // 4. 記錄該使用者目前有效的 token,供下次 `issue` 呼叫時作廢。
    redis::cmd("SET")
        .arg(&idx_key)
        .arg(&token)
        .arg("EX")
        .arg(PASSWORD_RESET_TTL_SECONDS)
        .query_async::<()>(redis)
        .await?;

    Ok(token)
}

/// 原子消費一個重設 token:成功時回傳它所屬的 `user_id`,並清掉該使用者的
/// index key。`GETDEL` 在同一個 round-trip 內讀出舊值並刪除該 key,杜絕雙
/// 重使用的競態。Token 不存在(未曾簽發/已過期/已被消費過)回
/// `BadRequest`;讀到的值解析不出合法 `Uuid`(理論上不會發生,除非 Redis
/// 資料被外部竄改)回 `Internal`——這個分支刻意不清 index,是搬移前就有的
/// 現狀,逐字保留,不順手補上。
pub(super) async fn consume(
    redis: &mut redis::aio::ConnectionManager,
    token: &str,
) -> Result<Uuid, AppError> {
    // 1. 原子消費 token:GETDEL 在同一個 round-trip 內讀出舊值並刪除該
    //    key,杜絕雙重使用的競態。
    let key = token_key(token);
    let user_id_str: Option<String> = redis::cmd("GETDEL").arg(&key).query_async(redis).await?;

    let user_id_str =
        user_id_str.ok_or_else(|| AppError::BadRequest("invalid or expired token".into()))?;

    let user_id: Uuid = user_id_str
        .parse()
        .map_err(|_| AppError::Internal(anyhow::anyhow!("invalid user_id in reset token")))?;

    // 2. 同時清掉該使用者的「目前有效 token」index。
    let _: () = redis.del::<_, ()>(index_key(user_id)).await?;

    Ok(user_id)
}
