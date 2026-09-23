//! Writing what was loaded into a directory tree.
//!
//! Write-only, and never deletes: a mirror that pruned would be one stray
//! `--mirror ~/` away from being a disaster, and an old file left behind costs
//! nothing. Existing files are overwritten without asking — being current is
//! the whole point.
//!
//! Two levels, and the ticket key says both of them:
//!
//! ```text
//! <root>/BTE/BTE-915.md    show BTE-915
//! ```
//!
//! No sprint level. A sprint can't be read off a listing — `/rest/api/3/search/jql`
//! silently drops a requested `sprint` field — so it would have to be fetched per
//! ticket, and a directory that only sometimes appears is worse than none.

use std::path::{Path, PathBuf};

use crate::Error;

// Long enough for a sprint name to stay recognisable, short enough to leave
// room under the path limit on any filesystem.
const MAX_SEGMENT: usize = 80;

pub struct Mirror {
    root: PathBuf,
}

impl Mirror {
    pub fn new(root: &str) -> Self {
        Self { root: PathBuf::from(root) }
    }

    // A project key is already `[A-Za-z0-9_]+` by the time it gets here, so it
    // needs no cleaning — but it goes through the same slug for safety, since
    // this is the one place a name becomes a path.
    // The whole address, from the key alone: BTE-915 is project BTE.
    pub fn write_ticket(&self, key: &str, body: &str) -> Result<PathBuf, Error> {
        let project = key.rsplit_once('-').map(|(project, _)| project).unwrap_or(key);
        let dir = self.project(project)?;
        self.ticket(&dir, key, body)
    }

    fn project(&self, key: &str) -> Result<PathBuf, Error> {
        let dir = self.root.join(slug(key));
        create(&dir)?;
        Ok(dir)
    }

    fn ticket(&self, dir: &Path, key: &str, body: &str) -> Result<PathBuf, Error> {
        let file = dir.join(format!("{}.md", slug(key)));
        // Trailing newline: a file that ends mid-line is a nuisance to every
        // tool that reads it.
        let body = if body.ends_with('\n') { body.to_string() } else { format!("{body}\n") };
        std::fs::write(&file, body).map_err(|e| format!("could not write {}: {e}", file.display()))?;
        Ok(file)
    }
}

fn create(dir: &Path) -> Result<(), Error> {
    std::fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    Ok(())
}

// A single path segment from arbitrary text. Everything outside a conservative
// set becomes "-", runs collapse, and the result can't come out as "." or ".."
// or empty — each of which would mean the path pointing somewhere else entirely.
fn slug(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' {
            out.push(ch);
        } else if !out.ends_with('-') {
            // "-" is the separator wherever it comes from, so a literal one next
            // to a replaced space collapses instead of doubling up.
            out.push('-');
        }
    }
    let trimmed = out.trim_matches(|ch| ch == '-' || ch == '.');
    let mut slug: String = trimmed.chars().take(MAX_SEGMENT).collect();
    // Truncation can leave a trailing separator, which reads as a typo.
    while slug.ends_with('-') || slug.ends_with('.') {
        slug.pop();
    }
    if slug.is_empty() { "unnamed".to_string() } else { slug }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The reason this exists at all: nothing may be able to climb out of the tree
    // or name a directory the shell reads as something else.
    #[test]
    fn a_name_cannot_escape_its_directory() {
        assert_eq!(slug("../../etc/passwd"), "etc-passwd");
        assert_eq!(slug("/absolute"), "absolute");
        assert_eq!(slug(".."), "unnamed");
        assert_eq!(slug("."), "unnamed");
        assert_eq!(slug(""), "unnamed");
        assert_eq!(slug("   "), "unnamed");
        for bad in ["a/b", "a\\b", "a:b", "a\0b", "a\nb"] {
            assert!(!slug(bad).contains(['/', '\\', ':', '\0', '\n']), "{bad:?} -> {}", slug(bad));
        }
    }

    #[test]
    fn runs_of_separators_collapse_rather_than_stacking_up() {
        assert_eq!(slug("a   ///  b"), "a-b");
        // A literal dash beside a replaced space is one separator, not two.
        assert_eq!(slug("15.11. - 31.12.21"), "15.11.-31.12.21");
    }

    #[test]
    fn a_very_long_name_is_cut_without_leaving_a_dangling_separator() {
        let long = "x".repeat(200);
        assert_eq!(slug(&long).len(), MAX_SEGMENT);
        let awkward = format!("{} - tail", "y".repeat(MAX_SEGMENT - 2));
        assert!(!slug(&awkward).ends_with('-'), "got: {}", slug(&awkward));
    }

    #[test]
    fn a_ticket_is_written_where_it_says_and_ends_with_a_newline() {
        let root = std::env::temp_dir().join(format!("myjira-mirror-test-{}", std::process::id()));
        let mirror = Mirror::new(root.to_str().unwrap());

        let file = mirror.write_ticket("BUA-1", "body").unwrap();

        assert!(file.ends_with("BUA/BUA-1.md"), "got: {}", file.display());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "body\n");

        // Overwriting is the point: a mirror that kept the first version would
        // go stale the moment a ticket changed.
        mirror.write_ticket("BUA-1", "newer\n").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "newer\n");

        std::fs::remove_dir_all(&root).ok();
    }

    // The backlog has no sprint id to speak of, and shouldn't grow a suffix.
}
