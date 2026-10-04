//! The agent board (`AGENT-BOARD.md` at a workspace root): who holds which paths, and
//! where finished work is handed off. The rules it carries out are the workspace's
//! `COLLABORATION.md`: claim before writing, never touch another agent's claimed paths,
//! and hand off with what changed and what is still open.
//!
//! The board is a Markdown file several agents edit by hand, so this module reads the
//! `## Active claims` and `## Handoffs` tables loosely and changes them by inserting or
//! removing whole rows, leaving every other byte as it was.

/// One row of `## Active claims`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claim {
    pub since: String,
    pub agent: String,
    /// The "Repo / area" cell.
    pub area: String,
    /// The "Paths" cell.
    pub paths: String,
    pub task: String,
}

/// The cells of a table row, unescaped (`\|` is a literal bar).
fn cells(line: &str) -> Option<Vec<String>> {
    let line = line.trim();
    let inner = line.strip_prefix('|')?.strip_suffix('|')?;
    let mut out = Vec::new();
    let mut cell = String::new();
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                cell.push('|');
                chars.next();
            }
            '|' => out.push(std::mem::take(&mut cell).trim().to_owned()),
            _ => cell.push(c),
        }
    }
    out.push(cell.trim().to_owned());
    Some(out)
}

/// A cell's text as a table cell: one line, bars escaped.
fn cell(text: &str) -> String {
    text.replace(['\n', '\r'], " ").replace('|', "\\|")
}

/// The line range of a section's table rows (after its header and separator).
fn table(board: &str, heading: &str) -> Option<(usize, usize)> {
    let lines: Vec<&str> = board.lines().collect();
    let start = lines.iter().position(|l| l.trim() == heading)?;
    let header = (start + 1..lines.len()).find(|&i| lines[i].trim_start().starts_with('|'))?;
    // The separator follows the header; rows follow it until the table ends.
    let first = header + 2;
    let mut end = first;
    while end < lines.len() && lines[end].trim_start().starts_with('|') {
        end += 1;
    }
    Some((first, end))
}

/// Every active claim.
pub fn claims(board: &str) -> Vec<Claim> {
    let Some((first, end)) = table(board, "## Active claims") else {
        return Vec::new();
    };
    board
        .lines()
        .skip(first)
        .take(end - first)
        .filter_map(cells)
        .filter(|c| c.len() >= 5)
        .map(|c| Claim {
            since: c[0].clone(),
            agent: c[1].clone(),
            area: c[2].clone(),
            paths: c[3].clone(),
            task: c[4].clone(),
        })
        .collect()
}

/// The path prefixes a claim's "Paths" cell names: each `code span`, up to its first
/// `{` or `*` (so `proactor/src/{lib,log}.rs` covers `proactor/src/`). Empty when the
/// cell names none, which means the whole area.
fn prefixes(paths: &str) -> Vec<String> {
    paths
        .split('`')
        .skip(1)
        .step_by(2)
        .map(|span| {
            let cut = span.find(['{', '*']).unwrap_or(span.len());
            span[..cut].trim().trim_start_matches("./").to_owned()
        })
        .collect()
}

/// The claim, held by an agent other than `me`, that covers `path` in repository
/// `repo` (`path` relative to the repository), if any.
///
/// A claim covers the path when its area names the repository and either its paths
/// name no specific files (prose: the whole repository) or one of them is a prefix of
/// the path. When in doubt it covers: refusing an edit costs less than overwriting
/// another agent's work.
pub fn conflict(board: &str, me: &str, repo: &str, path: &str) -> Option<Claim> {
    claims(board).into_iter().find(|claim| {
        if claim.agent.eq_ignore_ascii_case(me) {
            return false;
        }
        let names_repo = claim
            .area
            .split(|c: char| c == ',' || c == ';' || c.is_whitespace())
            .any(|word| word.trim_matches('`') == repo);
        if !names_repo {
            return false;
        }
        let prefixes = prefixes(&claim.paths);
        prefixes.is_empty()
            || prefixes
                .iter()
                .any(|p| p.is_empty() || path.starts_with(p.as_str()))
    })
}

/// The board with `row` inserted as the first row of `heading`'s table, or `None` when
/// the table is missing.
fn insert_first(board: &str, heading: &str, row: &str) -> Option<String> {
    let (first, _) = table(board, heading)?;
    let mut lines: Vec<&str> = board.lines().collect();
    lines.insert(first, row);
    let mut out = lines.join("\n");
    if board.ends_with('\n') {
        out.push('\n');
    }
    Some(out)
}

/// The board with this agent's claim on `area` set to `paths` (added, or replacing its
/// earlier row for that area).
pub fn set_claim(board: &str, claim: &Claim) -> Option<String> {
    let row = format!(
        "| {} | {} | {} | {} | {} | in progress |",
        cell(&claim.since),
        cell(&claim.agent),
        cell(&claim.area),
        cell(&claim.paths),
        cell(&claim.task)
    );
    let cleared = remove_claim(board, &claim.agent, &claim.area);
    insert_first(&cleared, "## Active claims", &row)
}

/// The board without `agent`'s claim rows on `area`.
pub fn remove_claim(board: &str, agent: &str, area: &str) -> String {
    let Some((first, end)) = table(board, "## Active claims") else {
        return board.to_owned();
    };
    let mut out: Vec<&str> = Vec::new();
    for (i, line) in board.lines().enumerate() {
        let ours = (first..end).contains(&i)
            && cells(line).is_some_and(|c| c.len() >= 3 && c[1] == agent && c[2] == area);
        if !ours {
            out.push(line);
        }
    }
    let mut text = out.join("\n");
    if board.ends_with('\n') {
        text.push('\n');
    }
    text
}

/// The board with a handoff row first in `## Handoffs`.
pub fn add_handoff(
    board: &str,
    when: &str,
    agent: &str,
    what: &str,
    where_: &str,
    open: &str,
) -> Option<String> {
    let row = format!(
        "| {} | {} | {} | {} | {} |",
        cell(when),
        cell(agent),
        cell(what),
        cell(where_),
        cell(open)
    );
    insert_first(board, "## Handoffs", &row)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOARD: &str = "# Agent Board

## Active claims

| Since | Agent | Repo / area | Paths | Task | Status |
|---|---|---|---|---|---|
| 2026-10-04 | Codex | sng-roguelite, sng-mahjong | Per-repo `target*/`, `build/`, device scripts | Deploy | ok |
| 2026-10-04 | Claude Code | loadngo, split paths | `inference/*`, `gpt-oss/*`, `docs/GPT_OSS.md` | gpt-oss | in progress |
| 2026-10-04 | Codex | qcoin | the whole node \\| everything | Review | ok |

## Handoffs

| When | Agent | What | Where | Open |
|---|---|---|---|---|
| 2026-10-03 | Codex | something | x | — |
";

    #[test]
    fn claims_parse_with_escaped_bars() {
        let all = claims(BOARD);
        assert_eq!(all.len(), 3);
        assert_eq!(all[2].paths, "the whole node | everything");
        assert_eq!(
            prefixes(&all[1].paths),
            ["inference/", "gpt-oss/", "docs/GPT_OSS.md"]
        );
    }

    #[test]
    fn another_agents_paths_conflict_and_ours_do_not() {
        let me = "gpt-oss";
        // Codex names specific prefixes in sng-roguelite.
        assert!(conflict(BOARD, me, "sng-roguelite", "target/debug/x").is_some());
        assert!(conflict(BOARD, me, "sng-roguelite", "build/out").is_some());
        assert!(conflict(BOARD, me, "sng-roguelite", "crates/game/src/lib.rs").is_none());
        // Prose paths cover the whole repository.
        assert!(conflict(BOARD, me, "qcoin", "anything.rs").is_some());
        // Claude Code's split paths in loadngo.
        assert!(conflict(BOARD, me, "loadngo", "gpt-oss/src/gpu.rs").is_some());
        assert!(conflict(BOARD, me, "loadngo", "line-editor/src/lib.rs").is_none());
        // An agent never conflicts with itself; unclaimed repositories are free.
        assert!(conflict(BOARD, "Claude Code", "loadngo", "gpt-oss/src/gpu.rs").is_none());
        assert!(conflict(BOARD, me, "starlight", "src/main.rs").is_none());
    }

    #[test]
    fn claims_are_set_replaced_and_removed_and_handoffs_added_without_touching_the_rest() {
        let claim = Claim {
            since: "2026-10-04 07:00".into(),
            agent: "gpt-oss".into(),
            area: "starlight".into(),
            paths: "`src/main.rs`".into(),
            task: "Fix | the frame gate".into(),
        };
        let once = set_claim(BOARD, &claim).unwrap();
        let wider = set_claim(
            &once,
            &Claim {
                paths: "`src/main.rs`, `src/fb.rs`".into(),
                ..claim.clone()
            },
        )
        .unwrap();
        let ours: Vec<Claim> = claims(&wider)
            .into_iter()
            .filter(|c| c.agent == "gpt-oss")
            .collect();
        assert_eq!(ours.len(), 1);
        assert_eq!(ours[0].paths, "`src/main.rs`, `src/fb.rs`");
        assert_eq!(ours[0].task, "Fix | the frame gate");
        assert_eq!(remove_claim(&wider, "gpt-oss", "starlight"), BOARD);
        let handed = add_handoff(
            BOARD,
            "2026-10-04",
            "gpt-oss",
            "Fixed it",
            "starlight `src/main.rs` (uncommitted)",
            "Review",
        )
        .unwrap();
        assert!(handed.contains("|---|---|---|---|---|\n| 2026-10-04 | gpt-oss | Fixed it |"));
        let row = handed.lines().find(|l| l.contains("Fixed it")).unwrap();
        assert_eq!(handed.replacen(&format!("{row}\n"), "", 1), BOARD);
    }
}
