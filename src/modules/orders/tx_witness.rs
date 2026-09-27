//! Self-deadlock discipline for `service::assemble_response`, lifted out of
//! three cross-referencing comments into the type system. See `TxReleased`.
//!
//! A private file module of `orders` on purpose: `TxReleased`'s only field is
//! private, so one can be built solely from *inside this file*. Isolating the
//! type here means the code this discipline governs — `orders::service` and
//! `orders::idempotency` — cannot hand-write `TxReleased(())` to skip the
//! constructors; they must go through `release` / `commit` / `no_open_tx`.
//! (Same private-field witness technique as `courses::seats`'s `SessionLock`.)

use sqlx::{Postgres, Transaction};

/// Proof that the checkout / status-update transaction has already been
/// released back to the pool — rolled back via `release` or committed via
/// `commit` — *before* `assemble_response` runs.
///
/// `assemble_response` re-reads the order's items + artifacts through the
/// pool (`fetch_artifacts`). Issuing a pool query while a transaction
/// still holds its pooled connection self-deadlocks under a low-connection
/// pool: the query waits for a free connection, the only connection is not
/// freed until the transaction ends, and the transaction cannot end while
/// it is blocked on that query. `assemble_response` takes this witness *by
/// value*, so it cannot be reached without proof the tx is already gone —
/// the invariant now lives in the signature instead of in a comment every
/// caller has to remember.
///
/// Deliberately not `#[must_use]`: the witness is *permission* to call
/// `assemble_response`, not an obligation to do anything with it —
/// `idempotency::replay_by_key`'s `Ok(None)` arm (no committed row yet for
/// this key) legitimately drops a released witness unused.
///
/// Honest residual seam: `no_open_tx` is a caller-attested assertion, not
/// a machine-checked fact. The two read-only callers — checkout's pre-check
/// (`idempotency::replay`) and `get_order` — never open a transaction, so
/// there is nothing to release; the witness there records "this path holds
/// no open tx" on the caller's word. `release` and `commit` consume a real
/// `Transaction`, so those two are machine-checked.
pub(super) struct TxReleased(());

impl TxReleased {
    pub(super) fn release(tx: Transaction<'_, Postgres>) -> Self {
        drop(tx); // sqlx 對被 drop 的交易發 rollback,語意同原呼叫端
        Self(())
    }
    pub(super) async fn commit(tx: Transaction<'_, Postgres>) -> Result<Self, sqlx::Error> {
        tx.commit().await?;
        Ok(Self(()))
    }
    pub(super) fn no_open_tx() -> Self {
        Self(())
    }
}
