//! Which untracked paths of a working tree `status` and `commit` leave alone: the
//! `.gitignore` files in the tree, read with git's rules. Tracked paths are never
//! ignored, as in git.
//!
//! Supported: blank lines and `#` comments; `!` to re-include; a trailing `/` for folders
//! only; a pattern with a `/` before its end matches from its file's folder, one without
//! matches a name at any depth; `*`, `?`, `[...]` (with `!` or `^` to negate, and ranges)
//! within one name; `**` as a whole name for any number of folders; `\` to escape. As in
//! git, nothing below an ignored folder is looked at, so a `!` rule cannot re-include it.
//! Not read: `.git/info/exclude` and the user's global excludes, which are not part of
//! the tree.

use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone)]
struct Rule {
    /// The folder of the `.gitignore` this came from, relative to the tree ("" for the
    /// top).
    base: String,
    /// The pattern split on `/`.
    parts: Vec<String>,
    /// Matches from `base` (the pattern held a `/` before its end), not a name anywhere.
    anchored: bool,
    folders_only: bool,
    negated: bool,
}

/// The rules of every `.gitignore` read so far, outermost first.
#[derive(Debug, Clone, Default)]
pub struct IgnoreRules {
    rules: Vec<Rule>,
}

impl IgnoreRules {
    /// Reads `<folder>/.gitignore`, if there is one, as the rules of tree path `base`.
    ///
    /// # Errors
    /// An unreadable file.
    pub fn read_folder(&mut self, folder: &Path, base: &str) -> Result<()> {
        let path = folder.join(".gitignore");
        match fs::read(&path) {
            Ok(bytes) => {
                self.add(base, &String::from_utf8_lossy(&bytes));
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
        }
    }

    /// Adds the rules in `text` as those of tree path `base`.
    pub fn add(&mut self, base: &str, text: &str) {
        for line in text.lines() {
            if let Some(rule) = parse(base, line) {
                self.rules.push(rule);
            }
        }
    }

    /// Whether tree path `path` (a folder when `folder` is set) is ignored: the last rule
    /// that matches it decides.
    #[must_use]
    pub fn is_ignored(&self, path: &str, folder: bool) -> bool {
        let mut ignored = false;
        for rule in &self.rules {
            if rule.folders_only && !folder {
                continue;
            }
            let Some(relative) = below(&rule.base, path) else {
                continue;
            };
            let matched = if rule.anchored {
                let names: Vec<&str> = relative.split('/').collect();
                match_parts(&rule.parts, &names)
            } else {
                let name = relative.rsplit('/').next().unwrap_or(relative);
                glob(rule.parts[0].as_bytes(), name.as_bytes())
            };
            if matched {
                ignored = !rule.negated;
            }
        }
        ignored
    }
}

/// `path` relative to `base`, when it is below it.
fn below<'a>(base: &str, path: &'a str) -> Option<&'a str> {
    if base.is_empty() {
        return Some(path);
    }
    path.strip_prefix(base)?.strip_prefix('/')
}

fn parse(base: &str, line: &str) -> Option<Rule> {
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    // Trailing spaces are dropped unless escaped.
    let mut text = line.to_string();
    while text.ends_with(' ') && !text.ends_with("\\ ") {
        text.pop();
    }
    if text.is_empty() {
        return None;
    }
    let mut negated = false;
    if let Some(rest) = text.strip_prefix('!') {
        negated = true;
        text = rest.to_string();
    } else if text.starts_with("\\!") || text.starts_with("\\#") {
        text.remove(0);
    }
    let mut folders_only = false;
    if text.ends_with('/') {
        folders_only = true;
        text.pop();
    }
    if text.is_empty() {
        return None;
    }
    let anchored = text.contains('/');
    let text = text.strip_prefix('/').unwrap_or(&text);
    let parts: Vec<String> = text.split('/').map(str::to_string).collect();
    if parts.iter().any(String::is_empty) {
        return None;
    }
    Some(Rule {
        base: base.to_string(),
        parts,
        anchored,
        folders_only,
        negated,
    })
}

/// Matches pattern names against path names; a `**` name matches any number of names,
/// none included.
fn match_parts(parts: &[String], names: &[&str]) -> bool {
    match parts.split_first() {
        None => names.is_empty(),
        Some((first, rest)) if first == "**" => {
            (0..=names.len()).any(|skip| match_parts(rest, &names[skip..]))
        }
        Some((first, rest)) => match names.split_first() {
            Some((name, others)) => {
                glob(first.as_bytes(), name.as_bytes()) && match_parts(rest, others)
            }
            None => false,
        },
    }
}

/// Matches one name against one glob: `*`, `?`, `[...]` and `\` escapes; `**` within a
/// name is the same as `*`.
fn glob(pattern: &[u8], name: &[u8]) -> bool {
    let (mut p, mut n) = (0, 0);
    // Where the last `*` was, and the name position it has swallowed up to.
    let mut star: Option<(usize, usize)> = None;
    while n < name.len() {
        if p < pattern.len() {
            match pattern[p] {
                b'*' => {
                    while p < pattern.len() && pattern[p] == b'*' {
                        p += 1;
                    }
                    star = Some((p, n));
                    continue;
                }
                b'?' => {
                    p += 1;
                    n += 1;
                    continue;
                }
                b'[' => {
                    if let Some((matched, next)) = class(pattern, p, name[n]) {
                        if matched {
                            p = next;
                            n += 1;
                            continue;
                        }
                    } else if name[n] == b'[' {
                        p += 1;
                        n += 1;
                        continue;
                    }
                }
                b'\\' if p + 1 < pattern.len() => {
                    if pattern[p + 1] == name[n] {
                        p += 2;
                        n += 1;
                        continue;
                    }
                }
                byte => {
                    if byte == name[n] {
                        p += 1;
                        n += 1;
                        continue;
                    }
                }
            }
        }
        match star {
            Some((after, swallowed)) => {
                p = after;
                n = swallowed + 1;
                star = Some((after, n));
            }
            None => return false,
        }
    }
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

/// A `[...]` class at `start`: whether `byte` is in it and where the pattern goes on;
/// `None` when the class is not closed (then `[` is literal).
fn class(pattern: &[u8], start: usize, byte: u8) -> Option<(bool, usize)> {
    let mut i = start + 1;
    let negated = matches!(pattern.get(i), Some(b'!' | b'^'));
    if negated {
        i += 1;
    }
    let mut matched = false;
    let mut first = true;
    while i < pattern.len() {
        if pattern[i] == b']' && !first {
            return Some((matched != negated, i + 1));
        }
        first = false;
        let low = if pattern[i] == b'\\' && i + 1 < pattern.len() {
            i += 1;
            pattern[i]
        } else {
            pattern[i]
        };
        if i + 2 < pattern.len() && pattern[i + 1] == b'-' && pattern[i + 2] != b']' {
            let high = pattern[i + 2];
            if (low..=high).contains(&byte) {
                matched = true;
            }
            i += 3;
        } else {
            if low == byte {
                matched = true;
            }
            i += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(text: &str) -> IgnoreRules {
        let mut rules = IgnoreRules::default();
        rules.add("", text);
        rules
    }

    #[test]
    fn loadngo_gitignore_rules_match_as_git_does() {
        let r = rules(
            "# Generated by Cargo\ndebug\ntarget\n**/*.rs.bk\n*.pdb\n.DS_Store\n**/mutants.out*/\n\
             /agentd.local.json\n/models/\n/task-artifacts/\n",
        );
        assert!(r.is_ignored("target", true));
        assert!(r.is_ignored("web3d/target", true));
        assert!(r.is_ignored("debug", false));
        assert!(r.is_ignored("data/src/x.rs.bk", false));
        assert!(r.is_ignored("x.rs.bk", false));
        assert!(r.is_ignored("gui/a.pdb", false));
        assert!(r.is_ignored("docs/.DS_Store", false));
        assert!(r.is_ignored("mutants.out", true));
        assert!(r.is_ignored("data/mutants.out.old", true));
        assert!(!r.is_ignored("data/mutants.out.old", false));
        assert!(r.is_ignored("agentd.local.json", false));
        assert!(!r.is_ignored("data/agentd.local.json", false));
        assert!(r.is_ignored("models", true));
        assert!(!r.is_ignored("models", false));
        assert!(!r.is_ignored("data/models", true));
        assert!(r.is_ignored("task-artifacts", true));
        assert!(!r.is_ignored("data/src/repository.rs", false));
        assert!(!r.is_ignored("targets", true));
    }

    #[test]
    fn later_rules_and_deeper_files_win_and_negation_reincludes() {
        let mut r = rules("*.log\n!keep.log\n");
        r.add("sub", "keep.log\n");
        assert!(r.is_ignored("a.log", false));
        assert!(!r.is_ignored("keep.log", false));
        assert!(r.is_ignored("sub/keep.log", false));
        assert!(!r.is_ignored("other/keep.log", false));
    }

    #[test]
    fn anchored_patterns_double_stars_classes_and_escapes() {
        let r = rules("docs/*.md\na/**/z\nlogs/**\n[Bb]uild\nfile[0-9].txt\n\\#hash\nq?.c\n");
        assert!(r.is_ignored("docs/x.md", false));
        assert!(!r.is_ignored("docs/sub/x.md", false));
        assert!(!r.is_ignored("other/docs/x.md", false));
        assert!(r.is_ignored("a/z", false));
        assert!(r.is_ignored("a/b/c/z", false));
        assert!(r.is_ignored("logs/a/b", false));
        assert!(r.is_ignored("Build", true));
        assert!(r.is_ignored("x/build", true));
        assert!(r.is_ignored("file7.txt", false));
        assert!(!r.is_ignored("filex.txt", false));
        assert!(r.is_ignored("#hash", false));
        assert!(r.is_ignored("q1.c", false));
        assert!(!r.is_ignored("q12.c", false));
        let n = rules("[!a]x\n");
        assert!(n.is_ignored("bx", false));
        assert!(!n.is_ignored("ax", false));
    }

    #[test]
    fn stars_match_within_a_name_only() {
        assert!(glob(b"*.rs", b"main.rs"));
        assert!(glob(b"a*b*c", b"aXXbYYc"));
        assert!(!glob(b"a*b*c", b"aXXbYY"));
        assert!(glob(b"*", b""));
        assert!(!glob(b"?", b""));
        assert!(glob(b"mutants.out*", b"mutants.out"));
    }
}
