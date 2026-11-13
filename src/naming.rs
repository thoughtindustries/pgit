use anyhow::{Result, bail};
use regex::Regex;

/// Postgres truncates identifiers longer than this (NAMEDATALEN - 1).
const MAX_LEN: usize = 63;

/// "feature/New-Audit" -> "feature_new_audit"
pub fn sanitize(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars().flat_map(char::to_lowercase) {
        let c = if c.is_ascii_lowercase() || c.is_ascii_digit() {
            c
        } else {
            '_'
        };
        if c == '_' && out.ends_with('_') {
            continue;
        }
        out.push(c);
    }
    out.trim_matches('_').to_string()
}

pub fn validate(name: &str) -> Result<()> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        bail!("invalid database name '{name}' (only alphanumeric, underscore, hyphen allowed)");
    }
    Ok(())
}

/// Returns the database name and whether it had to be truncated.
pub fn db_name(branch: &str, template: &str, extract: Option<&Regex>) -> Result<(String, bool)> {
    let part = extract
        .and_then(|re| re.captures(branch))
        .and_then(|c| c.get(1).or_else(|| c.get(0)))
        .map_or(branch, |m| m.as_str());
    let part = sanitize(part);
    if part.is_empty() {
        bail!("branch '{branch}' gives an empty database name");
    }
    let name = template.replace("{branch}", &part);
    validate(&name)?;
    if name.len() <= MAX_LEN {
        return Ok((name, false));
    }
    // Shorten only the branch part so the template's prefix and suffix survive (list/drop/prune match on them).
    let hash = format!("_{:08x}", fnv1a(&name));
    let affixes = template.len() - "{branch}".len();
    let Some(room) = MAX_LEN.checked_sub(affixes + hash.len()).filter(|&r| r > 0) else {
        bail!("name template '{template}' leaves no room for the branch within {MAX_LEN} bytes");
    };
    let mut part = part;
    part.truncate(room); // ASCII-only after sanitize, so byte truncation is safe
    Ok((template.replace("{branch}", &(part + &hash)), true))
}

/// Stable across Rust versions, unlike std's DefaultHasher.
fn fnv1a(s: &str) -> u32 {
    s.bytes()
        .fold(0x811c9dc5, |h, b| (h ^ b as u32).wrapping_mul(0x01000193))
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: &str = "db_branch_{branch}";

    #[test]
    fn matches_mise_task_sanitization() {
        assert_eq!(
            sanitize("feature/new-audit-table"),
            "feature_new_audit_table"
        );
        assert_eq!(sanitize("--Foo__Bar//"), "foo_bar");
        assert_eq!(sanitize("café"), "caf");
        assert_eq!(
            db_name("feature/x", T, None).unwrap().0,
            "db_branch_feature_x"
        );
    }

    #[test]
    fn extract_uses_capture_or_falls_back() {
        let re = Regex::new(r"([A-Z]+-\d+)").unwrap();
        assert_eq!(
            db_name("feature/TI-123-foo", T, Some(&re)).unwrap().0,
            "db_branch_ti_123"
        );
        assert_eq!(db_name("main", T, Some(&re)).unwrap().0, "db_branch_main");
        let whole = Regex::new(r"[a-z]+$").unwrap();
        assert_eq!(
            db_name("feat/abc", T, Some(&whole)).unwrap().0,
            "db_branch_abc"
        );
    }

    #[test]
    fn long_names_truncate_deterministically() {
        let b = "x".repeat(100);
        let (a, truncated) = db_name(&b, T, None).unwrap();
        assert!(truncated);
        assert_eq!(a.len(), MAX_LEN);
        assert_eq!(a, db_name(&b, T, None).unwrap().0);
        assert_ne!(a, db_name(&"x".repeat(101), T, None).unwrap().0);
        let (s, _) = db_name(&b, "{branch}_dev", None).unwrap();
        assert!(s.len() == MAX_LEN && s.ends_with("_dev"), "{s}");
        assert!(db_name(&b, &format!("{}{{branch}}", "p".repeat(60)), None).is_err());
    }

    #[test]
    fn rejects_bad_names() {
        assert!(db_name("///", T, None).is_err());
        assert!(db_name("ok", "bad name {branch}", None).is_err());
    }
}
