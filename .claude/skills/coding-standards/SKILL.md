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

## Guard tests
- [ ] **Pinned allowlists**: an exemption entry in a scanner test (e.g. `UNMAPPED` in `tests/business_time.rs`) pins its expected hit count, so a new hit at the same site fails the test instead of riding the exemption.

## Domain safety
- [ ] If domain logic changed, the relevant `docs/adr/` was checked and any conflict with an ADR invariant is surfaced; terms match `GLOSSARY.md`.
- [ ] Endpoint path, auth, request, or response changes match `docs/api/integration-contract.md`, and a DTO change regenerated `bindings/`.
