# Domain Docs

How the engineering skills consume this repo's domain documentation. This is a **single-context**
repo: one `CONTEXT.md` (the domain glossary) and one `docs/adr/` at the repo root.

## Before exploring

Read `CONTEXT.md` and the ADRs in `docs/adr/` that touch the area you're about to work in.

## Use the glossary's vocabulary

When your output names a domain concept (an issue title, a refactor proposal, a hypothesis, a test
name), use the term exactly as `CONTEXT.md` defines it.

A concept missing from the glossary is a signal: either the language is invented (reconsider it) or
the glossary has a real gap (note it for `/grill-with-docs`).

## Flag ADR conflicts

When your output contradicts an existing ADR, say so explicitly and give the reason to reopen it:

> _Contradicts ADR-0002 — but worth reopening because…_
