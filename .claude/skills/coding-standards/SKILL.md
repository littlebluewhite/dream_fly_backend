---
name: coding-standards
description: Backend review checklist. Use when reviewing a diff or PR in this repo.
---

# Review checklist

Conventions (module layout, DB patterns, studio time, seams) live in `AGENTS.md`; this file holds the review gates. Apply each item to the diff; it is a pass/fail gate, so cite `file:line` for any failure. `cargo fmt --check` and `cargo clippy -D warnings` already run in pre-commit; spend the review on what they cannot see.

## Ledgers and running balances
- [ ] **Chain test**: a change that writes or lists `point_ledger` (or any row that carries a running balance) has a test that writes rows out of time order, walks the listed chain across pages, and asserts each row's balance equals the previous row's plus its delta, e.g. `ledger_lists_a_chain_from_the_balance_whatever_the_write_order` in `tests/service_points.rs` (`balance_after` is derived at read time, ADR-0018). Writing a compensation (refund, clawback) out of time order breaks a stored running balance while every per-row assertion still passes.

## Idempotent reruns
- [ ] **Rerun after drift**: code that must be safe to run twice (the seed, retried jobs, `Idempotency-Key` paths) is tested by rerunning it after the state was moved by a normal app flow in between (an order cancelled, shipped, or refunded), not only on an untouched database.

## How tests assert
Applies to assertions and setup the diff adds or changes. Older raw SQL is switched when its test changes for another reason; no sweep.
- [ ] **Domain facts through the owner**: an assertion about a domain fact (an order's points ledger, a leave request's status, a user's roles, an order's status) reads it through the owning module's read (e.g. `points::service::get_my_points`, `permissions::repository::find_role_names_by_user`, `orders::service::get_order`) or over HTTP, not from its table. A table read pins the schema and can keep passing after the model it describes is gone.
- [ ] **Raw SQL only where the table is the property**: partial unique indexes and constraints, row locks, the `enum_range` probe, studio SQL functions, hash-at-rest, outbox rows no owner reads, database-wide totals.
- [ ] **Bypass setup says what it bypasses**: raw setup SQL (a bypass fixture) names the owner it skips, as the fixtures in `tests/common/fixtures.rs` do. If it builds a state no owner produces, it also says why the test needs that state.

## Guard tests
- [ ] **Pinned allowlists**: an exemption entry in a scanner test (e.g. `UNMAPPED` in `tests/business_time.rs`) pins its expected hit count, so a new hit at the same site fails the test instead of riding the exemption.

## GLOSSARY and module docs
- [ ] **GLOSSARY diffs change only definitions and `_Avoid_`**: an edit to `GLOSSARY.md` adds or renames a term, or rewords its definition or `_Avoid_` line. Rules, protocols, function names and test names belong in the owner module's doc (ADR-0019); `tests/glossary.rs` checks each entry's shape.
- [ ] **Rule changes land in the module doc**: a diff that changes a rule (an invariant, a lock order, an error precedence, a write path) updates the owner module's doc (`//!` or file header) in the same diff.

## Cross-module tables
- [ ] **Reads open, writes owned** (跨模組讀表): a module may SELECT or JOIN another module's table directly instead of going through that module's repository, but every write to the table goes through its owner. An open read does not license rebuilding a historical fact from another module's current rows: a value that must stay as it was is snapshotted when it is written (see 場次物化 in `GLOSSARY.md`).

## Domain safety
- [ ] If domain logic changed, the relevant `docs/adr/` was checked and any conflict with an ADR invariant is surfaced; terms match `GLOSSARY.md`.
- [ ] Endpoint path, auth, request, or response changes match `docs/api/integration-contract.md`, and a DTO change regenerated `bindings/`.
