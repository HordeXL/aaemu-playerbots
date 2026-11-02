//! The password of the bot accounts.
//!
//! Every bot account has the same password. It used to default to the public word `playerbots`,
//! which together with the GM grant made every bot account a GM account anybody could log in to.
//! Now the password comes from, in this order:
//!
//! 1. `server.password` in the config,
//! 2. the environment variable `PLAYERBOTS_PASSWORD`,
//! 3. the secrets file next to the roster (`roster.json` -> `roster.secrets.json`),
//! 4. a roster that exists without a secrets file: the old default (`playerbots`), with a loud
//!    warning, so bots created by an earlier version keep logging in,
//! 5. otherwise a fresh random password, written to the secrets file before any bot logs in.
//!
//! The secrets file is created with owner-only permissions where the platform lets us (mode 0600;
//! on Windows the inherited rights are narrowed to the current user, best effort). The password is
//! never logged, never part of a `Debug` output and never reachable through the web API.

use std::fmt;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{bail, ensure, Context};
use serde::{Deserialize, Serialize};

use crate::config::{Config, DEFAULT_PASSWORD, PASSWORD_ENV};

/// File format version of the secrets file.
pub const SECRETS_VERSION: u32 = 1;
/// Random bytes of a generated password (it is written as hex).
const GENERATED_BYTES: usize = 20;

/// A password. `Debug` hides it and there is no `Display`.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wraps `s`.
    pub fn new(s: impl Into<String>) -> Secret {
        Secret(s.into())
    }

    /// The password itself; only the login exchange needs it.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Whether this is the public default password or empty.
    pub fn is_weak(&self) -> bool {
        self.0.is_empty() || self.0 == DEFAULT_PASSWORD
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<hidden>)")
    }
}

/// Where the password came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// `server.password`.
    Config,
    /// `$PLAYERBOTS_PASSWORD`.
    Env,
    /// The secrets file next to the roster.
    SecretsFile,
    /// Newly generated and written to the secrets file.
    Generated,
    /// The old default, for a roster whose accounts were created before the secrets file existed.
    LegacyDefault,
}

/// The password and its origin.
#[derive(Debug, Clone)]
pub struct Resolved {
    /// The password.
    pub password: Secret,
    /// Where it came from.
    pub source: Source,
}

#[derive(Serialize, Deserialize)]
struct SecretsFile {
    version: u32,
    password: String,
}

/// The secrets file that belongs to the roster file `roster`.
pub fn secrets_path(roster: &Path) -> PathBuf {
    roster.with_extension("secrets.json")
}

/// What decides the password besides the files: the explicit settings.
#[derive(Debug, Default, Clone)]
pub struct Explicit {
    /// `server.password` (empty counts as unset).
    pub config: String,
    /// `$PLAYERBOTS_PASSWORD` (empty counts as unset).
    pub env: String,
}

impl Explicit {
    /// The settings of `cfg` and the environment.
    pub fn from_env(cfg: &Config) -> Explicit {
        Explicit {
            config: cfg.server.password.clone(),
            env: std::env::var(PASSWORD_ENV).unwrap_or_default(),
        }
    }
}

/// Decides the password. `roster` is the roster file, `existing_bots` whether it already lists
/// bots (their accounts exist on the login server under some password). The secrets file is only
/// written when a new password is generated.
pub fn resolve(
    explicit: &Explicit,
    roster: &Path,
    existing_bots: bool,
) -> anyhow::Result<Resolved> {
    resolve_with(explicit, roster, existing_bots, generate)
}

/// [`resolve`] with the settings read from `cfg` and the environment.
pub fn resolve_from_config(
    cfg: &Config,
    roster: &Path,
    existing_bots: bool,
) -> anyhow::Result<Resolved> {
    resolve(&Explicit::from_env(cfg), roster, existing_bots)
}

/// [`resolve`] with the password generator injected (tests).
pub fn resolve_with(
    explicit: &Explicit,
    roster: &Path,
    existing_bots: bool,
    generate: impl FnOnce() -> anyhow::Result<String>,
) -> anyhow::Result<Resolved> {
    if !explicit.config.is_empty() {
        return Ok(Resolved {
            password: Secret::new(explicit.config.clone()),
            source: Source::Config,
        });
    }
    if !explicit.env.is_empty() {
        return Ok(Resolved {
            password: Secret::new(explicit.env.clone()),
            source: Source::Env,
        });
    }
    let path = secrets_path(roster);
    if let Some(p) = read(&path)? {
        return Ok(Resolved {
            password: Secret::new(p),
            source: Source::SecretsFile,
        });
    }
    if existing_bots {
        return Ok(Resolved {
            password: Secret::new(DEFAULT_PASSWORD),
            source: Source::LegacyDefault,
        });
    }
    let password = generate()?;
    ensure!(!password.is_empty(), "the generated password is empty");
    write_private(
        &path,
        &serde_json::to_string_pretty(&SecretsFile {
            version: SECRETS_VERSION,
            password: password.clone(),
        })?,
    )
    .with_context(|| format!("writing the bot password to {}", path.display()))?;
    Ok(Resolved {
        password: Secret::new(password),
        source: Source::Generated,
    })
}

/// Reads the password from the secrets file; `None` if there is no file. A file that exists but
/// is unusable is an error: silently starting over would lock the existing accounts out.
fn read(path: &Path) -> anyhow::Result<Option<String>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let file: SecretsFile = serde_json::from_str(&text).with_context(|| {
        format!(
            "parsing {} (delete it only if no bot account exists yet)",
            path.display()
        )
    })?;
    ensure!(
        file.version == SECRETS_VERSION,
        "{}: version {} is not supported (expected {SECRETS_VERSION})",
        path.display(),
        file.version
    );
    if file.password.is_empty() {
        bail!("{}: the password is empty", path.display());
    }
    Ok(Some(file.password))
}

/// A fresh random password from the operating system's generator (hex, 160 bits).
pub fn generate() -> anyhow::Result<String> {
    let mut bytes = [0u8; GENERATED_BYTES];
    getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("the system random generator: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Refuses `gm_bootstrap` with the public default password (or none): the bot accounts are GM
/// accounts then, and anybody who knows the password could log in to them.
pub fn check_gm_password(cfg: &Config, resolved: &Resolved) -> anyhow::Result<()> {
    if cfg.bots.gm_bootstrap && resolved.password.is_weak() {
        bail!(
            "bots.gm_bootstrap = true needs a private bot password, but the accounts use the public default \
             ({:?}). Set server.password (or ${PASSWORD_ENV}) and change the password of the existing bot \
             accounts in the login database, or delete roster.json and its accounts so the bots start over \
             with a generated password",
            resolved.source
        );
    }
    Ok(())
}

/// Writes `text` to `path` readable by the owner only, through a temp file and a rename.
fn write_private(path: &Path, text: &str) -> anyhow::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let _ = std::fs::remove_file(&tmp);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    {
        let mut f = opts
            .open(&tmp)
            .with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(text.as_bytes())
            .with_context(|| format!("writing {}", tmp.display()))?;
        f.sync_all().ok();
    }
    restrict_to_owner(&tmp);
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

/// Windows: drops the inherited rights of `path` and keeps full control for the current user.
/// Best effort: a failure is only logged.
#[cfg(windows)]
fn restrict_to_owner(path: &Path) {
    let Ok(user) = std::env::var("USERNAME") else {
        return;
    };
    let result = std::process::Command::new("icacls")
        .arg(path)
        .args(["/inheritance:r", "/grant:r"])
        .arg(format!("{user}:F"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    match result {
        Ok(s) if s.success() => {}
        _ => tracing::warn!(
            "could not restrict the rights of {}; keep that file private",
            path.display()
        ),
    }
}

#[cfg(not(windows))]
fn restrict_to_owner(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::temp_dir;

    fn none() -> Explicit {
        Explicit::default()
    }

    fn fixed(p: &'static str) -> impl FnOnce() -> anyhow::Result<String> {
        move || Ok(p.to_string())
    }

    fn never() -> anyhow::Result<String> {
        panic!("no password may be generated here")
    }

    #[test]
    fn a_first_run_generates_a_private_password_and_keeps_it() {
        let dir = temp_dir("secrets-first");
        let roster = dir.join("roster.json");
        let r = resolve_with(&none(), &roster, false, fixed("s3cret-generated")).unwrap();
        assert_eq!(r.source, Source::Generated);
        assert_eq!(r.password.expose(), "s3cret-generated");
        assert!(!r.password.is_weak());
        let file = dir.join("roster.secrets.json");
        assert!(file.is_file());
        assert!(!dir.join("roster.secrets.json.tmp").exists());
        // the next run reads it back instead of generating another one
        let again = resolve_with(&none(), &roster, true, never).unwrap();
        assert_eq!(again.source, Source::SecretsFile);
        assert_eq!(again.password.expose(), "s3cret-generated");
        let again = resolve_with(&none(), &roster, false, never).unwrap();
        assert_eq!(again.password.expose(), "s3cret-generated");
    }

    #[test]
    fn the_explicit_settings_win_and_write_nothing() {
        let dir = temp_dir("secrets-explicit");
        let roster = dir.join("roster.json");
        let cfg = Explicit {
            config: "from-config".into(),
            env: "from-env".into(),
        };
        let r = resolve_with(&cfg, &roster, false, never).unwrap();
        assert_eq!(
            (r.source, r.password.expose()),
            (Source::Config, "from-config")
        );
        let env = Explicit {
            config: String::new(),
            env: "from-env".into(),
        };
        let r = resolve_with(&env, &roster, true, never).unwrap();
        assert_eq!((r.source, r.password.expose()), (Source::Env, "from-env"));
        assert!(!secrets_path(&roster).exists());
    }

    #[test]
    fn an_existing_roster_without_a_secrets_file_keeps_the_old_default() {
        let dir = temp_dir("secrets-legacy");
        let roster = dir.join("roster.json");
        let r = resolve_with(&none(), &roster, true, never).unwrap();
        assert_eq!(r.source, Source::LegacyDefault);
        assert_eq!(r.password.expose(), "playerbots");
        assert!(r.password.is_weak());
        // nothing was written: the operator decides when to move on
        assert!(!secrets_path(&roster).exists());
    }

    #[test]
    fn a_broken_secrets_file_is_an_error_not_a_new_password() {
        let dir = temp_dir("secrets-broken");
        let roster = dir.join("roster.json");
        let file = secrets_path(&roster);
        for bad in [
            "not json",
            r#"{"version":9,"password":"x"}"#,
            r#"{"version":1,"password":""}"#,
            r#"{"version":1}"#,
        ] {
            std::fs::write(&file, bad).unwrap();
            assert!(
                resolve_with(&none(), &roster, false, never).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn the_secrets_file_sits_next_to_the_roster() {
        assert_eq!(
            secrets_path(Path::new("D:/x/roster.json")),
            PathBuf::from("D:/x/roster.secrets.json")
        );
        assert_eq!(
            secrets_path(Path::new("bots")),
            PathBuf::from("bots.secrets.json")
        );
    }

    #[test]
    fn a_secret_never_shows_in_debug_output() {
        let s = Secret::new("hunter2");
        assert!(!format!("{s:?}").contains("hunter2"));
        let r = Resolved {
            password: s,
            source: Source::Config,
        };
        assert!(!format!("{r:?}").contains("hunter2"));
        assert!(Secret::new("").is_weak());
        assert!(Secret::new("playerbots").is_weak());
        assert!(!Secret::new("playerbots1").is_weak());
    }

    #[test]
    fn generated_passwords_are_long_hex_and_differ() {
        let a = generate().unwrap();
        let b = generate().unwrap();
        assert_eq!(a.len(), GENERATED_BYTES * 2);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn gm_bootstrap_refuses_the_default_and_an_empty_password() {
        let mut cfg = Config::default();
        let weak = |p: &str, source| Resolved {
            password: Secret::new(p),
            source,
        };
        // off: nothing to check
        assert!(check_gm_password(&cfg, &weak("playerbots", Source::LegacyDefault)).is_ok());
        cfg.bots.gm_bootstrap = true;
        assert!(check_gm_password(&cfg, &weak("playerbots", Source::LegacyDefault)).is_err());
        assert!(check_gm_password(&cfg, &weak("playerbots", Source::Config)).is_err());
        assert!(check_gm_password(&cfg, &weak("", Source::Env)).is_err());
        assert!(check_gm_password(&cfg, &weak("a-private-one", Source::Config)).is_ok());
        assert!(check_gm_password(&cfg, &weak("0123abcd", Source::Generated)).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn the_secrets_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("secrets-mode");
        let roster = dir.join("roster.json");
        resolve_with(&none(), &roster, false, fixed("x1")).unwrap();
        let mode = std::fs::metadata(secrets_path(&roster))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "{mode:o}");
    }
}
