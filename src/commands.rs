use crate::config::{Config, EnvKey, FILE_NAMES, HookMode};
use crate::{envfile, git, info, naming, ok, pg, warn};
use anyhow::{Context, Result, bail};
use postgres::Client;
use std::collections::HashSet;
use std::io::{BufRead, IsTerminal, Write};

fn branch_or_current(arg: Option<String>) -> Result<String> {
    if let Some(b) = arg {
        return Ok(b);
    }
    let b = git::current_branch()?;
    info(&format!("Using current branch: {b}"));
    Ok(b)
}

fn active(cfg: &Config) -> Result<Option<String>> {
    let k = &cfg.env_keys[0];
    let val = envfile::get(&cfg.env_file, k.key())?;
    Ok(match k {
        EnvKey::Name(_) => val,
        EnvKey::Url(_) => val.and_then(|u| pg::url_db(&u)),
    })
}

/// Refuses to touch the env file while a key holds a value pgit didn't write
/// (not a branch database name), so the user's own setting is never overwritten or removed.
fn check_env(cfg: &Config) -> Result<()> {
    let (prefix, suffix) = cfg.affixes();
    for k in &cfg.env_keys {
        // raw also catches values `get` can't read (non-strings, fnox secrets without `default`).
        let Some(raw) = envfile::raw(&cfg.env_file, k.key())? else {
            continue;
        };
        let val = envfile::get(&cfg.env_file, k.key())?;
        if val.as_deref() == Some("") {
            continue; // an empty placeholder loses nothing
        }
        let db = match k {
            EnvKey::Name(_) => val.clone(),
            EnvKey::Url(_) => val.as_deref().and_then(pg::url_db),
        };
        if !db.as_deref().is_some_and(|d| pg::fits(d, prefix, suffix)) {
            // Never echo a url: it may carry a password.
            let shown = match (k, &db) {
                (EnvKey::Name(_), _) => format!("'{}'", val.unwrap_or(raw)),
                (EnvKey::Url(_), Some(d)) => format!("a url for '{d}'"),
                (EnvKey::Url(_), None) => "a value with no database".into(),
            };
            bail!(
                "{} in {} holds {shown}, not a pgit branch database ({prefix}{{branch}}{suffix}). \
                 pgit owns this key: move your own value out of this file, \
                 or delete the line if pgit wrote it under an older `name`.",
                k.key(),
                cfg.env_file.display(),
            );
        }
    }
    Ok(())
}

fn activate(cfg: &Config, name: &str) -> Result<()> {
    check_env(cfg)?;
    let url = if cfg.env_keys.iter().any(|k| matches!(k, EnvKey::Url(_))) {
        let base = pg::connection_url(cfg)?
            .context("`url:` env keys need a connection url (`connection` or DATABASE_URL)")?;
        pg::url_with_db(&base, name)?
    } else {
        String::new()
    };
    let vals: Vec<(&str, &str)> = cfg
        .env_keys
        .iter()
        .map(|k| match k {
            EnvKey::Name(k) => (k.as_str(), name),
            EnvKey::Url(k) => (k.as_str(), url.as_str()),
        })
        .collect();
    envfile::set(&cfg.env_file, &vals)?;
    ok(&format!(
        "Updated {} -- restart your dev server to use it.",
        cfg.env_file.display()
    ));
    Ok(())
}

fn deactivate(cfg: &Config) -> Result<()> {
    check_env(cfg)?;
    let keys: Vec<&str> = cfg.env_keys.iter().map(EnvKey::key).collect();
    envfile::unset(&cfg.env_file, &keys)?;
    ok(&format!("Now using default database: {}", cfg.template));
    Ok(())
}

fn confirm(question: &str, yes: bool) -> Result<()> {
    if yes {
        return Ok(());
    }
    if !std::io::stdin().is_terminal() {
        bail!("{question} Refusing without --yes (no TTY).");
    }
    eprint!("{question} [y/N] ");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
        bail!("aborted");
    }
    Ok(())
}

fn create_db(
    cfg: &Config,
    db: &mut Client,
    branch: &str,
    name: &str,
    source: &str,
    force: bool,
) -> Result<()> {
    // First, so a refusal neither creates a database nor terminates template connections.
    check_env(cfg)?;
    if !pg::exists(db, source)? {
        bail!("template database '{source}' does not exist");
    }
    let conns = pg::connection_count(db, source)?;
    if conns > 0 {
        if !force {
            bail!(
                "cannot create from template: {conns} active connection(s) to '{source}'. \
                 Stop your dev server first, or use --force to terminate them."
            );
        }
        warn(&format!(
            "Terminating {conns} connection(s) to '{source}'..."
        ));
        pg::terminate_connections(db, source)?;
    }
    info(&format!("Creating '{name}' from template '{source}'..."));
    pg::create(db, name, source, &git::repo_id()?, branch)?;
    ok(&format!("Created '{name}' from '{source}'"));
    activate(cfg, name)
}

pub fn create(branch: Option<String>, from: Option<String>, force: bool) -> Result<()> {
    let cfg = Config::load()?;
    let branch = branch_or_current(branch)?;
    let name = cfg.db_name(&branch)?;
    let source = match from {
        Some(b) => cfg.db_name(&b)?,
        None => cfg.template.clone(),
    };
    let mut db = pg::connect(&cfg)?;
    if pg::exists(&mut db, &name)? {
        bail!("database '{name}' already exists (use: pgit use {branch})");
    }
    create_db(&cfg, &mut db, &branch, &name, &source, force)
}

pub fn use_(branch: Option<String>) -> Result<()> {
    let cfg = Config::load()?;
    let branch = branch_or_current(branch)?;
    let name = cfg.db_name(&branch)?;
    if !pg::exists(&mut pg::connect(&cfg)?, &name)? {
        bail!("database '{name}' does not exist (create it first: pgit create {branch})");
    }
    activate(&cfg, &name)?;
    ok(&format!("Now using '{name}'"));
    Ok(())
}

fn drop_db(cfg: &Config, db: &mut Client, name: &str, active: Option<&str>) -> Result<()> {
    let conns = pg::connection_count(db, name)?;
    if conns > 0 {
        warn(&format!("Terminating {conns} connection(s) to '{name}'..."));
    }
    if active == Some(name) {
        check_env(cfg)?; // before the drop, so a refusal can't leave the env file pointing at nothing
    }
    info(&format!("Dropping '{name}'..."));
    pg::drop(db, name)?;
    ok(&format!("Dropped '{name}'"));
    if active == Some(name) {
        info("It was the active database, removing override.");
        deactivate(cfg)?;
    }
    Ok(())
}

pub fn drop(branch: Option<String>, yes: bool) -> Result<()> {
    let cfg = Config::load()?;
    let branch = branch_or_current(branch)?;
    let name = cfg.db_name(&branch)?;
    let mut db = pg::connect(&cfg)?;
    let managed = pg::list_managed(&mut db, &cfg, &git::repo_id()?)?;
    if !managed.iter().any(|m| m.name == name) {
        if pg::exists(&mut db, &name)? {
            bail!("refusing to drop '{name}': not a pgit branch database");
        }
        let names: Vec<_> = managed.iter().map(|m| m.name.as_str()).collect();
        bail!(
            "database '{name}' does not exist. Branch databases: {}",
            if names.is_empty() {
                "none".into()
            } else {
                names.join(", ")
            }
        );
    }
    confirm(&format!("Drop database '{name}'?"), yes)?;
    drop_db(&cfg, &mut db, &name, active(&cfg)?.as_deref())
}

pub fn list() -> Result<()> {
    let cfg = Config::load()?;
    let active = active(&cfg)?;
    let mut db = pg::connect(&cfg)?;
    let managed = pg::list_managed(&mut db, &cfg, &git::repo_id()?)?;
    if managed.is_empty() {
        info("No branch databases found.");
    } else {
        let w = managed
            .iter()
            .map(|m| m.name.len())
            .max()
            .unwrap_or(0)
            .max(8);
        println!("{:<w$}  {:>10}  BRANCH", "DATABASE", "SIZE");
        for m in &managed {
            let mark = if active.as_deref() == Some(&m.name) {
                "  *"
            } else {
                ""
            };
            let branch = m.branch.as_deref().unwrap_or("(legacy)");
            println!(
                "{:<w$}  {:>10}  {branch}{mark}",
                m.name,
                pg::size(&mut db, &m.name)?
            );
        }
    }
    if active.is_none() {
        info(&format!("(using default: {})", cfg.template));
    }
    Ok(())
}

pub fn reset() -> Result<()> {
    let cfg = Config::load()?;
    match active(&cfg)? {
        None => info("No branch database override is active. Already using default."),
        Some(name) => {
            deactivate(&cfg)?;
            ok(&format!("Removed override for '{name}'"));
        }
    }
    Ok(())
}

pub fn status() -> Result<()> {
    let cfg = Config::load()?;
    let pgc = pg::pg_config(&cfg)?;
    let row = |k: &str, v: &str| println!("{:<10} {v}", format!("{k}:"));
    row("config", &cfg.path.display().to_string());
    row("template", &cfg.template);
    row(
        "env file",
        &format!(
            "{} ({})",
            cfg.env_file.display(),
            cfg.env_keys
                .iter()
                .map(|k| match k {
                    EnvKey::Name(k) => format!("name: {k}"),
                    EnvKey::Url(k) => format!("url: {k}"),
                })
                .collect::<Vec<_>>()
                .join(", ")
        ),
    );
    row(
        "active",
        &active(&cfg)?.unwrap_or_else(|| format!("(default: {})", cfg.template)),
    );
    let db = pgc.connect(postgres::NoTls);
    row(
        "server",
        &format!(
            "{}{}",
            pg::describe(&pgc),
            if db.is_ok() { "" } else { " (unreachable)" }
        ),
    );
    match git::current_branch() {
        Err(_) => row("branch", "(detached)"),
        Ok(b) => {
            row("branch", &b);
            let name = cfg.db_name(&b)?;
            let state = match db {
                Ok(mut db) => {
                    if pg::exists(&mut db, &name)? {
                        "exists"
                    } else {
                        "missing"
                    }
                }
                Err(_) => "unknown",
            };
            row("database", &format!("{name} ({state})"));
        }
    }
    Ok(())
}

pub fn init() -> Result<()> {
    if let Some(f) = FILE_NAMES.iter().find(|f| std::path::Path::new(f).exists()) {
        bail!("{f} already exists");
    }
    let uses_mise = ["mise.toml", ".mise.toml", ".config/mise.toml"]
        .iter()
        .any(|f| std::path::Path::new(f).exists());
    let file = if uses_mise {
        "mise.local.toml"
    } else {
        ".env.local"
    };
    let yaml = format!(
        "# Base database every branch database is cloned from.\n\
         template: postgres\n\
         # {{branch}} is the sanitized git branch (lowercase, [a-z0-9_]).\n\
         name: \"db_branch_{{branch}}\"\n\
         # Optional: use a regex capture instead of the whole branch, e.g. feature/TI-123-x -> ti_123\n\
         # extract: '([A-Z]+-\\d+)'\n\
         # Where the active database is written ([.]mise.local.toml = mise [env], [.]fnox.local.toml = fnox [secrets], else dotenv).\n\
         # Each key gets the database name, or the full connection url with the branch DB.\n\
         env:\n  file: {file}\n  keys:\n    - name: DB_NAME\n    # - url: DATABASE_URL\n\
         # Defaults to DATABASE_URL, then PG* env vars, then postgres@localhost:5432.\n\
         # connection:\n#   url: postgres://postgres:password@localhost:5432\n\
         #   fnox: DATABASE_URL           # or read the url from a fnox secret instead\n\
         #   env: POSTGRES_URL            # or read the url from this env var instead\n\
         # What `pgit hook run` does on checkout: use (switch if it exists) or create.\n\
         hook: use\n"
    );
    std::fs::write("pgit.yaml", yaml).context("writing pgit.yaml")?;
    ok("Wrote pgit.yaml -- set `template` to your base database.");
    Ok(())
}

pub fn prune(yes: bool, dry_run: bool) -> Result<()> {
    let cfg = Config::load()?;
    let branches = git::local_branches()?;
    // With `extract`, several branches share one database; keep it while any of them exists.
    let live: HashSet<String> = branches
        .iter()
        .filter_map(|b| naming::db_name(b, &cfg.name, cfg.extract.as_ref()).ok())
        .map(|(name, _)| name)
        .collect();
    let mut db = pg::connect(&cfg)?;
    let mut stale = Vec::new();
    for m in pg::list_managed(&mut db, &cfg, &git::repo_id()?)? {
        match &m.branch {
            None => info(&format!("Skipping legacy '{}' (branch unknown)", m.name)),
            Some(b) if !branches.contains(b) && !live.contains(&m.name) => stale.push(m),
            Some(_) => {}
        }
    }
    if stale.is_empty() {
        ok("Nothing to prune.");
        return Ok(());
    }
    for m in &stale {
        println!(
            "{}  (branch {} is gone)",
            m.name,
            m.branch.as_deref().unwrap_or_default()
        );
    }
    if dry_run {
        return Ok(());
    }
    confirm(&format!("Drop {} database(s)?", stale.len()), yes)?;
    let active = active(&cfg)?;
    for m in &stale {
        drop_db(&cfg, &mut db, &m.name, active.as_deref())?;
    }
    Ok(())
}

const HOOK_LINE: &str = "pgit hook run \"$@\"";

pub fn hook_print() {
    println!("{HOOK_LINE}");
    info(
        "Add this line to your post-checkout hook (husky, lefthook, ...), or run `pgit hook install`.",
    );
}

pub fn hook_install() -> Result<()> {
    let dir = git::hooks_dir()?;
    let path = dir.join("post-checkout");
    if path.exists() {
        bail!(
            "{} already exists, add this line to it: {HOOK_LINE}",
            path.display()
        );
    }
    std::fs::create_dir_all(&dir)?;
    std::fs::write(&path, format!("#!/bin/sh\n{HOOK_LINE}\n"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    }
    ok(&format!("Installed {}", path.display()));
    Ok(())
}

/// Called by git's post-checkout hook. Never fails, so a checkout is never blocked.
pub fn hook_run(flag: Option<String>) {
    // flag 1 = branch checkout, 0 = file checkout
    if flag.as_deref() != Some("1") {
        return;
    }
    if let Err(e) = hook_switch() {
        warn(&format!("pgit: {e:#}"));
    }
}

fn hook_switch() -> Result<()> {
    let cfg = Config::load()?;
    let Ok(branch) = git::current_branch() else {
        return Ok(());
    };
    let name = cfg.db_name(&branch)?;
    let current = active(&cfg)?;
    let mut db = pg::connect(&cfg)?;
    let exists = pg::exists(&mut db, &name)?;
    match (cfg.hook, exists) {
        (_, true) if current.as_deref() == Some(&name) => Ok(()),
        (_, true) => {
            activate(&cfg, &name)?;
            ok(&format!("pgit: now using '{name}'"));
            Ok(())
        }
        (HookMode::Create, false) => create_db(&cfg, &mut db, &branch, &name, &cfg.template, false),
        (HookMode::Use, false) if current.is_some() => deactivate(&cfg),
        (HookMode::Use, false) => Ok(()),
    }
}
