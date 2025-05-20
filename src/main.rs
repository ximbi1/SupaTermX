mod terminal;
mod tui;
mod ai;
mod scaffold;
mod utils;
mod session;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use log::{debug, error, info, warn};
use std::env;
use std::path::PathBuf;

/// SupaTerm - A lightweight intelligent terminal for Linux
#[derive(Parser, Debug)]
#[clap(name = "supaterm", version, about, long_about = None)]
struct Args {
    /// Shell to use (default: from environment)
    #[clap(short, long)]
    shell: Option<String>,
    
    /// OpenAI API key (default: from environment)
    #[clap(long, envvar = "OPENAI_API_KEY")]
    api_key: Option<String>,
    
    /// Log level
    #[clap(long, default_value = "info")]
    log_level: String,
    
    /// Configuration file
    #[clap(long)]
    config: Option<PathBuf>,
    
    /// Working directory
    #[clap(short, long)]
    workdir: Option<PathBuf>,
    
    /// Subcommands
    #[clap(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Create a new project scaffold
    New {
        /// Project name
        #[clap(required = true)]
        name: String,
        
        /// Project description
        #[clap(short, long)]
        description: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // ===== Initialization Phase =====
    // Set custom panic hook first, so terminal state is restored even on panic
    terminal::set_panic_hook();

    // Parse command line arguments
    let args = Args::parse();
    
    // Initialize logging early to capture all diagnostic information
    let log_level = match args.log_level.to_lowercase().as_str() {
        "trace" => log::LevelFilter::Trace,
        "debug" => log::LevelFilter::Debug,
        "info" => log::LevelFilter::Info,
        "warn" => log::LevelFilter::Warn,
        "error" => log::LevelFilter::Error,
        _ => log::LevelFilter::Info,
    };
    
    utils::init_logging(Some(log_level));
    
    info!("Starting SupaTerm v{}", env!("CARGO_PKG_VERSION"));
    debug!("Command line arguments: {:?}", args);
    
    // Check if important environment variables are set
    let env_warnings = validate_environment();
    for warning in env_warnings {
        warn!("{}", warning);
    }
    
    // Load configuration
    let mut config = match args.config {
        Some(path) => {
            info!("Loading configuration from {}", path.display());
            // TODO: Implement custom config file loading
            utils::config::Config::default()
        },
        None => {
            info!("Loading default configuration");
            match utils::config::Config::load() {
                Ok(config) => config,
                Err(e) => {
                    warn!("Failed to load configuration: {}", e);
                    info!("Using default configuration");
                    utils::config::Config::default()
                }
            }
        }
    };
    
    // ===== Configuration Phase =====
    // Override configuration with command line arguments
    if let Some(shell) = args.shell {
        config.terminal.shell = shell;
    }
    
    if let Some(api_key) = args.api_key {
        config.ai.api_key = Some(api_key);
    }
    
    // Validate shell exists
    if !utils::term::is_shell_available(&config.terminal.shell) {
        warn!("Shell '{}' not found, falling back to system default", config.terminal.shell);
        config.terminal.shell = utils::term::get_default_shell();
        info!("Using shell: {}", config.terminal.shell);
    }
    
    // Change working directory if specified
    if let Some(workdir) = args.workdir {
        std::env::set_current_dir(&workdir)
            .context(format!("Failed to change working directory to {}", workdir.display()))?;
        info!("Changed working directory to {}", workdir.display());
    }
    
    // ===== Service Initialization Phase =====
    // Initialize AI client
    let ai_config = ai::Config {
        api_key: config.ai.api_key.clone().unwrap_or_default(),
        model: config.ai.model.clone(),
        organization_id: config.ai.org_id.clone(),
        max_tokens: Some(2048),
        temperature: Some(0.7),
    };
    
    let ai_client = match ai::AiClient::new(ai_config) {
        Ok(client) => {
            info!("AI client initialized with model: {}", config.ai.model);
            client
        },
        Err(e) => {
            warn!("Failed to initialize AI client: {}", e);
            warn!("AI features will be disabled");
            
            // Create a client with empty API key, which will be non-functional
            // but allow the application to start without AI features
            match ai::AiClient::new(ai::Config::default()) {
                Ok(client) => client,
                Err(e) => {
                    error!("Failed to initialize fallback AI client: {}", e);
                    error!("Cannot continue without AI client");
                    // Clean up and exit
                    terminal::cleanup()?;
                    return Err(anyhow::anyhow!("Failed to initialize AI client"));
                }
            }
        }
    };
    
    // Initialize scaffolder
    let scaffolder = match scaffold::Scaffolder::new(ai_client.clone()) {
        Ok(s) => {
            info!("Project scaffolder initialized");
            s
        },
        Err(e) => {
            error!("Failed to initialize scaffolder: {}", e);
            return Err(e);
        }
    };
    
    // Check for subcommands
    if let Some(cmd) = args.command {
        match cmd {
            Commands::New { name, description } => {
                info!("Creating new project: {}", name);
                // TODO: Implement new project scaffolding without TUI
                println!("Project creation from command line not yet implemented");
                return Ok(());
            }
        }
    }
    
    // Initialize terminal
    let terminal_config = terminal::Config {
        shell: config.terminal.shell.clone(),
    };
    
    info!("Initializing terminal with shell: {}", terminal_config.shell);
    
    // ===== Application Startup Phase =====
    // Start the application
     let mut app = match tui::App::new(terminal_config) {
        Ok(app) => app,
        Err(e) => {
            error!("Failed to initialize application: {}", e);
            
            // Attempt to clean up resources
            terminal::cleanup()
                .unwrap_or_else(|ce| error!("Failed to clean up terminal resources: {}", ce));
                
            return Err(e);
        }
    };
    
    // Run the application with proper cleanup
    let result = app.run().await;
    
    // ===== Application Shutdown Phase =====
    // Always attempt to clean up terminal resources
    info!("Cleaning up terminal resources...");
    if let Err(e) = terminal::cleanup() {
        error!("Failed to clean up terminal resources: {}", e);
    }
    
    // Handle result after cleanup
    match result {
        Ok(_) => {
            info!("Application exited successfully");
            Ok(())
        },
        Err(e) => {
            error!("Application error: {}", e);
            Err(e)
        }
    }
}

/// Validate the environment and return any warnings
fn validate_environment() -> Vec<String> {
    let mut warnings = Vec::new();
    
    // Check OpenAI API key
    if env::var("OPENAI_API_KEY").is_err() {
        warnings.push(
            "OPENAI_API_KEY environment variable not set. AI features will be disabled.".to_string()
        );
    }
    
    // Check if terminal supports required features
    if !utils::term::supports_ansi() {
        warnings.push(
            "Terminal does not appear to support ANSI escape sequences. Display may be corrupted.".to_string()
        );
    }
    
    // Check if we're running inside a supported shell
    if let Ok(shell) = env::var("SHELL") {
        if !shell.contains("fish") && !shell.contains("bash") && !shell.contains("zsh") {
            warnings.push(
                format!("Running in potentially unsupported shell: {}. Best results with fish, bash, or zsh.", shell)
            );
        }
    }
    
    warnings
}
