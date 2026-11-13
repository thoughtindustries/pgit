use crate::config::Config;
use anyhow::{Context, Result, bail};
use postgres::config::Host;
use postgres::{Client, NoTls};
use std::env::var;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

/// CREATE/DROP DATABASE run from here so we never sit on the user's DB or the template.
const ADMIN_DB: &str = "template1";

/// `connection.url` / `.fnox` / `.env` (one of them) > DATABASE_URL. Cached so fnox is asked only once.
pub fn connection_url(cfg: &Config) -> Result<Option<String>> {
    static URL: OnceLock<Option<String>> = OnceLock::new();
    if let Some(u) = URL.get() {
        return Ok(u.clone());
    }
    let c = &cfg.connection;
    let url = if let Some(u) = &c.url {
        Some(u.clone())
    } else if let Some(key) = &c.fnox {
        Some(fnox_get(key)?)
    } else if let Some(name) = &c.env {
        Some(var(name).with_context(|| format!("`connection.env`: {name} is not set"))?)
    } else {
        var("DATABASE_URL").ok()
    };
    Ok(URL.get_or_init(|| url).clone())
}

/// `url` with its database replaced by `db`, keeping credentials, host and query params.
pub fn url_with_db(url: &str, db: &str) -> Result<String> {
    let Some((scheme, rest)) = url
        .split_once("://")
        .filter(|(s, _)| matches!(*s, "postgres" | "postgresql"))
    else {
        bail!("`url:` env keys need a postgres:// connection url");
    };
    let (authority, tail) = rest.split_at(rest.find(['/', '?']).unwrap_or(rest.len()));
    let query = tail.find('?').map_or("", |i| &tail[i..]);
    Ok(format!("{scheme}://{authority}/{db}{query}"))
}

pub fn url_db(url: &str) -> Option<String> {
    url.parse::<postgres::Config>()
        .ok()?
        .get_dbname()
        .map(String::from)
}

/// Fills gaps in the connection url from PG* env vars, then localhost:5432 as postgres.
/// shortcut: NoTls and no .pgpass, fine for local dev; add rustls / pgpass if someone needs remote servers.
pub fn pg_config(cfg: &Config) -> Result<postgres::Config> {
    let mut c: postgres::Config = match connection_url(cfg)? {
        Some(u) => u.parse().context("invalid connection url")?,
        None => postgres::Config::new(),
    };
    if c.get_hosts().is_empty() {
        c.host(&var("PGHOST").unwrap_or_else(|_| "localhost".into()));
    }
    if c.get_ports().is_empty()
        && let Ok(p) = var("PGPORT")
    {
        c.port(p.parse().context("invalid PGPORT")?);
    }
    if c.get_user().is_none() {
        c.user(&var("PGUSER").unwrap_or_else(|_| "postgres".into()));
    }
    if c.get_password().is_none()
        && let Ok(p) = var("PGPASSWORD")
    {
        c.password(p);
    }
    c.dbname(ADMIN_DB);
    Ok(c)
}

/// stdin/stderr stay attached so fnox can prompt for auth and show its own errors.
fn fnox_get(key: &str) -> Result<String> {
    let out = Command::new("fnox")
        .args(["get", key])
        .stdin(Stdio::inherit())
        .stderr(Stdio::inherit())
        .output()
        .context("could not run `fnox` (is it installed and on PATH?)")?;
    if !out.status.success() {
        bail!("`fnox get {key}` failed");
    }
    let url = String::from_utf8(out.stdout).context("fnox output is not UTF-8")?;
    Ok(url.trim_end().to_string())
}

pub fn describe(c: &postgres::Config) -> String {
    let host = match c.get_hosts().first() {
        Some(Host::Tcp(h)) => h.clone(),
        Some(other) => format!("{other:?}"),
        None => "?".into(),
    };
    let port = c.get_ports().first().copied().unwrap_or(5432);
    format!("{}@{host}:{port}", c.get_user().unwrap_or("?"))
}

pub fn connect(cfg: &Config) -> Result<Client> {
    let c = pg_config(cfg)?;
    c.connect(NoTls)
        .with_context(|| format!("could not connect to {}", describe(&c)))
}

fn ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

fn literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

pub fn exists(db: &mut Client, name: &str) -> Result<bool> {
    Ok(db
        .query_opt("SELECT 1 FROM pg_database WHERE datname = $1", &[&name])?
        .is_some())
}

pub fn connection_count(db: &mut Client, name: &str) -> Result<i64> {
    let row = db.query_one(
        "SELECT count(*) FROM pg_stat_activity WHERE datname = $1 AND pid <> pg_backend_pid()",
        &[&name],
    )?;
    Ok(row.get(0))
}

pub fn terminate_connections(db: &mut Client, name: &str) -> Result<()> {
    db.execute(
        "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = $1 AND pid <> pg_backend_pid()",
        &[&name],
    )?;
    Ok(())
}

/// Creates `name` from `template`, marks it as pgit-managed, and copies per-DB settings.
/// Drops it again if marking or copying fails, so no half-made database is left behind.
pub fn create(db: &mut Client, name: &str, template: &str, repo: &str, branch: &str) -> Result<()> {
    db.batch_execute(&format!(
        "CREATE DATABASE {} TEMPLATE {}",
        ident(name),
        ident(template)
    ))?;
    let rest = (|| -> Result<()> {
        db.batch_execute(&format!(
            "COMMENT ON DATABASE {} IS {}",
            ident(name),
            literal(&format!("{MARKER}{repo}:{branch}"))
        ))?;
        copy_settings(db, template, name)
    })();
    if rest.is_err() {
        let _ = drop(db, name);
    }
    rest
}

/// FORCE (Postgres 13+) terminates connections as part of the drop, so reconnecting clients can't race it.
pub fn drop(db: &mut Client, name: &str) -> Result<()> {
    db.batch_execute(&format!("DROP DATABASE {} WITH (FORCE)", ident(name)))?;
    Ok(())
}

/// GUCs whose stored value is an already-quoted list; single-quoting them would turn
/// `ti, etl, public` into one schema named "ti, etl, public".
const LIST_GUCS: [&str; 5] = [
    "search_path",
    "temp_tablespaces",
    "session_preload_libraries",
    "shared_preload_libraries",
    "local_preload_libraries",
];

fn set_clause(setting: &str) -> Result<String> {
    let (key, val) = setting.split_once('=').context("malformed setting")?;
    if key.is_empty()
        || !key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
    {
        bail!("unexpected setting name '{key}'");
    }
    let val = if LIST_GUCS.contains(&key.to_ascii_lowercase().as_str()) {
        val.to_string()
    } else {
        literal(val)
    };
    Ok(format!("SET {key} TO {val}"))
}

/// CREATE DATABASE ... TEMPLATE doesn't copy pg_db_role_setting (e.g. search_path).
fn copy_settings(db: &mut Client, from: &str, to: &str) -> Result<()> {
    let rows = db.query(
        "SELECT r.rolname, unnest(s.setconfig) FROM pg_db_role_setting s
         JOIN pg_database d ON d.oid = s.setdatabase
         LEFT JOIN pg_roles r ON r.oid = s.setrole
         WHERE d.datname = $1",
        &[&from],
    )?;
    for row in rows {
        let role: Option<String> = row.get(0);
        let setting: String = row.get(1);
        let target = match &role {
            Some(r) => format!("ROLE {} IN DATABASE {}", ident(r), ident(to)),
            None => format!("DATABASE {}", ident(to)),
        };
        let sql = format!("ALTER {target} {}", set_clause(&setting)?);
        db.batch_execute(&sql)
            .with_context(|| format!("copying setting: {sql}"))?;
    }
    Ok(())
}

const MARKER: &str = "pgit:";

pub struct Managed {
    pub name: String,
    /// None for legacy DBs (matched by name only, created before pgit).
    pub branch: Option<String>,
}

/// Databases matching this repo's name template that pgit created for `repo` (or legacy ones).
/// The configured template is never one of them, even when its name matches.
pub fn list_managed(db: &mut Client, cfg: &Config, repo: &str) -> Result<Vec<Managed>> {
    let (prefix, suffix) = cfg.affixes();
    let rows = db.query(
        "SELECT datname, shobj_description(oid, 'pg_database') FROM pg_database
         WHERE NOT datistemplate AND datname <> $1 ORDER BY datname",
        &[&cfg.template],
    )?;
    // Only definitely missing, and only inside a folder that still exists: an unreadable path
    // (another user's home) or one from a container/other machine/unmounted volume isn't gone.
    let gone = |r: &str| {
        let r = std::path::Path::new(r);
        r.parent()
            .and_then(std::path::Path::parent)
            .is_some_and(|p| p.is_dir())
            && matches!(r.try_exists(), Ok(false))
    };
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let name: String = row.get(0);
            let comment: Option<String> = row.get(1);
            let branch = owned(&name, comment.as_deref(), prefix, suffix, repo, gone)?;
            Some(Managed { name, branch })
        })
        .collect())
}

/// Whether `name` fits the name template around `{branch}`.
pub fn fits(name: &str, prefix: &str, suffix: &str) -> bool {
    name.len() > prefix.len() + suffix.len() && name.starts_with(prefix) && name.ends_with(suffix)
}

/// `Some(branch)` if the database belongs to `repo`, where `branch` is None for legacy ones:
/// no comment (only with a non-empty prefix), a marker from before repos were recorded,
/// or a marker whose repo is `gone` from disk (the repo folder was moved or deleted).
fn owned(
    name: &str,
    comment: Option<&str>,
    prefix: &str,
    suffix: &str,
    repo: &str,
    gone: impl Fn(&str) -> bool,
) -> Option<Option<String>> {
    if !fits(name, prefix, suffix) {
        return None;
    }
    let Some(comment) = comment else {
        return (!prefix.is_empty()).then_some(None);
    };
    // Branch names can't contain ':', so the last one separates repo and branch.
    match comment.strip_prefix(MARKER)?.rsplit_once(':') {
        Some((r, branch)) if r == repo => Some(Some(branch.to_string())),
        Some((r, _)) => gone(r).then_some(None),
        None => Some(None),
    }
}

pub fn size(db: &mut Client, name: &str) -> Result<String> {
    Ok(db
        .query_one(
            "SELECT pg_size_pretty(pg_database_size($1::text::name))",
            &[&name],
        )?
        .get(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_gucs_stay_raw_others_get_quoted() {
        assert_eq!(
            set_clause("search_path=ti, etl, audit, public").unwrap(),
            "SET search_path TO ti, etl, audit, public"
        );
        assert_eq!(
            set_clause("TimeZone=Europe/Paris").unwrap(),
            "SET TimeZone TO 'Europe/Paris'"
        );
        assert_eq!(set_clause("app.x=it's").unwrap(), "SET app.x TO 'it''s'");
        assert!(set_clause("bad;key=1").is_err());
    }

    #[test]
    fn url_db_swap() {
        let swap = |u| url_with_db(u, "db_x").unwrap();
        assert_eq!(
            swap("postgres://u:p@h:5432/app"),
            "postgres://u:p@h:5432/db_x"
        );
        assert_eq!(swap("postgresql://u@h"), "postgresql://u@h/db_x");
        assert_eq!(
            swap("postgres://u:p@h/app?sslmode=require"),
            "postgres://u:p@h/db_x?sslmode=require"
        );
        assert_eq!(
            swap("postgres://h?sslmode=disable"),
            "postgres://h/db_x?sslmode=disable"
        );
        assert!(url_with_db("host=h dbname=app", "db_x").is_err());
        assert_eq!(url_db("postgres://u:p@h/db_x").as_deref(), Some("db_x"));
        assert_eq!(url_db("postgres://u:p@h"), None);
    }

    #[test]
    fn ownership() {
        let own = |name, comment, prefix, suffix| {
            owned(name, comment, prefix, suffix, "/a:b/.git", |r| {
                r == "/gone/.git"
            })
        };
        let ours = Some(Some("feature/x".to_string()));
        assert_eq!(
            own("db_x", Some("pgit:/a:b/.git:feature/x"), "db_", ""),
            ours
        );
        assert_eq!(
            own("x_dev", Some("pgit:/a:b/.git:feature/x"), "", "_dev"),
            ours
        );
        // another repo on the same server
        assert_eq!(
            own("db_x", Some("pgit:/other/.git:feature/x"), "db_", ""),
            None
        );
        // legacy: another repo whose folder no longer exists (moved or deleted)
        assert_eq!(
            own("db_x", Some("pgit:/gone/.git:feature/x"), "db_", ""),
            Some(None)
        );
        // legacy: no comment needs a prefix; old marker without repo
        assert_eq!(own("db_x", None, "db_", ""), Some(None));
        assert_eq!(own("x_dev", None, "", "_dev"), None);
        assert_eq!(own("db_x", Some("pgit:feature/x"), "db_", ""), Some(None));
        // not ours: foreign comment, wrong shape, empty branch part
        assert_eq!(own("db_x", Some("prod data"), "db_", ""), None);
        assert_eq!(own("x_prod", Some("pgit:/a:b/.git:x"), "", "_dev"), None);
        assert_eq!(own("db_", None, "db_", ""), None);
    }
}
