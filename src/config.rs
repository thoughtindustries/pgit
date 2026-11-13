use anyhow::{Context, Result, bail};
use regex::Regex;
use serde::Deserialize;
use std::path::{Path, PathBuf};

pub const FILE_NAMES: [&str; 4] = [
    "pgit.yaml",
    ".pgit.yaml",
    ".config/pgit.yaml",
    ".config/.pgit.yaml",
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    template: String,
    #[serde(default = "default_name")]
    name: String,
    extract: Option<String>,
    #[serde(default)]
    env: Env,
    #[serde(default)]
    connection: Connection,
    #[serde(default)]
    hook: HookMode,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Env {
    pub file: PathBuf,
    pub keys: Vec<EnvKey>,
}

impl Default for Env {
    fn default() -> Self {
        Env {
            file: ".env.local".into(),
            keys: vec![EnvKey::Name("DB_NAME".into())],
        }
    }
}

/// An env key and what it gets: the database name, or the connection url pointed at that database.
#[derive(Deserialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum EnvKey {
    Name(String),
    Url(String),
}

impl EnvKey {
    pub fn key(&self) -> &str {
        match self {
            EnvKey::Name(k) | EnvKey::Url(k) => k,
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Connection {
    pub url: Option<String>,
    /// fnox secret key holding the connection url, resolved with `fnox get`.
    pub fnox: Option<String>,
    /// Env var holding the connection url, instead of DATABASE_URL.
    pub env: Option<String>,
}

#[derive(Deserialize, Default, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum HookMode {
    #[default]
    Use,
    Create,
}

fn default_name() -> String {
    "db_branch_{branch}".into()
}

pub struct Config {
    pub path: PathBuf,
    pub template: String,
    pub name: String,
    pub extract: Option<Regex>,
    pub env_file: PathBuf,
    pub env_keys: Vec<EnvKey>,
    pub connection: Connection,
    pub hook: HookMode,
}

impl Config {
    pub fn load() -> Result<Config> {
        let path = find(&std::env::current_dir()?)?;
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text, path)
    }

    fn parse(text: &str, path: PathBuf) -> Result<Config> {
        let raw: Raw =
            serde_saphyr::from_str(text).with_context(|| format!("parsing {}", path.display()))?;
        crate::naming::validate(&raw.template).context("`template`")?;
        if raw.name.matches("{branch}").count() != 1 {
            bail!(
                "`name` must contain `{{branch}}` exactly once, got '{}'",
                raw.name
            );
        }
        if raw.env.keys.is_empty() {
            bail!("`env.keys` must not be empty");
        }
        let c = &raw.connection;
        if [c.url.is_some(), c.fnox.is_some(), c.env.is_some()]
            .iter()
            .filter(|&&s| s)
            .count()
            > 1
        {
            bail!("set only one of `connection.url`, `connection.fnox` or `connection.env`");
        }
        let extract = raw
            .extract
            .map(|r| Regex::new(&r))
            .transpose()
            .context("`extract` is not a valid regex")?;
        let mut dir = path.parent().unwrap_or(Path::new("."));
        // `env.file` is relative to the project root, not to `.config/`.
        if dir.file_name() == Some(".config".as_ref()) {
            dir = dir.parent().unwrap_or(dir);
        }
        Ok(Config {
            env_file: dir.join(&raw.env.file),
            path,
            template: raw.template,
            name: raw.name,
            extract,
            env_keys: raw.env.keys,
            connection: raw.connection,
            hook: raw.hook,
        })
    }

    pub fn db_name(&self, branch: &str) -> Result<String> {
        let (name, truncated) = crate::naming::db_name(branch, &self.name, self.extract.as_ref())?;
        if truncated {
            crate::warn(&format!(
                "name for '{branch}' is longer than 63 bytes, using '{name}'"
            ));
        }
        Ok(name)
    }

    /// Text around `{branch}` in the name template, used to recognize this repo's databases.
    pub fn affixes(&self) -> (&str, &str) {
        self.name
            .split_once("{branch}")
            .expect("validated in parse")
    }
}

/// Walk up from `start` to the git root (or filesystem root) looking for a config file.
fn find(start: &Path) -> Result<PathBuf> {
    for dir in start.ancestors() {
        let found: Vec<_> = FILE_NAMES
            .iter()
            .map(|n| dir.join(n))
            .filter(|p| p.is_file())
            .collect();
        match found.as_slice() {
            [one] => return Ok(one.clone()),
            [] => {}
            many => bail!(
                "multiple config files found: {}, keep one",
                many.iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
        if dir.join(".git").exists() {
            break;
        }
    }
    bail!("no pgit.yaml found (run `pgit init`)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_config_gets_defaults() {
        let c = Config::parse("template: app_dev\n", "/r/pgit.yaml".into()).unwrap();
        assert_eq!(c.name, "db_branch_{branch}");
        assert_eq!(c.env_file, Path::new("/r/.env.local"));
        assert_eq!(c.affixes(), ("db_branch_", ""));
    }

    #[test]
    fn dot_config_env_file_is_relative_to_project_root() {
        let c = Config::parse("template: app_dev\n", "/r/.config/pgit.yaml".into()).unwrap();
        assert_eq!(c.env_file, Path::new("/r/.env.local"));
    }

    #[test]
    fn full_config_parses() {
        let yaml = "template: postgres\nname: '{branch}_dev'\nextract: '([A-Z]+-\\d+)'\nenv:\n  file: mise.local.toml\n  keys:\n    - name: DB_NAME\n    - url: OTHER\nconnection:\n  url: postgres://u:p@h:1\nhook: create\n";
        let c = Config::parse(yaml, "/r/pgit.yaml".into()).unwrap();
        assert_eq!(c.affixes(), ("", "_dev"));
        assert_eq!(
            c.env_keys,
            [EnvKey::Name("DB_NAME".into()), EnvKey::Url("OTHER".into())]
        );
        assert!(matches!(c.hook, HookMode::Create));
    }

    #[test]
    fn fnox_connection_parses() {
        let yaml = "template: x\nconnection:\n  fnox: DATABASE_URL\n";
        let c = Config::parse(yaml, "/r/pgit.yaml".into()).unwrap();
        assert_eq!(c.connection.fnox.as_deref(), Some("DATABASE_URL"));
    }

    #[test]
    fn url_key_parses() {
        let yaml = "template: x\nenv:\n  file: .env\n  keys: [{url: POSTGRES_URL}]\nconnection:\n  env: POSTGRES_URL\n";
        let c = Config::parse(yaml, "/r/pgit.yaml".into()).unwrap();
        assert_eq!(c.env_keys, [EnvKey::Url("POSTGRES_URL".into())]);
        assert_eq!(c.connection.env.as_deref(), Some("POSTGRES_URL"));
        let c = Config::parse("template: x\n", "/r/pgit.yaml".into()).unwrap();
        assert_eq!(c.env_keys, [EnvKey::Name("DB_NAME".into())]);
    }

    #[test]
    fn rejects_bad_configs() {
        let p = || PathBuf::from("/r/pgit.yaml");
        assert!(Config::parse("template: x\nname: fixed\n", p()).is_err());
        assert!(Config::parse("template: x\ntypo: 1\n", p()).is_err());
        assert!(Config::parse("template: 'bad name'\n", p()).is_err());
        assert!(Config::parse("template: x\nextract: '('\n", p()).is_err());
        assert!(
            Config::parse(
                "template: x\nconnection:\n  url: postgres://h\n  fnox: K\n",
                p()
            )
            .is_err()
        );
        assert!(Config::parse("template: x\nconnection:\n  env: A\n  fnox: K\n", p()).is_err());
        assert!(Config::parse("template: x\nenv:\n  file: .env\n  keys: []\n", p()).is_err());
        let env = |keys: &str| {
            Config::parse(
                &format!("template: x\nenv:\n  file: .env\n  keys: {keys}\n"),
                p(),
            )
        };
        assert!(env("[DB_NAME]").is_err(), "plain keys must say name or url");
        assert!(env("[{both: U}]").is_err());
        assert!(env("[{name: U, url: V}]").is_err());
        assert!(
            env("[{name: U}]\n  value: url").is_err(),
            "env.value was removed"
        );
    }
}
