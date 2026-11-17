use std::{
    fmt, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    #[default]
    None,
    Codex,
    Claude,
}

impl Provider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct AiConfig {
    pub provider: Provider,
    pub model: Option<String>,
    pub language: String,
    pub auto_explain: bool,
    pub timeout_seconds: u64,
    pub max_input_bytes: usize,
    /// Provider review commands inspect the repository and run longer than guide generation.
    pub review_timeout_seconds: u64,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            provider: Provider::None,
            model: None,
            language: "en".into(),
            auto_explain: true,
            timeout_seconds: 180,
            max_input_bytes: 160_000,
            review_timeout_seconds: 1200,
        }
    }
}

impl AiConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.timeout_seconds > 0,
            "AI timeout_seconds must be positive"
        );
        ensure!(
            self.max_input_bytes > 0,
            "AI max_input_bytes must be positive"
        );
        ensure!(
            self.review_timeout_seconds > 0,
            "AI review_timeout_seconds must be positive"
        );
        ensure!(
            !self.language.trim().is_empty(),
            "AI language cannot be empty"
        );
        ensure!(
            self.model
                .as_ref()
                .is_none_or(|model| !model.trim().is_empty()),
            "AI model cannot be empty"
        );
        Ok(())
    }
}

/// Where GitHub PR source opens in Helix.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OpenIn {
    /// The checkout prpr was launched from, as it is now; files absent there open read-only.
    #[default]
    Repository,
    /// An isolated Jujutsu workspace pinned to the PR head.
    Workspace,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct EditorConfig {
    pub open_in: OpenIn,
    /// Rows kept between the Diff cursor and the viewport edge before it scrolls.
    pub scrolloff: usize,
}

impl Default for EditorConfig {
    fn default() -> Self {
        Self {
            open_in: OpenIn::default(),
            scrolloff: 5,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiffLayout {
    #[default]
    Unified,
    Split,
    /// Split when the terminal is at least 160 columns wide.
    Auto,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct DiffConfig {
    pub layout: DiffLayout,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub ai: AiConfig,
    pub diff: DiffConfig,
    pub editor: EditorConfig,
    pub keybindings: crate::keybindings::Keybindings,
}

pub fn default_path() -> Result<PathBuf> {
    let root = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| directories::BaseDirs::new().map(|dirs| dirs.home_dir().join(".config")))
        .context("Cannot locate the configuration directory; pass --config FILE")?;
    Ok(root.join("prpr/config.toml"))
}

pub fn load(path: &Path) -> Result<Settings> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Settings::default());
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("Cannot read configuration {}", path.display()));
        }
    };
    let settings: Settings = toml::from_str(&text)
        .with_context(|| format!("Invalid configuration {}", path.display()))?;
    settings.ai.validate()?;
    settings
        .keybindings
        .validate()
        .with_context(|| format!("Invalid configuration {}", path.display()))?;
    Ok(settings)
}

pub fn save(path: &Path, settings: &Settings) -> Result<()> {
    settings.ai.validate()?;
    settings.keybindings.validate()?;
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)
        .with_context(|| format!("Cannot create configuration directory {}", parent.display()))?;
    let text = toml::to_string_pretty(settings).context("Cannot encode configuration")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    std::io::Write::write_all(&mut temporary, text.as_bytes())?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .with_context(|| format!("Cannot save configuration {}", path.display()))?;
    Ok(())
}
