use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;


/// Get the configuration directory
pub fn get_config_dir() -> Result<PathBuf> {
    let mut config_dir = dirs::config_dir()
        .ok_or_else(|| anyhow::anyhow!("Could not determine config directory"))?;
    config_dir.push("supaterm");
    std::fs::create_dir_all(&config_dir)?;
    Ok(config_dir)
}

/// Main application configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Terminal configuration
    pub terminal: TerminalConfig,
    /// AI service configuration
    pub ai: AiConfig,
    /// UI preferences
    pub ui: UiConfig,
}

/// Terminal configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalConfig {
    /// Default shell to use
    pub shell: String,
    /// Terminal size (columns)
    pub columns: Option<u16>,
    /// Terminal size (rows)
    pub rows: Option<u16>,
}

/// AI service configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiConfig {
    /// OpenAI API key
    pub api_key: Option<String>,
    /// Model to use
    pub model: String,
    /// Organization ID (if applicable)
    pub org_id: Option<String>,
}

/// UI preferences
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiConfig {
    /// Color theme
    pub theme: String,
    /// Font size
    pub font_size: u8,
    /// Show line numbers
    pub show_line_numbers: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            terminal: TerminalConfig::default(),
            ai: AiConfig::default(),
            ui: UiConfig::default(),
        }
    }
}

impl Default for TerminalConfig {
    fn default() -> Self {
        // Detect default shell from environment
        let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        
        Self {
            shell,
            columns: None,
            rows: None,
        }
    }
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            api_key: env::var("OPENAI_API_KEY").ok(),
            model: "gpt-4".to_string(),
            org_id: env::var("OPENAI_ORG_ID").ok(),
        }
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: "dark".to_string(),
            font_size: 12,
            show_line_numbers: true,
        }
    }
}

impl Config {
    /// Load configuration from file
    pub fn load() -> Result<Self> {
        let config_file = get_config_dir()?.join("config.toml");
        
        if config_file.exists() {
            let mut file = File::open(&config_file)
                .context("Failed to open configuration file")?;
            
            let mut content = String::new();
            file.read_to_string(&mut content)
                .context("Failed to read configuration file")?;
            
            let config: Config = toml::from_str(&content)
                .context("Failed to parse configuration file")?;
            
            Ok(config)
        } else {
            // Create default configuration
            let config = Config::default();
            config.save()?;
            Ok(config)
        }
    }
    
    /// Save configuration to file
    pub fn save(&self) -> Result<()> {
        let config_file = get_config_dir()?.join("config.toml");
        
        let content = toml::to_string_pretty(self)
            .context("Failed to serialize configuration")?;
        
        let mut file = File::create(&config_file)
            .context("Failed to create configuration file")?;
        
        file.write_all(content.as_bytes())
            .context("Failed to write configuration file")?;
        
        Ok(())
    }
}

