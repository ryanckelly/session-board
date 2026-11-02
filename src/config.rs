//! The board's settings: environment variables, then a config file, then defaults.
//!
//! ```toml
//! # ~/.config/session-board/config.toml   (or $SESSION_BOARD_CONFIG, or $XDG_CONFIG_HOME/…)
//! workspaces = ["~", "/opt"]   # a folder directly under one of these is a project of its own
//! scratch    = "/tmp"          # throwaway sessions here are grouped as one "scratch" project
//! days       = 30              # how far back ended sessions go (Claude Code's cleanupPeriodDays)
//! poll       = 10              # seconds between refreshes
//! claude     = "claude"        # the claude CLI
//! ```
//!
//! Every setting has an environment variable that wins over the file: `SESSION_BOARD_WORKSPACES`
//! (separated by `:`), `SESSION_BOARD_SCRATCH`, `SESSION_BOARD_DAYS`, `SESSION_BOARD_POLL`,
//! `SESSION_BOARD_CLAUDE`. Claude Code's own folder comes from `CLAUDE_CONFIG_DIR`, as in Claude
//! Code. The cache and the board's own state follow `XDG_CACHE_HOME` and `XDG_STATE_HOME`.
//!
//! Loading takes the environment as a map, so tests never see the machine's own settings.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::path::{Component, Path, PathBuf};

const ENV_PREFIX: &str = "SESSION_BOARD_";

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub home: String,
    pub claude_dir: String,
    pub workspaces: Vec<String>,
    pub scratch: String,
    pub days: u32,
    pub poll: f64,
    /// The claude CLI as typed; may be several words (`ssh box claude`). See [`Config::claude_argv`].
    pub claude: String,
    pub cache_dir: String,
    pub state_dir: String,
    pub config_path: String,
    /// Setting name → where its value came from: `$VAR`, the config file's path, or `default`.
    pub source: BTreeMap<&'static str, String>,
}

/// A config file that can't be read or a value that makes no sense. The program stops on it.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigError(pub String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "session-board: {}", self.0)
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    /// Load from the process environment.
    pub fn from_env() -> Result<Config, ConfigError> {
        load(&std::env::vars().collect())
    }

    /// The claude CLI split like a shell would, so `claude = "ssh box claude"` works.
    pub fn claude_argv(&self) -> Vec<String> {
        shlex::split(&self.claude)
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| vec!["claude".into()])
    }

    pub fn cache_path(&self) -> PathBuf {
        // Not the Python board's transcripts.json: the two formats differ, and both may run.
        Path::new(&self.cache_dir).join("transcripts-rs.json")
    }

    /// Conversations deleted from the board; shared with the Python board, same format.
    pub fn deleted_path(&self) -> PathBuf {
        Path::new(&self.state_dir).join("deleted.json")
    }

    /// Remembered choices (`show_ended`, `show_columns`); shared with the Python board.
    pub fn prefs_path(&self) -> PathBuf {
        Path::new(&self.state_dir).join("prefs.json")
    }
}

/// A raw setting before it is checked: from the environment it is always text.
enum Raw {
    Text(String),
    Toml(toml::Value),
}

impl Raw {
    /// Python's `repr` of the value, for error messages: `'soon'`, `0`.
    fn repr(&self) -> String {
        match self {
            Raw::Text(s) | Raw::Toml(toml::Value::String(s)) => format!("'{s}'"),
            Raw::Toml(v) => v.to_string(),
        }
    }
}

struct Picker<'a> {
    env: &'a HashMap<String, String>,
    file: toml::Table,
    config_path: String,
    source: BTreeMap<&'static str, String>,
}

impl Picker<'_> {
    fn pick(&mut self, name: &'static str) -> Option<Raw> {
        let var = format!("{ENV_PREFIX}{}", name.to_uppercase());
        if let Some(v) = self.env.get(&var).filter(|v| !v.is_empty()) {
            self.source.insert(name, format!("${var}"));
            return Some(Raw::Text(v.clone()));
        }
        if let Some(v) = self.file.get(name) {
            self.source.insert(name, self.config_path.clone());
            return Some(Raw::Toml(v.clone()));
        }
        self.source.insert(name, "default".into());
        None
    }

    fn not_positive(&self, name: &'static str, raw: &Raw) -> ConfigError {
        ConfigError(format!(
            "{name} must be a positive number, not {} (from {})",
            raw.repr(),
            self.source[name]
        ))
    }

    fn days(&mut self) -> Result<u32, ConfigError> {
        let Some(raw) = self.pick("days") else { return Ok(30) };
        let n = match &raw {
            Raw::Text(s) | Raw::Toml(toml::Value::String(s)) => s.trim().parse::<i64>().ok(),
            Raw::Toml(toml::Value::Integer(i)) => Some(*i),
            Raw::Toml(toml::Value::Float(f)) if f.is_finite() => Some(f.trunc() as i64),
            Raw::Toml(_) => None,
        };
        n.filter(|n| *n > 0)
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| self.not_positive("days", &raw))
    }

    fn poll(&mut self) -> Result<f64, ConfigError> {
        let Some(raw) = self.pick("poll") else { return Ok(10.0) };
        let n = match &raw {
            Raw::Text(s) | Raw::Toml(toml::Value::String(s)) => s.trim().parse::<f64>().ok(),
            Raw::Toml(toml::Value::Integer(i)) => Some(*i as f64),
            Raw::Toml(toml::Value::Float(f)) => Some(*f),
            Raw::Toml(_) => None,
        };
        n.filter(|n| *n > 0.0 && n.is_finite())
            .ok_or_else(|| self.not_positive("poll", &raw))
    }
}

/// Load the settings from `env` (the process environment, or a test's own map).
pub fn load(env: &HashMap<String, String>) -> Result<Config, ConfigError> {
    let get = |k: &str| env.get(k).filter(|v| !v.is_empty()).cloned();
    // Windows: $HOME is set by Git Bash and MSYS, not by Windows itself, which has USERPROFILE.
    let home = get("HOME")
        .or_else(|| if cfg!(windows) { get("USERPROFILE") } else { None })
        .unwrap_or_else(fallback_home);
    let home = if cfg!(windows) { normalize(&home) } else { home };
    let xdg_config = get("XDG_CONFIG_HOME").unwrap_or_else(|| join(&home, ".config"));
    let config_path = expand(
        &get("SESSION_BOARD_CONFIG").unwrap_or_else(|| join(&xdg_config, "session-board/config.toml")),
        &home,
        env,
    );
    let file = match std::fs::read_to_string(&config_path) {
        Ok(text) => text
            .parse::<toml::Table>()
            .map_err(|e| ConfigError(format!("can't read {config_path}: {}", e.message())))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
        Err(e) => return Err(ConfigError(format!("can't read {config_path}: {e}"))),
    };
    let mut p = Picker { env, file, config_path: config_path.clone(), source: BTreeMap::new() };

    let raw_workspaces: Vec<String> = match p.pick("workspaces") {
        None => vec!["~".into()],
        // Separated like PATH: `:` on Unix, `;` on Windows (where `:` follows the drive letter).
        Some(Raw::Text(s) | Raw::Toml(toml::Value::String(s))) => s
            .split(if cfg!(windows) { ';' } else { ':' })
            .filter(|w| !w.trim().is_empty())
            .map(String::from)
            .collect(),
        Some(Raw::Toml(toml::Value::Array(items))) => items
            .into_iter()
            .map(|v| match v {
                toml::Value::String(s) => s,
                other => other.to_string(),
            })
            .collect(),
        Some(Raw::Toml(other)) => vec![other.to_string()],
    };
    let mut workspaces: Vec<String> = Vec::new();
    for w in raw_workspaces {
        let w = expand(&w, &home, env);
        if !workspaces.contains(&w) {
            workspaces.push(w);
        }
    }

    let claude_dir_env = get("CLAUDE_CONFIG_DIR");
    p.source.insert(
        "claude_dir",
        if claude_dir_env.is_some() { "$CLAUDE_CONFIG_DIR".into() } else { "default".into() },
    );
    let claude_dir = expand(&claude_dir_env.unwrap_or_else(|| join(&home, ".claude")), &home, env);

    let scratch = match p.pick("scratch") {
        None => default_temp_dir(env),
        Some(Raw::Text(s) | Raw::Toml(toml::Value::String(s))) => s,
        Some(Raw::Toml(other)) => other.to_string(),
    };
    let scratch = expand(&scratch, &home, env);
    let days = p.days()?;
    let poll = p.poll()?;
    let claude = match p.pick("claude") {
        None => "claude".to_string(),
        Some(Raw::Text(s) | Raw::Toml(toml::Value::String(s))) => s,
        Some(Raw::Toml(other)) => other.to_string(),
    };
    let cache_home = get("XDG_CACHE_HOME").unwrap_or_else(|| join(&home, ".cache"));
    let state_home = get("XDG_STATE_HOME").unwrap_or_else(|| join(&home, ".local/state"));

    Ok(Config {
        cache_dir: join(&cache_home, "session-board"),
        state_dir: join(&state_home, "session-board"),
        home,
        claude_dir,
        workspaces,
        scratch,
        days,
        poll,
        claude,
        config_path,
        source: p.source,
    })
}

/// `board --config`: the settings in effect, and where each came from.
pub fn describe(c: &Config) -> String {
    let src = |k: &str| c.source.get(k).map(String::as_str).unwrap_or("default").to_string();
    let rows = [
        ("workspaces", c.workspaces.join(" "), src("workspaces")),
        ("scratch", c.scratch.clone(), src("scratch")),
        ("days", c.days.to_string(), src("days")),
        ("poll", format_g(c.poll), src("poll")),
        ("claude", c.claude.clone(), src("claude")),
        ("claude_dir", c.claude_dir.clone(), src("claude_dir")),
        ("cache_dir", c.cache_dir.clone(), "$XDG_CACHE_HOME or ~/.cache".into()),
        ("state_dir", c.state_dir.clone(), "$XDG_STATE_HOME or ~/.local/state".into()),
    ];
    let found = if Path::new(&c.config_path).exists() { "" } else { "  (not found; defaults apply)" };
    let mut out = format!("config file: {}{found}", c.config_path);
    for (k, v, s) in rows {
        out.push_str(&format!("\n  {k:<11} {v}    [{s}]"));
    }
    out
}

/// `$VAR` and `${VAR}` from `env` (unknown ones stay as typed), a leading `~`, then an absolute,
/// normalised path.
pub fn expand(path: &str, home: &str, env: &HashMap<String, String>) -> String {
    let path = expand_vars(path.trim(), env);
    let path = if path == "~" || path.starts_with("~/") { format!("{home}{}", &path[1..]) } else { path };
    normalize(&absolute(&path))
}

fn expand_vars(s: &str, env: &HashMap<String, String>) -> String {
    static VAR: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\$(\w+|\{[^}]*\})").unwrap());
    VAR.replace_all(s, |c: &regex::Captures| {
        let name = c[1].trim_start_matches('{').trim_end_matches('}');
        env.get(name).cloned().unwrap_or_else(|| c[0].to_string())
    })
    .into_owned()
}

/// `path` made absolute against the current folder, unless it already is (or starts at a root:
/// `/x` counts as absolute on Windows too).
pub fn absolute(path: &str) -> String {
    if path.starts_with('/') || Path::new(path).is_absolute() {
        return path.to_string();
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    canon(&cwd.join(path).to_string_lossy())
}

/// Lexical normalisation like `os.path.normpath`: no `.`, no `..`, no doubled or trailing `/`.
/// On Windows a drive is kept, with `/` separators: `C:\Users\u\..\v` → `C:/Users/v`.
pub fn normalize(path: &str) -> String {
    let path = canon(path);
    let mut prefix = String::new();
    let mut parts: Vec<&str> = Vec::new();
    for c in Path::new(&path).components() {
        match c {
            Component::Normal(p) => parts.push(p.to_str().unwrap_or_default()),
            Component::ParentDir => {
                parts.pop();
            }
            Component::Prefix(p) => prefix = canon(&p.as_os_str().to_string_lossy()),
            Component::RootDir | Component::CurDir => {}
        }
    }
    format!("{prefix}/{}", parts.join("/"))
}

/// The one spelling the board uses for a path, so paths from Claude Code's files, git, the
/// environment and the config file compare equal. On Unix the path as given. On Windows `/`
/// separators, an upper-case drive letter, and no `\\?\` prefix: `c:\Users\u` → `C:/Users/u`
/// (Windows, git and `claude` all take `/`).
#[cfg(not(windows))]
pub fn canon(path: &str) -> String {
    path.to_string()
}

/// The one spelling the board uses for a path, so paths from Claude Code's files, git, the
/// environment and the config file compare equal. On Unix the path as given. On Windows `/`
/// separators, an upper-case drive letter, and no `\\?\` prefix: `c:\Users\u` → `C:/Users/u`
/// (Windows, git and `claude` all take `/`).
#[cfg(windows)]
pub fn canon(path: &str) -> String {
    let path = path.strip_prefix(r"\\?\").unwrap_or(path);
    let mut out = path.replace('\\', "/");
    let b = out.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_lowercase() {
        out[..1].make_ascii_uppercase();
    }
    out
}

fn join(dir: &str, rest: &str) -> String {
    canon(&Path::new(dir).join(rest).to_string_lossy())
}

/// Where Python's `tempfile.gettempdir()` would point: `$TMPDIR` (macOS sets it), then `/tmp`.
fn default_temp_dir(env: &HashMap<String, String>) -> String {
    ["TMPDIR", "TEMP", "TMP"]
        .iter()
        .find_map(|k| env.get(*k).filter(|v| !v.is_empty()).cloned())
        .unwrap_or_else(|| if cfg!(windows) { canon(&std::env::temp_dir().to_string_lossy()) } else { "/tmp".into() })
}

fn fallback_home() -> String {
    std::env::home_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into())
}

/// `printf("%g")`: six significant digits, trailing zeros dropped (`10`, `2.5`).
pub fn format_g(x: f64) -> String {
    if x == 0.0 || !x.is_finite() {
        return x.to_string();
    }
    let exp = x.abs().log10().floor() as i32;
    if !(-4..6).contains(&exp) {
        let s = format!("{x:.5e}");
        let (mantissa, e) = s.split_once('e').unwrap_or((&s, "0"));
        let mantissa = trim_zeros(mantissa);
        let e: i32 = e.parse().unwrap_or(0);
        return format!("{mantissa}e{}{:02}", if e < 0 { '-' } else { '+' }, e.abs());
    }
    let decimals = (5 - exp).max(0) as usize;
    trim_zeros(&format!("{x:.decimals$}")).to_string()
}

fn trim_zeros(s: &str) -> &str {
    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.') } else { s }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_g_matches_python() {
        assert_eq!(format_g(10.0), "10");
        assert_eq!(format_g(2.5), "2.5");
        assert_eq!(format_g(0.1), "0.1");
        assert_eq!(format_g(1234567.0), "1.23457e+06");
    }

    #[test]
    fn normalize_is_lexical() {
        assert_eq!(normalize("/home/u/work/"), "/home/u/work");
        assert_eq!(normalize("/a//b/./c/../d"), "/a/b/d");
        assert_eq!(normalize("/"), "/");
    }
}
