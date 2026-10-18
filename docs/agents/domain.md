# Domain Docs

How the engineering skills consume this repo's domain documentation. This is a **single-context**
repo: one `GLOSSARY.md` (the domain glossary) and one `docs/adr/` at the repo root.

## Where each kind of knowledge lives

Each kind has one home (ADR-0019). Before exploring an area, read what it needs from each:

- **Terms**: `GLOSSARY.md`. One or two sentences per term, naming its owner module.
- **Rules**: the owner module's doc (`//!` or file header): invariants, protocols, lock order, error
  precedence, write paths.
- **Why**: the ADRs in `docs/adr/` that touch the area. An ADR records a decision as it stood when it
  was made; older ones still name functions that have since moved, so read the code and module doc for
  how things work now.
- **Frontend agreement**: `docs/api/integration-contract.md`.

A change edits the home of what it changes: a rule change edits the code and the module doc;
`GLOSSARY.md` changes only when a term is added, renamed, or changes meaning; an ADR addendum
records only what the decision changed and why, with no migration list, call sites, or commit
hashes.

## Use the glossary's vocabulary

When your output names a domain concept (an issue title, a refactor proposal, a hypothesis, a test
name), use the term exactly as `GLOSSARY.md` defines it.

A concept missing from the glossary is a signal: either the language is invented (reconsider it) or
the glossary has a real gap (note it for `/grill-with-docs`).

## Flag ADR conflicts

When your output contradicts an existing ADR, say so explicitly and give the reason to reopen it:

> _Contradicts ADR-0002 — but worth reopening because…_
