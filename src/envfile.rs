//! Reads/writes the active DB in a mise TOML (`[env]`), a fnox TOML (`[secrets]`) or a dotenv file.
use anyhow::{Context, Result};
use std::fs;
use std::path::Path;
use toml_edit::{DocumentMut, InlineTable, Item, table, value};

const FNOX: &str = "secrets";

/// The TOML table holding the keys, or None for dotenv.
fn toml_table(path: &Path) -> Option<&'static str> {
    match path.file_name()?.to_str()? {
        "mise.local.toml" | ".mise.local.toml" => Some("env"),
        "fnox.local.toml" | ".fnox.local.toml" => Some(FNOX),
        _ => None,
    }
}

fn read(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn write_or_remove(path: &Path, text: &str) -> Result<()> {
    if text.trim().is_empty() {
        fs::remove_file(path).with_context(|| format!("removing {}", path.display()))
    } else {
        fs::write(path, text).with_context(|| format!("writing {}", path.display()))
    }
}

fn parse_toml(path: &Path, text: &str) -> Result<DocumentMut> {
    text.parse()
        .with_context(|| format!("parsing {}", path.display()))
}

/// Matches `KEY=...` or `export KEY=...`.
fn dotenv_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let l = line.trim_start();
    let l = l.strip_prefix("export ").map_or(l, str::trim_start);
    l.strip_prefix(key)?.trim_start().strip_prefix('=')
}

pub fn get(path: &Path, key: &str) -> Result<Option<String>> {
    let Some(text) = read(path)? else {
        return Ok(None);
    };
    if let Some(t) = toml_table(path) {
        let doc = parse_toml(path, &text)?;
        let mut item = doc.get(t).and_then(|e| e.get(key));
        if t == FNOX {
            item = item.and_then(|i| i.get("default"));
        }
        return Ok(item.and_then(Item::as_str).map(String::from));
    }
    // Dotenv loaders let the last duplicate win, so read and write the last one.
    Ok(text
        .lines()
        .rev()
        .find_map(|l| dotenv_value(l, key))
        .map(|v| {
            let v = v.trim();
            let unquoted = [('"', '"'), ('\'', '\'')]
                .iter()
                .find_map(|(a, b)| v.strip_prefix(*a)?.strip_suffix(*b));
            unquoted.unwrap_or(v).to_string()
        }))
}

/// The key's value as written, also when `get` can't read it (e.g. a fnox provider secret).
pub fn raw(path: &Path, key: &str) -> Result<Option<String>> {
    let Some(t) = toml_table(path) else {
        return get(path, key);
    };
    let Some(text) = read(path)? else {
        return Ok(None);
    };
    let doc = parse_toml(path, &text)?;
    Ok(doc
        .get(t)
        .and_then(|e| e.get(key))
        .map(|i| i.to_string().trim().to_string()))
}

pub fn set(path: &Path, vals: &[(&str, &str)]) -> Result<()> {
    let text = read(path)?.unwrap_or_default();
    let out = if let Some(t) = toml_table(path) {
        let mut doc = parse_toml(path, &text)?;
        let env = doc.entry(t).or_insert(table());
        let env = env
            .as_table_like_mut()
            .with_context(|| format!("`{t}` in {} is not a table", path.display()))?;
        for &(k, val) in vals {
            let v = if t == FNOX {
                value(InlineTable::from_iter([("default", val)]))
            } else {
                value(val)
            };
            env.insert(k, v);
        }
        doc.to_string()
    } else {
        let mut lines: Vec<String> = text.lines().map(String::from).collect();
        for &(k, val) in vals {
            let new = format!("{k}={val}");
            match lines.iter_mut().rfind(|l| dotenv_value(l, k).is_some()) {
                Some(l) => *l = new,
                None => lines.push(new),
            }
        }
        lines.join("\n") + "\n"
    };
    write_or_remove(path, &out)
}

/// Removes the keys; deletes the file if nothing else is left in it.
pub fn unset(path: &Path, keys: &[&str]) -> Result<()> {
    let Some(text) = read(path)? else {
        return Ok(());
    };
    let out = if let Some(t) = toml_table(path) {
        let mut doc = parse_toml(path, &text)?;
        if let Some(env) = doc.get_mut(t).and_then(Item::as_table_like_mut) {
            for k in keys {
                env.remove(k);
            }
            if env.is_empty() {
                doc.remove(t);
            }
        }
        doc.to_string()
    } else {
        let kept: Vec<&str> = text
            .lines()
            .filter(|l| !keys.iter().any(|&k| dotenv_value(l, k).is_some()))
            .collect();
        kept.join("\n") + "\n"
    };
    write_or_remove(path, &out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str, content: Option<&str>) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("pgit-test-{}-{name}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        let _ = fs::remove_file(&p);
        if let Some(c) = content {
            fs::write(&p, c).unwrap();
        }
        p
    }

    const KEYS: [&str; 2] = ["DB_NAME", "OTHER_DB"];
    const VALS: [(&str, &str); 2] = [("DB_NAME", "db_x"), ("OTHER_DB", "db_x")];

    #[test]
    fn toml_round_trip_preserves_other_entries() {
        let p = tmp(
            "mise.local.toml",
            Some("# keep me\n[env]\nFOO = \"bar\"\n\n[tools]\nnode = \"22\"\n"),
        );
        set(&p, &VALS).unwrap();
        assert_eq!(get(&p, "DB_NAME").unwrap().as_deref(), Some("db_x"));
        assert_eq!(get(&p, "OTHER_DB").unwrap().as_deref(), Some("db_x"));
        unset(&p, &KEYS).unwrap();
        let after = fs::read_to_string(&p).unwrap();
        assert!(
            after.contains("# keep me")
                && after.contains("FOO = \"bar\"")
                && after.contains("node = \"22\"")
        );
        assert!(!after.contains("DB_NAME"));
    }

    #[test]
    fn toml_file_created_then_removed() {
        let p = tmp(".mise.local.toml", None);
        set(&p, &VALS).unwrap();
        assert!(fs::read_to_string(&p).unwrap().contains("[env]"));
        unset(&p, &KEYS).unwrap();
        assert!(!p.exists());
    }

    #[test]
    fn format_from_file_name() {
        let t = |p: &str| toml_table(Path::new(p));
        assert_eq!(t("a/b/.mise.local.toml"), Some("env"));
        assert_eq!(t(".config/fnox.local.toml"), Some(FNOX));
        assert_eq!(t("mise.toml"), None);
        assert_eq!(t("fnox.toml"), None);
    }

    #[test]
    fn fnox_round_trip_preserves_other_entries() {
        let p = tmp(
            "fnox.local.toml",
            Some("[providers.age]\ntype = \"age\"\n\n[secrets]\nFOO = { default = \"bar\" }\n"),
        );
        set(&p, &VALS).unwrap();
        let text = fs::read_to_string(&p).unwrap();
        assert!(text.contains("DB_NAME = { default = \"db_x\" }"), "{text}");
        assert_eq!(get(&p, "DB_NAME").unwrap().as_deref(), Some("db_x"));
        unset(&p, &KEYS).unwrap();
        assert_eq!(
            fs::read_to_string(&p).unwrap(),
            "[providers.age]\ntype = \"age\"\n\n[secrets]\nFOO = { default = \"bar\" }\n"
        );
    }

    #[test]
    fn raw_sees_values_get_cannot_read() {
        let p = tmp(
            ".fnox.local.toml",
            Some("[secrets]\nDATABASE_URL = { provider = \"op\", value = \"op://v/db\" }\n"),
        );
        assert_eq!(get(&p, "DATABASE_URL").unwrap(), None);
        assert!(
            raw(&p, "DATABASE_URL")
                .unwrap()
                .unwrap()
                .contains("op://v/db")
        );
        assert_eq!(raw(&p, "DB_NAME").unwrap(), None);
    }

    #[test]
    fn dotenv_round_trip() {
        let p = tmp(".env.local", Some("A=1\nexport DB_NAME=\"old\"\n# c\n"));
        assert_eq!(get(&p, "DB_NAME").unwrap().as_deref(), Some("old"));
        set(
            &p,
            &[("DB_NAME", "db_x"), ("OTHER_DB", "postgres://h/db_x")],
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(&p).unwrap(),
            "A=1\nDB_NAME=db_x\n# c\nOTHER_DB=postgres://h/db_x\n"
        );
        unset(&p, &KEYS).unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "A=1\n# c\n");
        assert_eq!(get(&p, "DB_NAMEX").unwrap(), None);
    }

    #[test]
    fn dotenv_duplicate_key_last_wins() {
        let p = tmp("dup.env", Some("DB_NAME=a\nDB_NAME=b\n"));
        assert_eq!(get(&p, "DB_NAME").unwrap().as_deref(), Some("b"));
        set(&p, &[("DB_NAME", "db_x")]).unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "DB_NAME=a\nDB_NAME=db_x\n");
        assert_eq!(get(&p, "DB_NAME").unwrap().as_deref(), Some("db_x"));
    }

    #[test]
    fn dotenv_removed_when_empty() {
        let p = tmp("only.env", None);
        set(&p, &VALS).unwrap();
        unset(&p, &KEYS).unwrap();
        assert!(!p.exists());
    }
}
