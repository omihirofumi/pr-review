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
            auto_explain: false,
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

/// Where GitHub PR source opens in the editor.
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
    /// Terminal editor for files no rule matches; `{file}` and `{line}` are substituted.
    pub command: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<EditorRule>,
}

impl Default for EditorConfig {
    fn default() -> Self {
        Self {
            open_in: OpenIn::default(),
            scrolloff: 5,
            command: ["hx", "+{line}", "{file}"].map(String::from).into(),
            rules: Vec::new(),
        }
    }
}

/// Opens files with these extensions in another editor, such as an IDE for one language.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EditorRule {
    pub extensions: Vec<String>,
    pub command: Vec<String>,
    /// A GUI editor runs beside prpr instead of taking over the terminal until it exits.
    #[serde(default)]
    pub background: bool,
}

/// A program and arguments ready to run for one file.
#[derive(Debug, PartialEq, Eq)]
pub struct EditorCommand {
    pub program: String,
    pub args: Vec<String>,
    pub background: bool,
}

impl EditorConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.command.is_empty(),
            "editor command needs at least a program"
        );
        for rule in &self.rules {
            ensure!(
                !rule.command.is_empty(),
                "editor rule command needs at least a program"
            );
            ensure!(
                !rule.extensions.is_empty()
                    && rule
                        .extensions
                        .iter()
                        .all(|extension| !extension.trim_start_matches('.').is_empty()),
                "editor rule extensions cannot be empty"
            );
        }
        Ok(())
    }

    /// The first rule whose extension ends the file name wins, so `gradle.kts` can precede `kts`.
    pub fn command_for(&self, file: &Path, line: u32) -> EditorCommand {
        let name = file
            .file_name()
            .map(|name| name.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let rule = self.rules.iter().find(|rule| {
            rule.extensions.iter().any(|extension| {
                name.ends_with(&format!(
                    ".{}",
                    extension.trim_start_matches('.').to_lowercase()
                ))
            })
        });
        let (command, background) = rule.map_or((&self.command, false), |rule| {
            (&rule.command, rule.background)
        });
        let file = file.to_string_lossy();
        let mut words = command.iter().map(|word| {
            word.replace("{file}", &file)
                .replace("{line}", &line.to_string())
        });
        EditorCommand {
            program: words.next().unwrap_or_default(),
            args: words.collect(),
            background,
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
    /// Wrap long lines instead of scrolling them sideways.
    pub wrap: bool,
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
        .editor
        .validate()
        .with_context(|| format!("Invalid configuration {}", path.display()))?;
    settings
        .keybindings
        .validate()
        .with_context(|| format!("Invalid configuration {}", path.display()))?;
    Ok(settings)
}

pub fn save(path: &Path, settings: &Settings) -> Result<()> {
    settings.ai.validate()?;
    settings.editor.validate()?;
    settings.keybindings.validate()?;
    // Renaming over a symlink would replace the link, so write where it points, as dotfiles setups expect.
    let resolved = fs::canonicalize(path).ok();
    let path = resolved.as_deref().unwrap_or(path);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_rules_pick_a_command_by_extension_and_fill_placeholders() {
        let config: EditorConfig = toml::from_str(
            r#"
            [[rules]]
            extensions = ["gradle.kts"]
            command = ["gradle-editor", "{file}"]

            [[rules]]
            extensions = [".KT", "java"]
            command = ["idea", "--line", "{line}", "{file}"]
            background = true
            "#,
        )
        .unwrap();
        config.validate().unwrap();
        assert_eq!(
            config.command_for(Path::new("/repo/src/Main.kt"), 42),
            EditorCommand {
                program: "idea".into(),
                args: ["--line", "42", "/repo/src/Main.kt"]
                    .map(String::from)
                    .into(),
                background: true,
            }
        );
        assert_eq!(
            config
                .command_for(Path::new("/repo/build.gradle.kts"), 1)
                .program,
            "gradle-editor"
        );
        let fallback = config.command_for(Path::new("/repo/src/main.rs"), 7);
        assert_eq!(fallback.program, "hx");
        assert_eq!(fallback.args, ["+7", "/repo/src/main.rs"]);
        assert!(!fallback.background);

        let empty: EditorConfig =
            toml::from_str("[[rules]]\nextensions = []\ncommand = [\"idea\"]").unwrap();
        assert!(empty.validate().is_err());
    }
}
