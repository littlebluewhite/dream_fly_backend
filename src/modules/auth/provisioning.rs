//! 帳號誕生 owner——`create_account` 是「`INSERT users` + 指派 `member` 角色 +
//! 排入 `user_registered` outbox 事件」三步驟的唯一 owner。兩個真正的帳號
//! 誕生呼叫端(`auth::service::register`、`users::service::create_user`)換
//! 線於此,行為(誰能建、建出來的列長什麼樣)絲毫不變,只是把三步驟從各自
//! 手寫收斂成一次呼叫。
//!
//! **兩個誕生點刻意不納入這個 owner**,理由記錄於此(而非留給呼叫端猜):
//!
//! 1. **`google_auth` 的 Create 分支**(`auth::repository::
//!    create_or_update_google_user_tx`)——它的
//!    `INSERT ... ON CONFLICT (google_id) DO UPDATE` 是併發首次登入的防
//!    線:同一 `google_id` 兩個請求同時到達時,後到的那個要嘛更新既有列、
//!    要嘛不能整個 500。換成 `create_account` 內裸的 `INSERT`(沒有
//!    `ON CONFLICT`)會讓這個 race 從「安全 upsert」退化成「唯一鍵違例
//!    500」。此外 `linking::plan` 的 `emit_registered_event` 旗標橫跨
//!    Create *與* Link 兩個分支(見該模組文件的真值表)——事件補發是
//!    Create/Link 共用的判斷,不是「新建帳號」獨有,把 Create 半條腿拆進
//!    `create_account` 會把這個橫跨兩分支的判斷切開,違反 `linking`
//!    模組存在的目的。google 的角色指派已在 Phase 6A 收斂到
//!    `permissions::assign_role_by_name`,不需要再靠這裡收攏一次。
//!
//! 2. **`bin/seed.rs` 的開發種子資料**——`upsert_user`/`upsert_seed_member`
//!    都是冪等 upsert(`ON CONFLICT DO NOTHING` + 條件式二次 `SELECT`),
//!    不是一次性誕生;種子帳號的角色因人而異(admin 帳完全沒有 `member`
//!    角色),且 `points_balance` 是顯式種子輸入,不是「新帳號預設 0」的
//!    誕生不變量。更根本的是:`create_account` 無條件寫入
//!    `user_registered` outbox 事件——讓 dev seed 的每筆種子使用者每次重
//!    跑都各自產生一筆事件,是這個函式完全不該有的副作用。
//!
//! 呼叫端仍各自擁有:密碼雜湊(Argon2 需在 blocking thread 執行,不可搬進
//! tx 內)、session 簽發/歡迎通知,以及各自已公告的重複 email 錯誤文案
//! (`create_account` 只回傳裸 `sqlx::Error`,由呼叫端各自經
//! `AppError::conflict_on_unique` 映射——`register` 是
//! `"registration failed"`,`users::service::create_user` 是
//! `"Email 已被使用"`,兩者是不同的已公告契約,刻意不在此處統一)。

use chrono::NaiveDate;
use sqlx::{Postgres, Transaction};

use crate::extractors::auth::RoleCacheDirty;
use crate::kafka::events::UserRegisteredPayload;
use crate::kafka::outbox;
use crate::modules::permissions::repository as permissions_repository;

use super::model::{normalize_email, User};
use super::repository;

/// Input to [`create_account`]. The caller already holds a hashed password
/// (Argon2 hashing is blocking CPU work and must never run inside an open
/// transaction) and has decided what `phone`/`birth_date` to write, if any —
/// `register` always passes `None`/`None`; the admin-creation endpoint may
/// supply both.
pub struct NewAccount<'a> {
    pub email: &'a str,
    pub name: &'a str,
    pub phone: Option<&'a str>,
    pub birth_date: Option<NaiveDate>,
    pub password_hash: &'a str,
}

/// The freshly-created row plus the [`RoleCacheDirty`] witness from its
/// `member`-role grant. `#[must_use]`, same discipline as `RoleCacheDirty`
/// itself: forgetting to carry `dirty` out to a post-commit `.flush(redis)`
/// should not compile away silently.
#[must_use]
pub struct ProvisionedAccount {
    pub user: User,
    pub dirty: RoleCacheDirty,
}

/// The account-birth owner: one atomic INSERT + role grant + outbox event,
/// shared by every real (non-upsert) account-creation call site —
/// `auth::service::register` and `users::service::create_user`. Four fixed
/// steps, always in this order:
/// 1. normalize the email (case-insensitive identity — a birth invariant,
///    not a per-caller choice)
/// 2. insert the `users` row (`repository::create_user_tx`, absorbed from
///    the former `users::repository::create_user_tx` — same 10-column SQL)
/// 3. assign the `member` role (hardcoded — every account born through this
///    owner gets exactly this role; `coach`/`admin` are granted by a
///    separate, later action, never at creation time)
/// 4. queue a `user_registered` outbox event, unconditionally — unlike
///    `google_auth`'s Create/Link split (see `linking`'s module doc), there
///    is no branch here where the event should NOT fire
///
/// Takes `&mut Transaction` (not a plain connection) because steps 2-4 must
/// commit atomically or not at all. Returns a bare `sqlx::Error` rather than
/// `AppError`: see the module doc for why the unique-violation-to-message
/// translation stays at the call site instead of being decided here.
pub async fn create_account(
    tx: &mut Transaction<'_, Postgres>,
    account: NewAccount<'_>,
    correlation_id: Option<String>,
) -> Result<ProvisionedAccount, sqlx::Error> {
    let email = normalize_email(account.email);

    let user = repository::create_user_tx(
        tx,
        &email,
        account.name,
        account.phone,
        account.password_hash,
        account.birth_date,
    )
    .await?;

    let dirty = permissions_repository::assign_role_by_name(&mut **tx, user.id, "member").await?;

    outbox::insert_domain_event_tx(
        tx,
        UserRegisteredPayload {
            user_id: user.id,
            email: user.email.clone(),
            name: user.name.clone(),
        },
        correlation_id,
    )
    .await?;

    Ok(ProvisionedAccount { user, dirty })
}
