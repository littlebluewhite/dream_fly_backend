//! Google 連結決策 (Account-Linking Decision) — `google_auth`(`auth::service`)
//! 在完成 HTTP 換權杖 + JWKS 簽章驗證(步驟 1-2)後,依「以 google_id 查到的
//! 既有使用者」與「以 email 查到的既有使用者」兩個 `Option<&User>`,決定要
//! Create 一個全新使用者、把 Google 帳號 Link 到既有密碼帳號,還是單純
//! Refresh 既有 Google 使用者的 profile——連同兩個驅動 event/歡迎通知發送的
//! 布林旗標。純函式,零 DB、零 async,同 `orders::pricing`/
//! `orders::fulfilment`/`attendance::marking`/`messages::pairing`的形狀:
//! `service::google_auth`仍擁有兩次查詢本身(google_id 路走 pool、email 路
//! 惰性——只在 google miss 時才查,且落在 tx 內)與其後的 repo 呼叫、role
//! 指派、session 簽發、tx commit、event/通知投遞——那些都是 DB 協調,不是
//! 連結決策。
//!
//! **輸入刻意不含 claims。** `plan()`只讀兩個 `Option`的「形狀」(命中/未命
//! 中,以及命中時 email 側的 `google_id`是否已綁)決策,不比對 google_id 的
//! 「值」——現行 Conflict 分支本來就不比較 claims.sub 與既有 google_id 是否
//! 相等,只要 email 帳號已綁「任一」google_id 即 409。輸入不含 claims 讓
//! 「比對 sub 值」這件事在型別上不可能發生,不必靠 review 或測試守住。
//!
//! **兩個布林刻意物化,不從 `action` 推導。** `emit_registered_event`/
//! `send_welcome`理論上可以從 `action`算出來,但把它們攤平成獨立欄位正是要
//! 釘住這條規則本身——這是要保留的產品行為,不是疏漏:
//! - `emit_registered_event`(舊稱 `!existed`)——Create *與* Link 都發
//!   `user_registered`事件,只有 Refresh 不發。
//! - `send_welcome`(舊稱 `created_new_user`)——只有 Create 發歡迎通知;
//!   Link 是把 Google 綁上一個「已經在密碼註冊時收過歡迎通知」的既有帳號,
//!   不能重發(`!existed`當年不能拿來當「新使用者」的代理,正是因為它也涵
//!   蓋了 Link 這個分支)。
//!
//! 真值表(5 列,對應同名單元測試):
//! 1. **google 命中**——不論 email 有沒有查(見查詢惰性說明)——一律
//!    `Refresh` / 兩旗標皆 `false`。
//! 2. **google 未命中、email 命中且已綁任一 google_id**——`Err(Conflict)`,
//!    訊息逐字同現行(`"email already associated with another account"`)。
//! 3. **google 未命中、email 命中但未綁**——`Link { user_id }` / event
//!    `true` / welcome `false`。
//! 4. **兩者皆未命中**——`Create` / 兩旗標皆 `true`。
//! 5. **兩者皆命中(防禦列)**——`service::google_auth`惰性查詢的實際呼叫方
//!    式下不會發生(google 命中時根本不會去查 email),但 `plan()`本身仍需
//!    對這個組合防禦性地正確:google 優先,等同第 1 列的 `Refresh`。

use uuid::Uuid;

use crate::error::AppError;

use super::model::User;

/// `google_auth`的三選一結果。見模組文件的真值表。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkAction {
    Create,
    Link { user_id: Uuid },
    Refresh,
}

/// `plan()`的完整輸出——`service::google_auth`依 `action` match 到既有的
/// repo 函式,再用兩個旗標驅動 event/歡迎通知的發送。
#[derive(Debug)]
pub struct LinkPlan {
    pub action: LinkAction,
    /// 舊稱 `!existed`(service.rs:314)——Create 與 Link 皆為 `true`,只有
    /// Refresh 為 `false`。
    pub emit_registered_event: bool,
    /// 舊稱 `created_new_user`(service.rs:336)——只有 Create 為 `true`。
    pub send_welcome: bool,
}

/// 決定 google_auth 的連結動作。`existing_by_google`/`existing_by_email`是
/// 呼叫端已完成的兩次查詢結果——兩者的「形狀」(命中與否,以及 email 側是
/// 否已綁定任一 google_id)是這個純函式唯一讀取的輸入,不讀 claims,見模組
/// 文件「輸入刻意不含 claims」一節。
pub fn plan(
    existing_by_google: Option<&User>,
    existing_by_email: Option<&User>,
) -> Result<LinkPlan, AppError> {
    // google_id 命中——雙命中防禦列也在這裡收斂:google 優先。
    if existing_by_google.is_some() {
        return Ok(LinkPlan {
            action: LinkAction::Refresh,
            emit_registered_event: false,
            send_welcome: false,
        });
    }

    match existing_by_email {
        Some(user) if user.google_id.is_some() => Err(AppError::Conflict(
            "email already associated with another account".into(),
        )),
        Some(user) => Ok(LinkPlan {
            action: LinkAction::Link { user_id: user.id },
            emit_registered_event: true,
            send_welcome: false,
        }),
        None => Ok(LinkPlan {
            action: LinkAction::Create,
            emit_registered_event: true,
            send_welcome: true,
        }),
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    /// A minimal `User` fixture — only `id`/`google_id` vary across the
    /// truth-table rows below, every other field is a fixed placeholder.
    fn fixture_user(id: Uuid, google_id: Option<&str>) -> User {
        User {
            id,
            email: "fixture@example.com".to_string(),
            name: "Fixture User".to_string(),
            phone: None,
            phone_verified: false,
            avatar_url: None,
            password_hash: None,
            google_id: google_id.map(|s| s.to_string()),
            is_active: true,
            last_login: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            points_balance: 0,
            preferences: None,
            birth_date: None,
        }
    }

    // --- 真值表 5 列 ---

    #[test]
    fn google_hit_refreshes_with_both_flags_false() {
        // google_auth_refetches_jwks_on_kid_rotation phase 2
        // (tests/http_auth.rs): the same google-sub logging in a second time
        // is the only existing exercise of a returning Google user, albeit
        // incidental to that test's real purpose (JWKS kid-rotation) — it is
        // what makes phase 2's success possible at all.
        let google_user = fixture_user(Uuid::now_v7(), Some("google-sub-1"));
        let plan = plan(Some(&google_user), None).expect("plans");
        assert_eq!(plan.action, LinkAction::Refresh);
        assert!(!plan.emit_registered_event);
        assert!(!plan.send_welcome);
    }

    #[test]
    fn email_hit_already_linked_is_conflict() {
        // No existing test anywhere (http or unit) exercises this branch —
        // this is its first coverage, per the module doc.
        let existing = fixture_user(Uuid::now_v7(), Some("other-google-sub"));
        let err = plan(None, Some(&existing)).expect_err("must reject");
        assert!(
            matches!(
                err,
                AppError::Conflict(ref m) if m == "email already associated with another account"
            ),
            "got: {err:?}"
        );
    }

    #[test]
    fn email_hit_unlinked_links_with_event_but_no_welcome() {
        // google_auth_linking_existing_account_does_not_resend_welcome
        // (tests/http_auth.rs): a password account with no google_id gets
        // linked, not created — the event fires, the welcome does not
        // (already sent at register time).
        let existing = fixture_user(Uuid::now_v7(), None);
        let plan = plan(None, Some(&existing)).expect("plans");
        assert_eq!(
            plan.action,
            LinkAction::Link {
                user_id: existing.id
            }
        );
        assert!(plan.emit_registered_event);
        assert!(!plan.send_welcome);
    }

    #[test]
    fn double_miss_creates_with_both_flags_true() {
        // google_auth_new_user_gets_welcome_notification (tests/http_auth.rs)
        // + phase 1 of google_auth_refetches_jwks_on_kid_rotation: neither
        // lookup hits — a genuinely brand-new Google user.
        let plan = plan(None, None).expect("plans");
        assert_eq!(plan.action, LinkAction::Create);
        assert!(plan.emit_registered_event);
        assert!(plan.send_welcome);
    }

    #[test]
    fn double_hit_google_wins_over_email_defensively() {
        // Not reachable through service::google_auth's actual call pattern —
        // the email lookup is lazy and only runs on a google miss (see the
        // module doc), so the two Options are never both Some in practice.
        // This pins plan()'s own defensive correctness if it were ever
        // called directly with both hits — constructed, not sourced from an
        // integration test (mirrors orders::pricing's boundary cases).
        let google_user = fixture_user(Uuid::now_v7(), Some("google-sub-2"));
        let email_user = fixture_user(Uuid::now_v7(), None);
        let plan = plan(Some(&google_user), Some(&email_user)).expect("plans");
        assert_eq!(plan.action, LinkAction::Refresh);
        assert!(!plan.emit_registered_event);
        assert!(!plan.send_welcome);
    }
}
