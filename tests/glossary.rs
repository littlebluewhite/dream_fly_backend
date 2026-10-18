//! GLOSSARY.md holds definitions, not rules. An entry is the term in bold
//! (`**Term**:`), one or two sentences of what it is, naming its owner
//! module, and one `_Avoid_:` line. Rules, protocols and test names live in
//! the owner module's doc; history lives in git and the ADRs.
//!
//! This test reads GLOSSARY.md from the crate root and checks every entry:
//! - the definition (every line but the term and the `_Avoid_` line) is at
//!   most `MAX_DEFINITION_CHARS` characters;
//! - the whole entry, term and `_Avoid_` included, has at most
//!   `MAX_CODE_SPANS` code spans;
//! - it has an `_Avoid_` line;
//! - no word is a source path (`src/…`, `bin/…`, see `PATH_ROOTS`), a file
//!   name (`FILE_EXTENSIONS`), a line number (`:120`, `L120`), a commit hash
//!   or a round number (`R22`). ADR numbers are fine.
//!
//! An entry starts at a line `**Term**:` and runs to the next one or to a
//! heading; its name is the term up to its first `(`. Samples that must pass
//! and must fail run alongside the real file, and `MIN_ENTRIES` keeps a parse
//! that finds nothing from passing.
//!
//! `PENDING` is temporary: the entries not shortened yet, skipped by the
//! checks. It is the only exemption. A listed entry that passes, or that the
//! glossary no longer has, fails the test.

use std::fs;
use std::path::Path;

/// Entries not shortened yet, by name: one block per ticket, so the tickets
/// can edit their blocks in parallel. Skipped by the checks; an entry here
/// that already passes fails the test, so it leaves the list when it is
/// shortened.
#[rustfmt::skip]
const PENDING: &[&str] = &[

    // R23-09: accounts, authorization, infrastructure
    "教練範圍",
    "課程教練所有權",
    "授權閘門",
    "Session 生命週期",
    "憑證檢查",
    "恆定成本",
    "帳號存取",
    "短期狀態",
    "帳號誕生",
    "Google 身分",
    "健康探針",
    "AppState 組裝",
    "唯一鍵衝突映射",
    "跨模組讀表",
    "執行環境",
    "事件派送",
    "消費迴圈",
    "Notification",
    "Wire enum 值域",

    // R23-10: courses, sessions, leave, rental, reports, clock
    "留存",
    "漏斗",
    "場租",
    "場租佔位",
    "試上預約",
    "系統設定",
    "場次狀態",
    "座位",
    "出席口徑",
    "點名計畫",
    "請假",
    "請假規則",
    "請假讀取投影",
    "讀取投影慣例",
    "場次物化",
    "有效報名",
    "候補",
    "週課表",
    "時段狀態",
    "對話配對",
    "studio 時間軸 SQL 攣生",
    "報表組裝",
    "工作室時鐘",
    "時鐘 seam",
    "場景時刻",
];

/// Fewer entries than this means the parse broke (a format change would
/// otherwise pass by finding nothing). Lower it only when terms are deleted.
const MIN_ENTRIES: usize = 60;

#[test]
fn glossary_entries_are_definitions() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("GLOSSARY.md");
    let entries = entries(&fs::read_to_string(path).unwrap());
    assert!(
        entries.len() >= MIN_ENTRIES,
        "GLOSSARY.md parsed into {} entries, expected at least {MIN_ENTRIES}: an entry starts \
         at a line `**Term**:`",
        entries.len()
    );
    let problems = glossary_problems(&entries, PENDING);
    assert!(
        problems.is_empty(),
        "GLOSSARY.md entries that are not definitions:\n{}\n\
         An entry is `**Term**:`, one or two sentences of what it is (owner module by name), and \
         one `_Avoid_:` line. Move rules, protocols and test names into the owner module's doc; \
         split a term that does not fit. Paths, file names, line numbers, commits and rounds \
         are history: delete them (ADR numbers are fine).",
        problems.join("\n")
    );
}

/// One `**Term**:` entry of the glossary.
struct Entry {
    /// The bold term up to its first `(`: `工作室時鐘(Studio Clock)` is
    /// `工作室時鐘`.
    name: String,
    /// Line the term starts on.
    line: usize,
    /// Every line but the term and the `_Avoid_` line, trimmed, joined by
    /// spaces.
    definition: String,
    has_avoid: bool,
    /// The whole entry, term line included.
    text: String,
}

/// Every entry in `glossary`. An entry starts at a line `**Term**:` and runs
/// to the next one or to a heading.
fn entries(glossary: &str) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    let mut open = false;
    for (i, line) in glossary.lines().enumerate() {
        if let Some((term, rest)) = term_line(line) {
            out.push(Entry {
                name: term.split(['(', '（']).next().unwrap().trim().to_owned(),
                line: i + 1,
                definition: rest.trim().to_owned(),
                has_avoid: false,
                text: line.to_owned(),
            });
            open = true;
            continue;
        }
        if line.starts_with('#') {
            open = false;
        }
        let Some(entry) = out.last_mut().filter(|_| open) else {
            continue;
        };
        entry.text.push('\n');
        entry.text.push_str(line);
        let line = line.trim();
        if line.starts_with("_Avoid_") {
            entry.has_avoid = true;
        } else if !line.is_empty() {
            if !entry.definition.is_empty() {
                entry.definition.push(' ');
            }
            entry.definition.push_str(line);
        }
    }
    for entry in &mut out {
        entry.text.truncate(entry.text.trim_end().len());
    }
    out
}

const MAX_DEFINITION_CHARS: usize = 200;
const MAX_CODE_SPANS: usize = 2;
/// A word starting with one of these and a `/` is a path.
const PATH_ROOTS: &[&str] = &[
    "src",
    "tests",
    "migrations",
    "docs",
    "config",
    "scripts",
    "bindings",
    "bin",
    "modules",
];
const FILE_EXTENSIONS: &[&str] = &[
    "rs", "sql", "toml", "md", "ts", "js", "json", "sh", "yml", "yaml",
];

/// What `entry` breaks, in a fixed order: length, code spans, `_Avoid_`,
/// then each offending word as it appears.
fn entry_problems(entry: &Entry) -> Vec<String> {
    let mut problems = Vec::new();
    let chars = entry.definition.chars().count();
    if chars > MAX_DEFINITION_CHARS {
        problems.push(format!("definition {chars} chars > {MAX_DEFINITION_CHARS}"));
    }
    let spans = entry.text.matches('`').count() / 2;
    if spans > MAX_CODE_SPANS {
        problems.push(format!("{spans} code spans > {MAX_CODE_SPANS}"));
    }
    if !entry.has_avoid {
        problems.push("no _Avoid_ line".to_owned());
    }
    for word in ascii_words(&entry.text) {
        if let Some(kind) = word_problem(word) {
            let problem = format!("{kind} {word}");
            if !problems.contains(&problem) {
                problems.push(problem);
            }
        }
    }
    problems
}

/// Runs of ASCII letters, digits and `_./:-`, without trailing `.`, `:`
/// or `-`.
fn ascii_words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || "_./:-".contains(c)))
        .map(|w| w.trim_end_matches(['.', ':', '-']))
        .filter(|w| !w.is_empty())
}

/// The kind of history or implementation detail `word` is, if any.
fn word_problem(word: &str) -> Option<&'static str> {
    if word
        .split_once('/')
        .is_some_and(|(root, _)| PATH_ROOTS.contains(&root))
    {
        return Some("path");
    }
    let file = word.split(':').next().unwrap();
    if file
        .rsplit_once('.')
        .is_some_and(|(stem, ext)| !stem.is_empty() && FILE_EXTENSIONS.contains(&ext))
    {
        return Some("file");
    }
    let after_colon = word.strip_prefix(':').is_some_and(|n| {
        n.split('-')
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    });
    let parts = || word.split(['.', '/', ':', '-']);
    if after_colon || parts().any(|p| is_numbered(p, 'L')) {
        return Some("line number");
    }
    if parts().any(is_commit_hash) {
        return Some("commit");
    }
    if parts().any(|p| is_numbered(p, 'R')) {
        return Some("round");
    }
    None
}

/// `prefix` followed by digits only: `L395`, `R22`.
fn is_numbered(part: &str, prefix: char) -> bool {
    part.strip_prefix(prefix)
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// 7 to 40 lowercase hex digits, mixing digits and letters: `d819e21`.
fn is_commit_hash(part: &str) -> bool {
    (7..=40).contains(&part.len())
        && part.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        && part.bytes().any(|b| b.is_ascii_digit())
        && part.bytes().any(|b| b.is_ascii_alphabetic())
}

/// One line per entry that breaks a rule, then one per `pending` name
/// that names no entry. A `pending` entry is not checked, but passing every
/// check is a problem: it has been shortened and should leave the list.
fn glossary_problems(entries: &[Entry], pending: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for entry in entries {
        let problems = entry_problems(entry);
        let at = format!("  「{}」 line {}", entry.name, entry.line);
        if pending.contains(&entry.name.as_str()) {
            if problems.is_empty() {
                out.push(format!("{at}: passes now, delete it from PENDING"));
            }
        } else if !problems.is_empty() {
            out.push(format!("{at}: {}", problems.join("; ")));
        }
    }
    for name in pending {
        if !entries.iter().any(|e| e.name == *name) {
            out.push(format!(
                "  PENDING lists 「{name}」, which GLOSSARY.md does not have: delete it or fix \
                 the name"
            ));
        }
    }
    out
}

/// `(term, rest of the line)` when `line` is `**term**:rest` (`:` or `：`).
fn term_line(line: &str) -> Option<(&str, &str)> {
    let body = line.strip_prefix("**")?;
    let (term, rest) = body.split_once("**")?;
    let rest = rest.strip_prefix(':').or_else(|| rest.strip_prefix('：'))?;
    Some((term, rest))
}

#[test]
fn scanner_splits_entries_at_terms_and_headings() {
    let sample = "\
# Title

Intro **not**: an entry.

## Language

**工作室時鐘(Studio Clock)**:
牆鐘語意的歸屬,
`utils::studio_clock`。
_Avoid_: wall clock

**`run` 只做組合**:inline 定義
**Bold**(not a term) stays in the entry above.

### Section
**Event**：
fullwidth colon, no avoid.
";
    let got: Vec<_> = entries(sample)
        .iter()
        .map(|e| (e.name.clone(), e.line, e.definition.clone(), e.has_avoid))
        .collect();
    assert_eq!(
        got,
        [
            (
                "工作室時鐘".to_owned(),
                7,
                "牆鐘語意的歸屬, `utils::studio_clock`。".to_owned(),
                true
            ),
            (
                "`run` 只做組合".to_owned(),
                12,
                "inline 定義 **Bold**(not a term) stays in the entry above.".to_owned(),
                false
            ),
            (
                "Event".to_owned(),
                16,
                "fullwidth colon, no avoid.".to_owned(),
                false
            ),
        ]
    );
    assert!(entries(sample)[0].text.starts_with("**工作室時鐘"));
    assert!(entries(sample)[0].text.ends_with("_Avoid_: wall clock"));
}

/// `(name, problems)` per entry of `sample`.
fn problems_by_entry(sample: &str) -> Vec<(String, Vec<String>)> {
    entries(sample)
        .iter()
        .map(|e| (e.name.clone(), entry_problems(e)))
        .collect()
}

#[test]
fn a_definition_with_its_owner_and_an_adr_passes() {
    let at_limit = "定".repeat(200);
    let sample = format!(
        "\
**點數級距(Points Tier)**:
會員依點數餘額分入的固定四級報表維度,owner 是 `points::model::PointsTier`;為什麼不帶權益見 ADR-0010,時段 09:30 起算。
_Avoid_: 會員等級、membership level

**剛好**:
{at_limit}
_Avoid_: 超過
"
    );
    assert_eq!(
        problems_by_entry(&sample),
        [("點數級距".to_owned(), vec![]), ("剛好".to_owned(), vec![])]
    );
}

#[test]
fn scanner_flags_length_code_spans_and_a_missing_avoid() {
    let over = "定".repeat(201);
    let sample = format!(
        "\
**太長**:
{over}
_Avoid_: x

**Spans**:
`a` and `b`
_Avoid_: `c`

**NoAvoid**:
short.
"
    );
    assert_eq!(
        problems_by_entry(&sample),
        [
            (
                "太長".to_owned(),
                vec!["definition 201 chars > 200".to_owned()]
            ),
            ("Spans".to_owned(), vec!["3 code spans > 2".to_owned()]),
            ("NoAvoid".to_owned(), vec!["no _Avoid_ line".to_owned()]),
        ]
    );
}

#[test]
fn scanner_flags_paths_files_line_numbers_commits_and_rounds() {
    let sample = "\
**Paths**:
見 src/modules/orders/service.rs:64-74、bin/seed/dataset/ 與 dataset.rs,另見 GLOSSARY.md;L395 與 (:2140, :2287-2290);env/config/pool 與 {i:02} 不算。
_Avoid_: z

**History**:
回歸 d819e21(R22-09 修正),見R7輪;ADR-0010、12:30、points::service、20261016、deadbeef 都不算。
_Avoid_: w
";
    assert_eq!(
        problems_by_entry(sample),
        [
            (
                "Paths".to_owned(),
                vec![
                    "path src/modules/orders/service.rs:64-74".to_owned(),
                    "path bin/seed/dataset/".to_owned(),
                    "file dataset.rs".to_owned(),
                    "file GLOSSARY.md".to_owned(),
                    "line number L395".to_owned(),
                    "line number :2140".to_owned(),
                    "line number :2287-2290".to_owned(),
                ]
            ),
            (
                "History".to_owned(),
                vec![
                    "commit d819e21".to_owned(),
                    "round R22-09".to_owned(),
                    "round R7".to_owned(),
                ]
            ),
        ]
    );
}

#[test]
fn a_pending_entry_is_skipped_until_it_passes() {
    let sample = "\
**未縮**:
見 `a` `b` `c`。
_Avoid_: x

**已縮**:
短定義。
_Avoid_: y

**Clean**:
短。
_Avoid_: z

**Dirty**:
回歸 d819e21,`a` `b` `c`。
_Avoid_: w
";
    assert_eq!(
        glossary_problems(&entries(sample), &["未縮", "已縮", "改名了"]),
        [
            "  「已縮」 line 5: passes now, delete it from PENDING",
            "  「Dirty」 line 13: 3 code spans > 2; commit d819e21",
            "  PENDING lists 「改名了」, which GLOSSARY.md does not have: delete it or fix the name",
        ]
    );
}
