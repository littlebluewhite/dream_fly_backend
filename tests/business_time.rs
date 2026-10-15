//! Business time (ADR-0017): a column that any read buckets, or compares
//! with the sampled instant, is business time, and every writer binds the
//! handler's `now` into it. Only audit time may take the DB clock. This test
//! owns which columns that is: `AUDIT_COLUMNS`.
//!
//! It scans every string literal in `src/**` and `tests/common/**` (raw
//! strings, `format!` and `QueryBuilder` fragments included; attributes and
//! doc comments skipped) for a DB-clock token: `now()`, `current_timestamp`,
//! `current_date`, `localtimestamp`, `clock_timestamp()`,
//! `statement_timestamp()`, `transaction_timestamp()`. Each hit is mapped
//! to the `table.column` it writes:
//! - `INSERT INTO t (cols…) VALUES (exprs…)`, position by position, CTEs
//!   included;
//! - `UPDATE t SET col = …` and `ON CONFLICT … DO UPDATE SET col = …`.
//!
//! The test fails when a mapped column is not in `AUDIT_COLUMNS`, when a hit
//! it cannot map (a `WHERE` predicate, an `INSERT … SELECT` source) is not in
//! `UNMAPPED`, or when an entry in either list no longer matches anything.
//! Promoting a column to business time = deleting it from `AUDIT_COLUMNS`;
//! the test then names every runtime, seed and fixture writer still on the
//! DB clock.
//!
//! Not covered: migration `DEFAULT`s and triggers, the `now()` inside
//! `subscription_derived_status` (W7-7), an `INSERT` that leaves a timestamp
//! column out (its `DEFAULT` fills it), and a bound value that is the wrong
//! instant.
//!
//! A second rule keeps the test harness on the scene instant: in
//! `tests/common/**`, `Utc::now`, `Local::now` and `SystemTime::now` may
//! appear only in the `fn`s `WALL_CLOCK_SITES` lists, each with a pinned
//! read count.

use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};

use proc_macro2::{Delimiter, TokenStream, TokenTree};

/// `table.column`s the DB clock may write (audit time), each with why.
/// `*.updated_at` matches every table.
const AUDIT_COLUMNS: &[(&str, &str)] = &[
    // Row-touch time; on every table but `settings` the BEFORE UPDATE trigger
    // `set_updated_at` rewrites it with NOW() whatever the SQL binds.
    ("*.updated_at", "row-touch time, trigger-written"),
    (
        "attendance_records.created_at",
        "row insert time; its business date is the session's",
    ),
    ("attendance_records.marked_at", "ADR-0017: audit this round"),
    (
        "audit_log.created_at",
        "consumption time, by design (kafka::consumer module doc)",
    ),
    (
        "bookings.created_at",
        "list order only; the seed binds a history instant",
    ),
    ("cart_items.created_at", "cart line order only"),
    (
        "certificates.created_at",
        "insert time; the certificate's date is the bound issued_on",
    ),
    (
        "clock_records.clock_in",
        "ADR-0017: clock_records are audit this round",
    ),
    (
        "clock_records.clock_out",
        "ADR-0017: clock_records are audit this round",
    ),
    (
        "clock_records.created_at",
        "ADR-0017: clock_records are audit this round",
    ),
    (
        "coach_schedules.created_at",
        "row insert time of a weekly availability slot",
    ),
    (
        "coaches.created_at",
        "coach list tiebreak and the profile's registered date; never bucketed",
    ),
    (
        "conversations.created_at",
        "ADR-0017: messages are audit this round",
    ),
    (
        "conversations.last_message_at",
        "ADR-0017: messages are audit this round",
    ),
    (
        "coupons.created_at",
        "admin list order; validity compares the bound expires_at",
    ),
    (
        "course_schedule_slots.created_at",
        "row insert time of a weekly schedule slot",
    ),
    (
        "course_sessions.created_at",
        "row insert time; the session's date is session_date",
    ),
    ("courses.created_at", "catalog row insert time"),
    (
        "events_outbox.next_attempt_at",
        "retry lease, written and compared on the DB clock only",
    ),
    (
        "events_outbox.published_at",
        "dispatch bookkeeping, read only as IS NULL",
    ),
    (
        "leave_requests.created_at",
        "leave list order; the leave's business date is its session",
    ),
    ("leave_requests.decided_at", "ADR-0017: audit this round"),
    (
        "messages.created_at",
        "ADR-0017: messages are audit this round",
    ),
    (
        "messages.read_at",
        "ADR-0017: messages are audit this round",
    ),
    (
        "notifications.created_at",
        "list order and display; runtime inserts take the column DEFAULT",
    ),
    (
        "order_idempotency.created_at",
        "when the key was recorded; never read",
    ),
    (
        "posts.created_at",
        "row insert time; posts order by published_at",
    ),
    (
        "posts.published_at",
        "only orders posts (confirmed audit time)",
    ),
    ("products.created_at", "catalog list order"),
    (
        "refresh_tokens.created_at",
        "token issue time; expiry is the bound expires_at",
    ),
    (
        "report_cards.created_at",
        "list order and display; the card's period is term_label",
    ),
    (
        "reward_redemptions.created_at",
        "history order; its points move is a bound point_ledger row",
    ),
    ("rewards.created_at", "catalog list tiebreak"),
    (
        "time_slots.created_at",
        "row insert time; the slot's time is date + start_time",
    ),
    (
        "users.last_login",
        "when the DB recorded the login; displayed, never bucketed or compared",
    ),
    ("venue_categories.created_at", "catalog row insert time"),
    ("venues.created_at", "catalog row insert time"),
    (
        "waitlist_entries.created_at",
        "ADR-0017: audit this round (FIFO order)",
    ),
];

/// DB-clock tokens the scan cannot map to a column, keyed by
/// `(file, substring of the SQL literal)` so unrelated edits don't make an
/// entry stale, with how many hits the entry covers — a further clock added
/// to an exempt statement fails instead of riding its exemption.
const UNMAPPED: &[(&str, &str, usize, &str)] = &[
    (
        "src/bin/seed/dataset.rs",
        "INSERT INTO attendance_records",
        1,
        "INSERT … SELECT FROM UNNEST: NOW() lands in attendance_records.created_at",
    ),
    (
        "src/kafka/outbox.rs",
        "next_attempt_at <= NOW()",
        1,
        "claim predicate: compares next_attempt_at with the DB clock that wrote it",
    ),
    (
        "src/modules/auth/session.rs",
        "expires_at < NOW()",
        1,
        "purge_expired: refresh-token retention sweep (token lifetimes are a clock-seam carve-out)",
    ),
    (
        "src/modules/schedule/repository.rs",
        "INSERT INTO time_slots",
        2,
        "UNNEST batch insert: ARRAY_FILL(now()) lands in time_slots.created_at/updated_at",
    ),
    (
        "src/modules/sessions/calendar.rs",
        "INSERT INTO course_sessions",
        1,
        "UNNEST batch insert: ARRAY_FILL(now()) lands in course_sessions.created_at",
    ),
];

#[test]
fn db_clock_is_written_only_into_audit_columns() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for dir in ["src", "tests/common"] {
        rust_files(&root.join(dir), &mut files);
    }
    let mut sites = Vec::new();
    for path in files {
        let file = slash_path(path.strip_prefix(root).unwrap());
        let text = fs::read_to_string(&path).unwrap();
        sites.extend(
            clock_sites(&text, &file)
                .into_iter()
                .map(|site| (file.clone(), site)),
        );
    }
    let problems = audit_problems(&sites, AUDIT_COLUMNS, UNMAPPED);
    assert!(
        problems.is_empty(),
        "DB clock outside the audit set (ADR-0017):\n{}\n\
         Business time binds the sampled `now`. Add an audit column to AUDIT_COLUMNS, or a site \
         the scan cannot map to UNMAPPED, with a reason; drop stale entries.",
        problems.join("\n")
    );
}

/// What `sites` (`(file, site)`) break against the two lists: a mapped
/// column not in `audit`, an unmapped hit no `unmapped` entry covers, an
/// `audit` entry that matches nothing, and an `unmapped` entry whose hit
/// count is not the one it pins.
fn audit_problems(
    sites: &[(String, ClockSite)],
    audit: &[(&str, &str)],
    unmapped: &[(&str, &str, usize, &str)],
) -> Vec<String> {
    let mut audit_seen = vec![false; audit.len()];
    let mut unmapped_hits = vec![0; unmapped.len()];
    let mut problems = Vec::new();
    for (file, site) in sites {
        let at = format!("{file}:{}", site.line);
        match &site.target {
            Some(col) => match audit.iter().position(|(c, _)| {
                c == col
                    || c.strip_prefix('*')
                        .is_some_and(|suffix| col.ends_with(suffix))
            }) {
                Some(i) => audit_seen[i] = true,
                None => problems.push(format!("  {at}  writes the DB clock into {col}")),
            },
            None => match unmapped
                .iter()
                .position(|(f, needle, _, _)| f == file && site.sql.contains(needle))
            {
                Some(i) => unmapped_hits[i] += 1,
                None => problems.push(format!("  {at}  DB clock the scan cannot map")),
            },
        }
    }
    for ((col, _), seen) in audit.iter().zip(audit_seen) {
        if !seen {
            problems.push(format!(
                "  stale AUDIT_COLUMNS entry {col}: nothing writes it"
            ));
        }
    }
    for ((file, needle, hits, _), found) in unmapped.iter().zip(unmapped_hits) {
        if found != *hits {
            problems.push(format!(
                "  UNMAPPED entry {file} {needle:?}: {found} DB-clock hits, expects {hits}"
            ));
        }
    }
    problems
}

/// `.rs` files under `dir`, sorted.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn slash_path(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// One DB-clock token in a string literal.
struct ClockSite {
    /// Line the literal starts on.
    line: usize,
    /// The `table.column` it writes; `None` when the scan cannot map it.
    target: Option<String>,
    /// The unescaped literal.
    sql: String,
}

/// Every DB-clock token in the string literals of the Rust source `text`,
/// mapped to the column it writes. Attributes, doc comments included, are
/// skipped.
fn clock_sites(text: &str, file: &str) -> Vec<ClockSite> {
    let stream: TokenStream = text
        .parse()
        .unwrap_or_else(|e| panic!("tokenize {file}: {e}"));
    let mut literals = Vec::new();
    string_literals(stream, &mut literals);
    let mut sites = Vec::new();
    for (line, sql) in literals {
        let toks = sql_tokens(&sql);
        let targets = write_targets(&toks);
        for i in (0..toks.len()).filter(|&i| is_db_clock(&toks, i)) {
            sites.push(ClockSite {
                line,
                target: targets[i].clone(),
                sql: sql.clone(),
            });
        }
    }
    sites
}

/// `(line, unescaped value)` of each string literal, macro bodies included.
fn string_literals(stream: TokenStream, out: &mut Vec<(usize, String)>) {
    let mut iter = stream.into_iter().peekable();
    while let Some(tt) = iter.next() {
        match tt {
            // `#[…]` / `#![…]`; doc comments reach us as `#[doc = "…"]`.
            TokenTree::Punct(p) if p.as_char() == '#' => {
                iter.next_if(|t| matches!(t, TokenTree::Punct(p) if p.as_char() == '!'));
                iter.next_if(
                    |t| matches!(t, TokenTree::Group(g) if g.delimiter() == Delimiter::Bracket),
                );
            }
            TokenTree::Group(g) => string_literals(g.stream(), out),
            TokenTree::Literal(lit) => {
                let line = lit.span().start().line;
                if let syn::Lit::Str(s) = syn::Lit::new(lit) {
                    out.push((line, s.value()));
                }
            }
            _ => {}
        }
    }
}

#[derive(PartialEq)]
enum Tok {
    /// Identifier, keyword or number, lowercased.
    Word(String),
    /// `(`, `[` or `CASE`.
    Open,
    /// `)`, `]` or `END`.
    Close,
    Comma,
    Eq,
    Other,
}

fn sql_tokens(sql: &str) -> Vec<Tok> {
    let mut toks = Vec::new();
    let mut chars = sql.chars().peekable();
    while let Some(c) = chars.next() {
        toks.push(match c {
            '(' | '[' => Tok::Open,
            ')' | ']' => Tok::Close,
            ',' => Tok::Comma,
            '=' => Tok::Eq,
            '\'' => {
                // A string constant: its commas and parens are not SQL's.
                chars.find(|&c| c == '\'');
                Tok::Other
            }
            c if c.is_alphanumeric() || c == '_' => {
                let mut word: String = c.to_lowercase().collect();
                while let Some(c) = chars.next_if(|c| c.is_alphanumeric() || *c == '_') {
                    word.extend(c.to_lowercase());
                }
                match word.as_str() {
                    "case" => Tok::Open,
                    "end" => Tok::Close,
                    _ => Tok::Word(word),
                }
            }
            c if c.is_whitespace() => continue,
            _ => Tok::Other,
        });
    }
    toks
}

fn word(toks: &[Tok], i: usize) -> &str {
    match toks.get(i) {
        Some(Tok::Word(w)) => w,
        _ => "",
    }
}

fn is_db_clock(toks: &[Tok], i: usize) -> bool {
    let call = toks.get(i + 1) == Some(&Tok::Open) && toks.get(i + 2) == Some(&Tok::Close);
    match word(toks, i) {
        "current_timestamp" | "current_date" | "localtimestamp" => true,
        "now" | "clock_timestamp" | "statement_timestamp" | "transaction_timestamp" => call,
        _ => false,
    }
}

/// For each token, the `table.column` whose written value it belongs to:
/// `INSERT INTO t (cols…) VALUES (exprs…)` position by position, and
/// `UPDATE t SET col = …` / `ON CONFLICT … DO UPDATE SET col = …`.
fn write_targets(toks: &[Tok]) -> Vec<Option<String>> {
    let mut targets = vec![None; toks.len()];
    let mut insert_table = None;
    for i in 0..toks.len() {
        if word(toks, i) == "insert" && word(toks, i + 1) == "into" {
            let table = word(toks, i + 2);
            insert_table = Some(table);
            // `INSERT INTO t AS x (…)`
            let cols = if word(toks, i + 3) == "as" {
                i + 5
            } else {
                i + 3
            };
            map_values(toks, cols, table, &mut targets);
        } else if word(toks, i) == "set" {
            let table = if i >= 2 && word(toks, i - 1) == "update" && word(toks, i - 2) == "do" {
                insert_table
            } else {
                // `UPDATE t SET`, `UPDATE t AS x SET`
                (i.saturating_sub(4)..i)
                    .rev()
                    .find(|&u| word(toks, u) == "update")
                    .map(|u| word(toks, u + 1))
            };
            if let Some(table) = table {
                map_set(toks, i + 1, table, &mut targets);
            }
        }
    }
    targets
}

/// `(cols…) VALUES (exprs…), …` starting at `open`.
fn map_values(toks: &[Tok], open: usize, table: &str, targets: &mut [Option<String>]) {
    if toks.get(open) != Some(&Tok::Open) {
        return;
    }
    let (cols, close) = group(toks, open);
    let cols: Vec<&str> = cols.into_iter().map(|c| word(toks, c.start)).collect();
    if word(toks, close + 1) != "values" {
        return; // `INSERT … SELECT`: not a position-by-position write
    }
    let mut open = close + 2;
    while toks.get(open) == Some(&Tok::Open) {
        let (exprs, close) = group(toks, open);
        for (expr, col) in exprs.into_iter().zip(&cols) {
            for t in expr {
                targets[t] = Some(format!("{table}.{col}"));
            }
        }
        if toks.get(close + 1) != Some(&Tok::Comma) {
            return;
        }
        open = close + 2;
    }
}

/// The comma-separated parts of the group opening at `open`, and the index
/// of its closing token.
fn group(toks: &[Tok], open: usize) -> (Vec<Range<usize>>, usize) {
    let mut depth = 0;
    let mut parts = Vec::new();
    let mut start = open + 1;
    for (i, tok) in toks.iter().enumerate().skip(open) {
        match tok {
            Tok::Open => depth += 1,
            Tok::Close => {
                depth -= 1;
                if depth == 0 {
                    parts.push(start..i);
                    return (parts, i);
                }
            }
            Tok::Comma if depth == 1 => {
                parts.push(start..i);
                start = i + 1;
            }
            _ => {}
        }
    }
    (parts, toks.len())
}

/// `col = expr, …` starting at `i`, up to `WHERE`/`FROM`/`RETURNING` or the
/// enclosing `)`.
fn map_set(toks: &[Tok], mut i: usize, table: &str, targets: &mut [Option<String>]) {
    while toks.get(i + 1) == Some(&Tok::Eq) && !word(toks, i).is_empty() {
        let target = format!("{table}.{}", word(toks, i));
        i += 2;
        let mut depth = 0;
        while let Some(tok) = toks.get(i) {
            match tok {
                Tok::Open => depth += 1,
                Tok::Close if depth == 0 => return,
                Tok::Close => depth -= 1,
                Tok::Comma if depth == 0 => break,
                Tok::Word(w)
                    if depth == 0 && matches!(w.as_str(), "where" | "from" | "returning") =>
                {
                    return;
                }
                _ => {}
            }
            targets[i] = Some(target.clone());
            i += 1;
        }
        i += 1;
    }
}

/// `"<line> <table.column>"` per hit in `sample`, `?` when unmapped.
fn hits(sample: &str) -> Vec<String> {
    clock_sites(sample, "sample")
        .iter()
        .map(|s| format!("{} {}", s.line, s.target.as_deref().unwrap_or("?")))
        .collect()
}

#[test]
fn scanner_maps_values_by_position() {
    let sample = r##"
fn f() {
    sqlx::query("INSERT INTO t (id, note, a, b) VALUES ($1, 'x, y', CASE WHEN $2 THEN NOW() ELSE NULL END, coalesce($3, now()))");
    sqlx::query("INSERT INTO u (id, created_at) VALUES ($1, now()), ($2, now())");
}
"##;
    assert_eq!(
        hits(sample),
        ["3 t.a", "3 t.b", "4 u.created_at", "4 u.created_at"]
    );
}

#[test]
fn scanner_reads_raw_strings_format_and_query_builder() {
    let sample = r##"
fn f() {
    sqlx::query(r#"
        INSERT INTO t (id, created_at)
        VALUES ($1, NOW())
    "#);
    sqlx::query(&format!("UPDATE t SET updated_at = now() WHERE id = {id}"));
    let qb = QueryBuilder::<Postgres>::new("UPDATE p SET updated_at = now()");
}
"##;
    assert_eq!(
        hits(sample),
        ["3 t.created_at", "7 t.updated_at", "8 p.updated_at"]
    );
}

#[test]
fn scanner_maps_cte_insert_and_set_assignments() {
    let sample = r##"
fn f() {
    sqlx::query("WITH ins AS (INSERT INTO t (id, made_at) VALUES ($1, NOW()) RETURNING id) SELECT id FROM ins");
    sqlx::query("INSERT INTO s (key, value, updated_at) VALUES ($1, $2, $3) ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = now()");
    sqlx::query("UPDATE q AS x SET tries = tries + 1, next_at = NOW() + $2 WHERE id = $1");
    sqlx::query("INSERT INTO w AS x (id, created_at) VALUES ($1, NOW())");
    sqlx::query("UPDATE v SET ok = CASE WHEN $1 IS DISTINCT FROM ok THEN false ELSE ok END, updated_at = NOW()");
}
"##;
    assert_eq!(
        hits(sample),
        [
            "3 t.made_at",
            "4 s.updated_at",
            "5 q.next_at",
            "6 w.created_at",
            "7 v.updated_at"
        ]
    );
}

#[test]
fn scanner_unescapes_line_continuations() {
    let sample = r##"
fn f() {
    sqlx::query("INSERT INTO t (id, created_at) \
                 VALUES ($1, NOW())");
}
"##;
    assert_eq!(hits(sample), ["3 t.created_at"]);
}

#[test]
fn scanner_skips_doc_comments_and_attributes() {
    let sample = r##"
//! UPDATE t SET a = NOW()
/// INSERT INTO t (id, b) VALUES ($1, NOW())
#[doc = "UPDATE t SET c = NOW()"]
/** UPDATE t SET d = NOW() */
// UPDATE t SET e = NOW()
fn f() {
    sqlx::query("UPDATE t SET kept = NOW()");
}
"##;
    assert_eq!(hits(sample), ["8 t.kept"]);
}

#[test]
fn scanner_leaves_predicates_and_select_sources_unmapped() {
    let sample = r##"
fn f() {
    sqlx::query("DELETE FROM r WHERE expires_at < NOW()");
    sqlx::query("INSERT INTO a (id, created_at) SELECT id, CURRENT_TIMESTAMP FROM b");
    sqlx::query("UPDATE q SET next_at = clock_timestamp() WHERE id IN (SELECT id FROM q WHERE next_at <= now())");
    sqlx::query(&format!("SELECT {now} FROM now_table"));
}
"##;
    assert_eq!(hits(sample), ["3 ?", "4 ?", "5 q.next_at", "5 ?"]);
}

#[test]
fn unmapped_entry_does_not_cover_a_second_clock_in_its_statement() {
    let sample = r##"
fn f() {
    sqlx::query("INSERT INTO s (id, made_on, created_at) SELECT id, CURRENT_DATE, now() FROM UNNEST($1::uuid[]) AS id");
}
"##;
    let sites: Vec<(String, ClockSite)> = clock_sites(sample, "sample")
        .into_iter()
        .map(|site| ("sample".to_owned(), site))
        .collect();
    let unmapped = [(
        "sample",
        "INSERT INTO s",
        1,
        "UNNEST batch insert: now() lands in s.created_at",
    )];
    assert_eq!(
        audit_problems(&sites, &[], &unmapped),
        ["  UNMAPPED entry sample \"INSERT INTO s\": 2 DB-clock hits, expects 1"]
    );
}

/// Where the test harness (`tests/common/**`) may read the wall clock
/// (`Utc::now`, `Local::now`, `SystemTime::now`), keyed by `(file, enclosing
/// fn)` with how many reads the entry covers and why. Everything else in the
/// harness takes the caller's instant (ADR-0017 Addendum 2026-10-15:
/// 測試場景時刻). `Instant::now` is not a business time and is not scanned.
const WALL_CLOCK_SITES: &[(&str, &str, usize, &str)] = &[
    (
        "tests/common/google.rs",
        "sign_id_token_with",
        1,
        "id_token iat/exp: the app's JWT validation checks exp against the real clock",
    ),
    (
        "tests/common/mod.rs",
        "scene_now",
        1,
        "the scene instant itself: the one sample a scene is set at",
    ),
    (
        "tests/common/mod.rs",
        "seed_member",
        1,
        "\"just joined\" default; users.created_at only moves new-member counts and list order",
    ),
];

#[test]
fn harness_reads_the_wall_clock_only_at_listed_sites() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rust_files(&root.join("tests/common"), &mut files);
    let mut reads = Vec::new();
    for path in files {
        let file = slash_path(path.strip_prefix(root).unwrap());
        let text = fs::read_to_string(&path).unwrap();
        reads.extend(
            wall_clock_reads(&text, &file)
                .into_iter()
                .map(|read| (file.clone(), read)),
        );
    }
    let problems = wall_clock_problems(&reads, WALL_CLOCK_SITES);
    assert!(
        problems.is_empty(),
        "wall clock read in the test harness outside WALL_CLOCK_SITES:\n{}\n\
         Take the caller's instant (`app.studio_now()`, `common::scene_now()`) as a parameter. \
         A read that must stay goes into WALL_CLOCK_SITES with a reason; fix stale counts.",
        problems.join("\n")
    );
}

/// One `Utc::now` / `Local::now` / `SystemTime::now` in Rust code.
struct WallClockRead {
    line: usize,
    /// The innermost named `fn` around it; empty outside any.
    func: String,
}

/// What `reads` (`(file, read)`) break against `allowed`: a read no entry
/// covers, and an entry whose read count is not the one it pins.
fn wall_clock_problems(
    reads: &[(String, WallClockRead)],
    allowed: &[(&str, &str, usize, &str)],
) -> Vec<String> {
    let mut found = vec![0; allowed.len()];
    let mut problems = Vec::new();
    for (file, read) in reads {
        match allowed
            .iter()
            .position(|(f, func, _, _)| f == file && *func == read.func)
        {
            Some(i) => found[i] += 1,
            None => problems.push(format!(
                "  {file}:{}  `{}` reads the wall clock",
                read.line, read.func
            )),
        }
    }
    for ((file, func, hits, _), found) in allowed.iter().zip(found) {
        if found != *hits {
            problems.push(format!(
                "  WALL_CLOCK_SITES entry {file} `{func}`: {found} wall-clock reads, expects {hits}"
            ));
        }
    }
    problems
}

/// Every wall-clock read in the Rust source `text`. Works on tokens, so
/// comments never match, nor string literals (doc comments reach the token
/// stream as `#[doc = "…"]` literals).
fn wall_clock_reads(text: &str, file: &str) -> Vec<WallClockRead> {
    let stream: TokenStream = text
        .parse()
        .unwrap_or_else(|e| panic!("tokenize {file}: {e}"));
    let mut out = Vec::new();
    wall_clock_reads_in(stream, "", &mut out);
    out
}

fn wall_clock_reads_in(stream: TokenStream, func: &str, out: &mut Vec<WallClockRead>) {
    let toks: Vec<TokenTree> = stream.into_iter().collect();
    let ident = |i: usize| match toks.get(i) {
        Some(TokenTree::Ident(id)) => id.to_string(),
        _ => String::new(),
    };
    let punct =
        |i: usize, c: char| matches!(toks.get(i), Some(TokenTree::Punct(p)) if p.as_char() == c);
    // The name of a `fn` whose body has not been reached yet.
    let mut pending: Option<String> = None;
    for (i, tt) in toks.iter().enumerate() {
        match tt {
            TokenTree::Group(g) => {
                let inner = match (&pending, g.delimiter()) {
                    (Some(name), Delimiter::Brace) => name.clone(),
                    _ => func.to_owned(),
                };
                if g.delimiter() == Delimiter::Brace {
                    pending = None;
                }
                wall_clock_reads_in(g.stream(), &inner, out);
            }
            TokenTree::Ident(id) if id == "fn" => {
                pending = Some(ident(i + 1)).filter(|n| !n.is_empty())
            }
            TokenTree::Ident(id)
                if matches!(id.to_string().as_str(), "Utc" | "Local" | "SystemTime")
                    && punct(i + 1, ':')
                    && punct(i + 2, ':')
                    && ident(i + 3) == "now" =>
            {
                out.push(WallClockRead {
                    line: id.span().start().line,
                    func: func.to_owned(),
                });
            }
            _ => {}
        }
    }
}

/// `"<line> <fn>"` per wall-clock read in `sample`.
fn wall_clock_hits(sample: &str) -> Vec<String> {
    wall_clock_reads(sample, "sample")
        .iter()
        .map(|r| format!("{} {}", r.line, r.func))
        .collect()
}

#[test]
fn wall_clock_matcher_finds_each_clock_and_names_its_fn() {
    let sample = r##"
fn a() -> DateTime<Utc> {
    let t = chrono::Utc::now();
    let d = Local::now().date_naive();
    std::time::SystemTime::now()
}
impl Seed {
    pub async fn b(&self) {
        self.at.unwrap_or_else(Utc::now);
        let f = |x: i64| -> i64 { x + Utc::now().timestamp() };
    }
}
fn outer() {
    fn inner() { Utc::now(); }
    Utc::now();
}
"##;
    assert_eq!(
        wall_clock_hits(sample),
        ["3 a", "4 a", "5 a", "9 b", "10 b", "14 inner", "15 outer"]
    );
}

#[test]
fn wall_clock_matcher_skips_comments_strings_and_other_nows() {
    let sample = r##"
//! Utc::now()
/// Seeded at `Utc::now()`.
#[doc = "Utc::now()"]
/** Local::now() */
// SystemTime::now()
/* Utc::now() */
fn f(g: fn() -> i64) {
    let deadline = std::time::Instant::now();
    let id = Uuid::now_v7();
    let now = app.studio_now().now;
    panic!("call Utc::now() yourself");
}
"##;
    assert!(wall_clock_hits(sample).is_empty());
}

#[test]
fn wall_clock_entry_pins_its_read_count() {
    let sample = r##"
fn listed() { Utc::now(); Utc::now(); }
fn unlisted() { Utc::now(); }
"##;
    let reads: Vec<(String, WallClockRead)> = wall_clock_reads(sample, "sample")
        .into_iter()
        .map(|read| ("sample".to_owned(), read))
        .collect();
    let allowed = [
        ("sample", "listed", 1, "one read"),
        ("sample", "gone", 1, "nothing reads here any more"),
    ];
    assert_eq!(
        wall_clock_problems(&reads, &allowed),
        [
            "  sample:3  `unlisted` reads the wall clock",
            "  WALL_CLOCK_SITES entry sample `listed`: 2 wall-clock reads, expects 1",
            "  WALL_CLOCK_SITES entry sample `gone`: 0 wall-clock reads, expects 1",
        ]
    );
}
