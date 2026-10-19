use std::sync::LazyLock;

use regex::Regex;

/// Metadata Odoo's commit conventions and mergebot trailers carry in a message.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CommitMeta {
    pub tag: Option<String>,
    pub modules: Vec<String>,
    pub task: Option<u64>,
    pub opw: Option<u64>,
    /// `owner/repo#N` from `closes` or `Part-of`.
    pub pr: Option<String>,
    pub related_prs: Vec<String>,
    pub original_sha: Option<String>,
}

static SUBJECT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*\[([A-Za-z0-9]+)\](?:\s*\[[A-Za-z0-9]+\])*\s*([^:\s][^:]*?)\s*:").unwrap());
static TASK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\btask(?:[- ]?id)?[-:# ]\s*(\d{3,})").unwrap());
static OPW: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bopw[-:# ]\s*(\d{3,})").unwrap());
static PR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?im)^(?:closes|part-of):?\s+([\w.-]+/[\w.-]+#\d+)").unwrap());
static RELATED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?im)^related:?\s+([\w.-]+/[\w.-]+#\d+)").unwrap());
static ORIGINAL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?im)^x-original-commit:\s*([0-9a-f]{7,40})").unwrap());

impl CommitMeta {
    pub fn parse(message: &str) -> Self {
        let subject = message.lines().next().unwrap_or("");
        let mut meta = CommitMeta::default();
        if let Some(c) = SUBJECT.captures(subject) {
            meta.tag = Some(c[1].to_ascii_uppercase());
            meta.modules = c[2]
                .split(',')
                .map(|m| m.trim().to_string())
                .filter(|m| !m.is_empty() && !m.contains(' '))
                .collect();
        }
        meta.task = TASK.captures(message).and_then(|c| c[1].parse().ok());
        meta.opw = OPW.captures(message).and_then(|c| c[1].parse().ok());
        meta.pr = PR.captures(message).map(|c| c[1].to_string());
        meta.related_prs = RELATED.captures_iter(message).map(|c| c[1].to_string()).collect();
        meta.original_sha = ORIGINAL.captures(message).map(|c| c[1].to_string());
        meta
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mergebot_message() {
        let msg = "[FIX] hr, resource: warn when a shared variable schedule changes\n\n\
                   Body text.\n\ntask-6643661\n\ncloses odoo/odoo#296010\n\n\
                   Related: odoo/enterprise#9001\n\
                   X-original-commit: 37a27c724923eb88aa56c5aec9aaf57306fd2a8d\n\
                   Signed-off-by: Someone <x@odoo.com>\n";
        let m = CommitMeta::parse(msg);
        assert_eq!(m.tag.as_deref(), Some("FIX"));
        assert_eq!(m.modules, ["hr", "resource"]);
        assert_eq!(m.task, Some(6643661));
        assert_eq!(m.pr.as_deref(), Some("odoo/odoo#296010"));
        assert_eq!(m.related_prs, ["odoo/enterprise#9001"]);
        assert_eq!(
            m.original_sha.as_deref(),
            Some("37a27c724923eb88aa56c5aec9aaf57306fd2a8d")
        );
    }

    #[test]
    fn part_of_and_star_module() {
        let m = CommitMeta::parse("[IMP] *: correct documentation links\n\nPart-of: odoo/odoo#12\nopw-334455");
        assert_eq!(m.modules, ["*"]);
        assert_eq!(m.pr.as_deref(), Some("odoo/odoo#12"));
        assert_eq!(m.opw, Some(334455));
    }

    #[test]
    fn untagged_subject() {
        let m = CommitMeta::parse("Merge branch 'x' into master");
        assert_eq!(m.tag, None);
        assert!(m.modules.is_empty());
    }
}
