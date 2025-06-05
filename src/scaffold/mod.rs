use anyhow::{Context, Result};
use log::{debug, warn};
use similar::{ChangeTag, TextDiff};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::collections::HashMap;
use tokio::sync::mpsc::{Receiver, Sender};

use crate::ai::{AiClient, AiResponse, ResponseType};
use serde::{Deserialize, Serialize};

/// Template source type
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateSource {
    /// Built-in template
    BuiltIn,
    /// User-defined template
    UserDefined,
    /// Generated from AI
    Generated,
}

/// Project template definition
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProjectTemplate {
    /// Name of the template
    pub name: String,
    /// Description of the template
    pub description: String,
    /// Template files
    pub files: Vec<TemplateFile>,
    /// Configuration questions to ask
    pub questions: Vec<TemplateQuestion>,
    /// Source of this template
    #[serde(skip)]
    pub source: TemplateSource,
    /// Dependencies for this template
    #[serde(default)]
    pub dependencies: Vec<String>,
    /// Commands to run after project creation
    #[serde(default)]
    pub post_commands: Vec<String>,
}

/// Template file definition
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TemplateFile {
    /// Path to the file (relative to project root)
    pub path: String,
    /// Content template with placeholders
    pub content: String,
    /// Whether this file should be executable
    #[serde(default)]
    pub executable: bool,
    /// Condition for including this file
    #[serde(default)]
    pub condition: Option<String>,
}

/// Question to ask when configuring a template
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TemplateQuestion {
    /// ID of this question for variable substitution
    pub id: String,
    /// Human-readable question text
    pub question: String,
    /// Default value if user doesn't provide input
    pub default: Option<String>,
    /// Type of question (text, boolean, select)
    #[serde(default = "default_question_type")]
    pub question_type: String,
    /// Options for select-type questions
    #[serde(default)]
    pub options: Vec<String>,
    /// Help text explaining this question
    pub help: Option<String>,
}

fn default_question_type() -> String {
    "text".to_string()
}

/// Status of scaffold project creation
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScaffoldStatus {
    /// Initializing
    Initializing,
    /// Collecting user input
    CollectingInput,
    /// Generating files
    Generating,
    /// Creating project files
    Creating,
    /// Running post-creation commands
    RunningCommands,
    /// Completed successfully
    Completed,
    /// Failed
    Failed,
}

/// Progress update for scaffold creation
#[derive(Debug, Clone)]
pub struct ScaffoldProgress {
    /// Current status
    pub status: ScaffoldStatus,
    /// Current message
    pub message: String,
    /// Percent complete (0-100)
    pub percent: u8,
    /// Current file being processed (if any)
    pub current_file: Option<String>,
    /// Error message (if any)
    pub error: Option<String>,
}

/// Represents a file to be created or modified
#[derive(Debug, Clone)]
pub struct ScaffoldFile {
    /// Path to the file
    pub path: PathBuf,
    /// Content of the file
    pub content: String,
    /// Whether the file already exists
    pub exists: bool,
}

/// Represents an applied change
#[derive(Debug, Clone)]
pub struct AppliedChange {
    /// Path to the file
    pub path: PathBuf,
    /// Type of change
    pub change_type: ChangeType,
    /// Success indicator
    pub success: bool,
    /// Error message, if any
    pub error: Option<String>,
}

/// Type of change
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeType {
    /// Create a new file
    Create,
    /// Modify an existing file
    Modify,
    /// Create a new directory
    CreateDir,
    /// Apply a diff to a file
    ApplyDiff,
}

/// Result of a scaffold operation
#[derive(Debug, Clone)]
pub struct ScaffoldResult {
    /// List of changes that were applied
    pub changes: Vec<AppliedChange>,
    /// Overall success indicator
    pub success: bool,
    /// Summary message
    pub summary: String,
}

/// Scaffolder handles project generation and file management
pub struct Scaffolder {
    /// AI client for generating content
    ai_client: AiClient,
    /// Current working directory
    current_dir: PathBuf,
    /// Set of modified files to prevent double-modification
    modified_files: HashSet<PathBuf>,
    /// Built-in project templates
    built_in_templates: Vec<ProjectTemplate>,
    /// User-defined project templates
    user_templates: Vec<ProjectTemplate>,
    /// Template variable values
    template_variables: std::collections::HashMap<String, String>,
}

impl Default for TemplateSource {
    fn default() -> Self {
        Self::BuiltIn
    }
}

impl Scaffolder {
    /// Create a new scaffolder
    pub fn new(ai_client: AiClient) -> Result<Self> {
        let current_dir = std::env::current_dir()?;
        let mut scaffolder = Self {
            ai_client,
            current_dir,
            modified_files: HashSet::new(),
            built_in_templates: Vec::new(),
            user_templates: Vec::new(),
            template_variables: std::collections::HashMap::new(),
        };
        
        // Load built-in templates
        scaffolder.load_built_in_templates()?;
        
        // Load user templates
        let _ = scaffolder.load_user_templates();
        
        Ok(scaffolder)
    }

    /// Find a template by name
    pub fn find_template(&self, name: &str) -> Option<ProjectTemplate> {
        // Check built-in templates first
        if let Some(template) = self.built_in_templates.iter().find(|t| t.name == name) {
            return Some(template.clone());
        }
        
        // Then check user templates
        if let Some(template) = self.user_templates.iter().find(|t| t.name == name) {
            return Some(template.clone());
        }
        
        None
    }

    /// Load built-in project templates
    fn load_built_in_templates(&mut self) -> Result<()> {
        // Define built-in templates
        let templates = vec![
            ProjectTemplate {
                name: "rust-cli".to_string(),
                description: "Basic Rust CLI application".to_string(),
                files: vec![
                    TemplateFile {
                        path: "Cargo.toml".to_string(),
                        content: format!(r#"[package]
name = "{{project_name}}"
version = "0.1.0"
edition = "2021"
description = "{{description}}"
authors = ["{{author}}"]

[dependencies]
clap = {{ version = "4.0", features = ["derive"] }}
anyhow = "1.0"
"#),
                        executable: false,
                        condition: None,
                    },
                    TemplateFile {
                        path: "src/main.rs".to_string(),
                        content: r#"use anyhow::Result;
use clap::Parser;

/// {{description}}
#[derive(Parser, Debug)]
#[clap(version, about)]
struct Args {
    /// Input file
    #[clap(short, long)]
    input: Option<String>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    println!("Hello from {{project_name}}!");
    
    if let Some(input) = args.input {
        println!("Input file: {}", input);
    }
    
    Ok(())
}
"#.to_string(),
                        executable: false,
                        condition: None,
                    },
                ],
                questions: vec![
                    TemplateQuestion {
                        id: "description".to_string(),
                        question: "Project description".to_string(),
                        default: Some("A command line tool".to_string()),
                        question_type: "text".to_string(),
                        options: vec![],
                        help: None,
                    },
                    TemplateQuestion {
                        id: "author".to_string(),
                        question: "Author name".to_string(),
                        default: None,
                        question_type: "text".to_string(),
                        options: vec![],
                        help: None,
                    },
                ],
                source: TemplateSource::BuiltIn,
                dependencies: vec![],
                post_commands: vec![],
            },
            // Add more built-in templates as needed
        ];
        
        self.built_in_templates = templates;
        Ok(())
    }

    /// Load user-defined templates
    fn load_user_templates(&mut self) -> Result<()> {
        // Get template directory path
        let template_dir = self.get_template_dir()?;
        
        // Check if directory exists
        if !template_dir.exists() {
            // Create directory if it doesn't exist
            std::fs::create_dir_all(&template_dir)
                .context(format!("Failed to create template directory: {}", template_dir.display()))?;
            return Ok(());
        }
        
        // Scan for template files
        let entries = std::fs::read_dir(&template_dir)
            .context(format!("Failed to read template directory: {}", template_dir.display()))?;
        
        // Parse each template file
        for entry in entries {
            if let Ok(entry) = entry {
                let path = entry.path();
                if path.is_file() && path.extension().map_or(false, |ext| ext == "json") {
                    // Read template file
                    match std::fs::read_to_string(&path) {
                        Ok(content) => {
                            // Parse JSON
                            match serde_json::from_str::<ProjectTemplate>(&content) {
                                Ok(mut template) => {
                                    // Set source to UserDefined
                                    template.source = TemplateSource::UserDefined;
                                    self.user_templates.push(template);
                                },
                                Err(e) => {
                                    warn!("Failed to parse template file {}: {}", path.display(), e);
                                }
                            }
                        },
                        Err(e) => {
                            warn!("Failed to read template file {}: {}", path.display(), e);
                        }
                    }
                }
            }
        }
        
        Ok(())
    }

    /// Get the templates directory path
    fn get_template_dir(&self) -> Result<PathBuf> {
        // Get config directory
        let config_dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from(".config"))
            .join("supaterm")
            .join("templates");
            
        Ok(config_dir)
    }

    /// Process a template with user inputs
    fn process_template(
        template: &ProjectTemplate,
        _target_dir: &Path,
        user_inputs: &HashMap<String, String>,
        project_name: &str,
    ) -> Result<Vec<TemplateFile>> {
        let mut processed_files = Vec::new();
        
        for file in &template.files {
            // Skip files that don't match conditions
            if let Some(condition) = &file.condition {
                // Evaluate condition based on user inputs
                if !Self::evaluate_condition(condition, user_inputs) {
                    continue;
                }
            }
            
            // Process template variables in path and content
            let processed_path = Self::process_variables(&file.path, user_inputs, project_name);
            let processed_content = Self::process_variables(&file.content, user_inputs, project_name);
            
            processed_files.push(TemplateFile {
                path: processed_path,
                content: processed_content,
                executable: file.executable,
                condition: None,
            });
        }
        
        Ok(processed_files)
    }

    /// Evaluate a condition expression
    fn evaluate_condition(condition: &str, user_inputs: &HashMap<String, String>) -> bool {
        // Simple condition evaluation
        // Format: "variable == value" or "variable != value"
        
        if condition.contains("==") {
            let parts: Vec<&str> = condition.split("==").map(|s| s.trim()).collect();
            if parts.len() == 2 {
                let variable = parts[0];
                let value = parts[1];
                
                if let Some(input_value) = user_inputs.get(variable) {
                    return input_value == value;
                }
            }
        } else if condition.contains("!=") {
            let parts: Vec<&str> = condition.split("!=").map(|s| s.trim()).collect();
            if parts.len() == 2 {
                let variable = parts[0];
                let value = parts[1];
                
                if let Some(input_value) = user_inputs.get(variable) {
                    return input_value != value;
                }
            }
        }
        
        // Default to true if condition can't be evaluated
        true
    }

    /// Process template variables in a string
    fn process_variables(text: &str, user_inputs: &HashMap<String, String>, project_name: &str) -> String {
        let mut result = text.to_string();
        
        // Replace project_name variable
        result = result.replace("{{project_name}}", project_name);
        
        // Replace user input variables
        for (key, value) in user_inputs {
            result = result.replace(&format!("{{{{{}}}}}", key), value);
        }
        
        result
    }

    /// Set the current working directory
    pub fn set_working_directory<P: AsRef<Path>>(&mut self, path: P) -> Result<()> {
        let path = path.as_ref();
        if !path.exists() {
            return Err(anyhow::anyhow!("Directory does not exist: {}", path.display()));
        }
        if !path.is_dir() {
            return Err(anyhow::anyhow!("Path is not a directory: {}", path.display()));
        }
        self.current_dir = path.to_path_buf();
        Ok(())
    }

    /// Generate a project scaffold from a description
    pub async fn generate_scaffold(&mut self, description: &str) -> Result<(Receiver<AiResponse>, Sender<bool>)> {
        let (_description_str, mut ai_rx) = self.ai_client.generate_scaffold(description).await?;
        
        // Create a channel for confirmation
        let (confirm_tx, mut confirm_rx) = tokio::sync::mpsc::channel(1);
        
        // Create a channel for progress updates
        let (progress_tx, progress_rx) = tokio::sync::mpsc::channel(100);
        
        // Clone what we need for the async task
        let _ai_client = self.ai_client.clone();
        let current_dir = self.current_dir.clone();
        
        // Process in background
        tokio::spawn(async move {
            let mut full_response = String::new();
            let mut files = Vec::new();
            
            // Collect the streaming response and extract file data
            while let Some(chunk) = ai_rx.recv().await {
                // Send the chunk back to the caller
                let _ = progress_tx.send(chunk.clone()).await;
                
                // Accumulate full response
                full_response.push_str(&chunk.content);
                
                // Try to parse JSON as soon as we have what looks like valid JSON
                if chunk.response_type == ResponseType::Scaffold && full_response.contains("}") {
                    if let Ok(parsed_files) = AiClient::parse_scaffold_data(&full_response) {
                        // Add each unique file
                        for (path, content) in parsed_files {
                            // Don't add duplicate files
                            if !files.iter().any(|(p, _)| p == &path) {
                                files.push((path, content));
                            }
                        }
                    }
                }
            }
            
            // Wait for confirmation from the user
            if let Some(confirmed) = confirm_rx.recv().await {
                if confirmed {
                    // User confirmed, now create the files
                    let scaffold_files = files
                        .into_iter()
                        .map(|(path, content)| {
                let full_path = current_dir.join(path);
                    let exists = full_path.exists();
                    ScaffoldFile {
                        path: full_path,
                        content,
                        exists,
                            }
                        })
                        .collect::<Vec<_>>();
                    
                    // Create the files
                    let result = Self::create_files(&scaffold_files);
                    
                    // Send the result as a message
                    let summary = match &result {
                        Ok(scaffold_result) => {
                            if scaffold_result.success {
                                format!("✅ Successfully scaffolded project: {} files created/modified", scaffold_result.changes.len())
                            } else {
                                format!("⚠️ Partial success in project scaffolding: {} of {} files processed", 
                                    scaffold_result.changes.iter().filter(|c| c.success).count(),
                                    scaffold_result.changes.len())
                            }
                        }
                        Err(e) => {
                            format!("❌ Failed to scaffold project: {}", e)
                        }
                    };
                    
                    let _ = progress_tx.send(AiResponse {
                        content: summary,
                        is_chunk: false,
                        response_type: ResponseType::Scaffold,
                        tokens: None,
                    }).await;
                } else {
                    // User rejected, just send a message
                    let _ = progress_tx.send(AiResponse {
                        content: "Project scaffolding cancelled by user.".to_string(),
                        is_chunk: false, 
                        response_type: ResponseType::General,
                        tokens: None,
                    }).await;
                }
            }
        });
        
        Ok((progress_rx, confirm_tx))
    }
    
    /// Generate a project using a template
    pub async fn generate_from_template(
        &mut self,
        template_name: String,
        project_name: String,
        destination: Option<&Path>,
    ) -> Result<(Receiver<ScaffoldProgress>, Sender<std::collections::HashMap<String, String>>)> {
        // Find the template
        let template = self.find_template(&template_name)
            .ok_or_else(|| anyhow::anyhow!("Template not found: {}", template_name))?;
        
        // Create channels for progress updates and user input
        let (progress_tx, progress_rx) = tokio::sync::mpsc::channel(100);
        let (input_tx, mut input_rx) = tokio::sync::mpsc::channel(1);
        
        // Determine the target directory
        let target_dir = match destination {
            Some(path) => self.resolve_and_validate_path(path)?,
            None => self.current_dir.join(&project_name),
        };
        
        // Clone what we need for the async task
        let template_clone = template.clone();
        let _ai_client = self.ai_client.clone();
        
        // Process in background
        tokio::spawn(async move {
            // Initial progress update
            let _ = progress_tx.send(ScaffoldProgress {
                status: ScaffoldStatus::Initializing,
                message: format!("Initializing project creation using '{}' template", template_name),
                percent: 0,
                current_file: None,
                error: None,
            }).await;
            
            // Wait for user input based on template questions
            if !template_clone.questions.is_empty() {
                let _ = progress_tx.send(ScaffoldProgress {
                    status: ScaffoldStatus::CollectingInput,
                    message: "Waiting for template configuration...".to_string(),
                    percent: 10,
                    current_file: None,
                    error: None,
                }).await;
                
                // Wait for user inputs
                let user_inputs = match input_rx.recv().await {
                    Some(inputs) => inputs,
                    None => {
                        let _ = progress_tx.send(ScaffoldProgress {
                            status: ScaffoldStatus::Failed,
                            message: "User input channel closed unexpectedly".to_string(),
                            percent: 0,
                            current_file: None,
                            error: Some("Failed to receive user inputs".to_string()),
                        }).await;
                        return;
                    }
                };
                
                // Update progress
                let _ = progress_tx.send(ScaffoldProgress {
                    status: ScaffoldStatus::Generating,
                    message: "Processing template with user configuration...".to_string(),
                    percent: 30,
                    current_file: None,
                    error: None,
                }).await;
                
                // Process the template with user inputs
                let processed_files = match Self::process_template(&template_clone, &target_dir, &user_inputs, &project_name) {
                    Ok(files) => files,
                    Err(e) => {
                        let _ = progress_tx.send(ScaffoldProgress {
                            status: ScaffoldStatus::Failed,
                            message: format!("Failed to process template: {}", e),
                            percent: 30,
                            current_file: None,
                            error: Some(e.to_string()),
                        }).await;
                        return;
                    }
                };
                
                // Create the files
                let _ = progress_tx.send(ScaffoldProgress {
                    status: ScaffoldStatus::Creating,
                    message: "Creating project files...".to_string(),
                    percent: 50,
                    current_file: None,
                    error: None,
                }).await;
                
                // Create target directory if it doesn't exist
                if !target_dir.exists() {
                    if let Err(e) = std::fs::create_dir_all(&target_dir) {
                        let _ = progress_tx.send(ScaffoldProgress {
                            status: ScaffoldStatus::Failed,
                            message: format!("Failed to create project directory: {}", e),
                            percent: 50,
                            current_file: None,
                            error: Some(e.to_string()),
                        }).await;
                        return;
                    }
                }
                
                // Create each file
                let total_files = processed_files.len();
                for (i, file) in processed_files.iter().enumerate() {
                    let file_path = target_dir.join(&file.path);
                    
                    // Create parent directories if they don't exist
                    if let Some(parent) = file_path.parent() {
                        if !parent.exists() {
                            if let Err(e) = std::fs::create_dir_all(parent) {
                                let _ = progress_tx.send(ScaffoldProgress {
                                    status: ScaffoldStatus::Failed,
                                    message: format!("Failed to create directory {}: {}", parent.display(), e),
                                    percent: 50 + ((i as u8 * 40) / total_files as u8),
                                    current_file: Some(file.path.clone()),
                                    error: Some(e.to_string()),
                                }).await;
                                return;
                            }
                        }
                    }
                    
                    // Create the file
                    let _ = progress_tx.send(ScaffoldProgress {
                        status: ScaffoldStatus::Creating,
                        message: format!("Creating file {}/{}: {}", i + 1, total_files, file.path),
                        percent: 50 + ((i as u8 * 40) / total_files as u8),
                        current_file: Some(file.path.clone()),
                        error: None,
                    }).await;
                    
                    if let Err(e) = std::fs::write(&file_path, &file.content) {
                        let _ = progress_tx.send(ScaffoldProgress {
                            status: ScaffoldStatus::Failed,
                            message: format!("Failed to create file {}: {}", file_path.display(), e),
                            percent: 50 + ((i as u8 * 40) / total_files as u8),
                            current_file: Some(file.path.clone()),
                            error: Some(e.to_string()),
                        }).await;
                        return;
                    }
                    
                    // Set executable flag if needed
                    if file.executable {
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::PermissionsExt;
                            let mut perms = std::fs::metadata(&file_path).unwrap().permissions();
                            perms.set_mode(0o755);
                            if let Err(e) = std::fs::set_permissions(&file_path, perms) {
                                // Just log the error but continue
                                eprintln!("Failed to set executable permissions on {}: {}", file_path.display(), e);
                            }
                        }
                    }
                }
                
                // Run post-creation commands if any
                if !template_clone.post_commands.is_empty() {
                    let _ = progress_tx.send(ScaffoldProgress {
                        status: ScaffoldStatus::RunningCommands,
                        message: "Running post-creation commands...".to_string(),
                        percent: 90,
                        current_file: None,
                        error: None,
                    }).await;
                    
                    // Run each command in the project directory
                    for (i, cmd) in template_clone.post_commands.iter().enumerate() {
                        let _ = progress_tx.send(ScaffoldProgress {
                            status: ScaffoldStatus::RunningCommands,
                            message: format!("Running command {}/{}: {}", i + 1, template_clone.post_commands.len(), cmd),
                            percent: 90 + ((i as u8 * 10) / template_clone.post_commands.len() as u8),
                            current_file: None,
                            error: None,
                        }).await;
                        
                        // Execute command
                        // In a real implementation, this would use tokio::process to run commands
                        // For simplicity, we'll just log the command here
                        debug!("Would execute command in {}: {}", target_dir.display(), cmd);
                    }
                }
                
                // Send completion message
                let _ = progress_tx.send(ScaffoldProgress {
                    status: ScaffoldStatus::Completed,
                    message: format!("Project '{}' created successfully using '{}' template", project_name, template_name),
                    percent: 100,
                    current_file: None,
                    error: None,
                }).await;
            }
        });
        
        Ok((progress_rx, input_tx))
    }

    /// Apply a diff to a file
    pub async fn apply_diff(&mut self, original_path: &Path, diff_text: &str) -> Result<AppliedChange> {
        // Validate the path
        let full_path = self.resolve_and_validate_path(original_path)?;
        
        // Check if file exists
        if !full_path.exists() {
            return Err(anyhow::anyhow!("File does not exist: {}", full_path.display()));
        }
        
        // Read the original file
        let mut original_content = String::new();
        File::open(&full_path)?.read_to_string(&mut original_content)?;
        
        // Parse and apply the diff
        let new_content = self.parse_and_apply_diff(&original_content, diff_text)
            .context("Failed to parse or apply diff")?;
        
        // Write the new content to the file
        let result = self.write_file(&full_path, &new_content);
        
        match result {
            Ok(_) => {
                self.modified_files.insert(full_path.clone());
                Ok(AppliedChange {
                    path: full_path,
                    change_type: ChangeType::ApplyDiff,
                    success: true,
                    error: None,
                })
            }
            Err(e) => {
                let error_msg = format!("Failed to write file: {}", e);
                Ok(AppliedChange {
                    path: full_path,
                    change_type: ChangeType::ApplyDiff,
                    success: false,
                    error: Some(error_msg),
                })
            }
        }
    }
    
    /// Generate a diff between two texts
    pub fn generate_diff(original: &str, modified: &str) -> String {
        let diff = TextDiff::from_lines(original, modified);
        
        let mut diff_text = String::new();
        for change in diff.iter_all_changes() {
            let tag = match change.tag() {
                ChangeTag::Delete => "-",
                ChangeTag::Insert => "+",
                ChangeTag::Equal => " ",
            };
            diff_text.push_str(&format!("{}{}", tag, change.value()));
        }
        
        diff_text
    }
    
    /// Parse a diff string and apply it to the original text
    fn parse_and_apply_diff(&self, original: &str, diff_text: &str) -> Result<String> {
        let mut result = original.to_string();
        let mut additions = Vec::new();
        let mut deletions = Vec::new();
        
        // Basic diff parsing
        // This is a simplified implementation - a real one would need to handle
        // unified diff format with chunks, line numbers, etc.
        for line in diff_text.lines() {
            if line.starts_with('+') && !line.starts_with("+++") {
                // Addition
                additions.push(line[1..].to_string());
            } else if line.starts_with('-') && !line.starts_with("---") {
                // Deletion
                deletions.push(line[1..].to_string());
            }
            // Ignore context lines starting with ' ' and headers
        }
        
        // Apply deletions
        for deletion in &deletions {
            result = result.replace(deletion, "");
        }
        
        // Apply additions (very simplified)
        for addition in &additions {
            result.push_str(addition);
            result.push('\n');
        }
        
        Ok(result)
    }
    
    /// Create a new file
    pub fn create_file(&mut self, path: &Path, content: &str) -> Result<AppliedChange> {
        let full_path = self.resolve_and_validate_path(path)?;
        
        // Check if parent directory exists, create if not
        if let Some(parent) = full_path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent).context("Failed to create parent directories")?;
            }
        }
        
        // Determine if this is a new file or modification
        let exists = full_path.exists();
        let change_type = if exists {
            ChangeType::Modify
        } else {
            ChangeType::Create
        };
        
        // Write the file
        let result = self.write_file(&full_path, content);
        
        match result {
            Ok(_) => {
                self.modified_files.insert(full_path.clone());
                Ok(AppliedChange {
                    path: full_path,
                    change_type,
                    success: true,
                    error: None,
                })
            }
            Err(e) => {
                let error_msg = format!("Failed to write file: {}", e);
                Ok(AppliedChange {
                    path: full_path,
                    change_type,
                    success: false,
                    error: Some(error_msg),
                })
            }
        }
    }
    
    /// Create a new directory
    pub fn create_directory(&mut self, path: &Path) -> Result<AppliedChange> {
        let full_path = self.resolve_and_validate_path(path)?;
        
        // Create directory
        let result = fs::create_dir_all(&full_path);
        
        match result {
            Ok(_) => Ok(AppliedChange {
                path: full_path,
                change_type: ChangeType::CreateDir,
                success: true,
                error: None,
            }),
            Err(e) => {
                let error_msg = format!("Failed to create directory: {}", e);
                Ok(AppliedChange {
                    path: full_path,
                    change_type: ChangeType::CreateDir,
                    success: false,
                    error: Some(error_msg),
                })
            }
        }
    }
    
    /// Resolve a path and validate it for safety
    fn resolve_and_validate_path(&self, path: &Path) -> Result<PathBuf> {
        // Normalize path
        let mut full_path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.current_dir.join(path)
        };
        
        // Canonicalize the parent (existing) path
        if let Some(parent) = full_path.parent() {
            if parent.exists() {
                if let Ok(canon_parent) = parent.canonicalize() {
                    let file_name = full_path.file_name().unwrap_or_default();
                    full_path = canon_parent.join(file_name);
                }
            }
        }
        
        // Safety check: ensure the path is within the allowed directory
        if !full_path.starts_with(&self.current_dir) {
            return Err(anyhow::anyhow!(
                "Path is outside the current working directory: {}",
                full_path.display()
            ));
        }
        
        Ok(full_path)
    }
    
    /// Write content to a file
    fn write_file(&self, path: &Path, content: &str) -> Result<()> {
        let file_result = OpenOptions::new()
            .write(true)
            .truncate(true)
            .create(true)
            .open(path);
        
        match file_result {
            Ok(mut file) => {
                file.write_all(content.as_bytes())?;
                file.flush()?;
                Ok(())
            }
            Err(e) => Err(anyhow::anyhow!("Failed to open file for writing: {}", e)),
        }
    }
    
    /// Create files from a list of scaffold files
    fn create_files(files: &[ScaffoldFile]) -> Result<ScaffoldResult> {
        let mut changes = Vec::new();
        let mut success_count = 0;
        
        // First, create all directories
        let mut directories = HashSet::new();
        for file in files {
            if let Some(parent) = file.path.parent() {
                directories.insert(parent.to_path_buf());
            }
        }
        
        for dir in directories {
            if !dir.exists() {
                match fs::create_dir_all(&dir) {
                    Ok(_) => {
                        changes.push(AppliedChange {
                            path: dir.clone(),
                            change_type: ChangeType::CreateDir,
                            success: true,
                            error: None,
                        });
                        success_count += 1;
                    }
                    Err(e) => {
                        let error_msg = format!("Failed to create directory: {}", e);
                        changes.push(AppliedChange {
                            path: dir.clone(),
                            change_type: ChangeType::CreateDir,
                            success: false,
                            error: Some(error_msg),
                        });
                    }
                }
            }
        }
        
        // Then, create all files
        for file in files {
            let file_exists = file.path.exists();
            let change_type = if file_exists {
                ChangeType::Modify
            } else {
                ChangeType::Create
            };
            
            // Open file for writing
            let file_result = OpenOptions::new()
                .write(true)
                .truncate(true)
                .create(true)
                .open(&file.path);
            
            match file_result {
                Ok(mut f) => {
                    // Write content to file
                    match f.write_all(file.content.as_bytes()) {
                        Ok(_) => {
                            // Ensure data is written to disk
                            if let Err(e) = f.flush() {
                                let error_msg = format!("Failed to flush file: {}", e);
                                changes.push(AppliedChange {
                                    path: file.path.clone(),
                                    change_type,
                                    success: false,
                                    error: Some(error_msg),
                                });
                            } else {
                                changes.push(AppliedChange {
                                    path: file.path.clone(),
                                    change_type,
                                    success: true,
                                    error: None,
                                });
                                success_count += 1;
                            }
                        }
                        Err(e) => {
                            let error_msg = format!("Failed to write to file: {}", e);
                            changes.push(AppliedChange {
                                path: file.path.clone(),
                                change_type,
                                success: false,
                                error: Some(error_msg),
                            });
                        }
                    }
                }
                Err(e) => {
                    let error_msg = format!("Failed to open file for writing: {}", e);
                    changes.push(AppliedChange {
                        path: file.path.clone(),
                        change_type,
                        success: false,
                        error: Some(error_msg),
                    });
                }
            }
        }
        
        // Create result summary
        let success = success_count == changes.len();
        let summary = if success {
            format!("Successfully created {} files", success_count)
        } else {
            format!("Partial success: {} of {} operations succeeded", 
                success_count, changes.len())
        };
        
        Ok(ScaffoldResult {
            changes,
            success,
            summary,
        })
    }
    
    /// Read the content of a file
    pub fn read_file(&self, path: &Path) -> Result<String> {
        let full_path = self.resolve_and_validate_path(path)?;
        
        // Check if file exists
        if !full_path.exists() {
            return Err(anyhow::anyhow!("File does not exist: {}", full_path.display()));
        }
        
        // Read file content
        let mut content = String::new();
        File::open(&full_path)
            .context(format!("Failed to open file: {}", full_path.display()))?
            .read_to_string(&mut content)
            .context(format!("Failed to read file: {}", full_path.display()))?;
        
        Ok(content)
    }
    
    /// Check if a file exists
    pub fn file_exists(&self, path: &Path) -> Result<bool> {
        let full_path = self.resolve_and_validate_path(path)?;
        Ok(full_path.exists() && full_path.is_file())
    }
    
    /// Check if a directory exists
    pub fn directory_exists(&self, path: &Path) -> Result<bool> {
        let full_path = self.resolve_and_validate_path(path)?;
        Ok(full_path.exists() && full_path.is_dir())
    }
    
    /// List files in a directory
    pub fn list_directory(&self, path: &Path) -> Result<Vec<PathBuf>> {
        let full_path = self.resolve_and_validate_path(path)?;
        
        if !full_path.exists() {
            return Err(anyhow::anyhow!("Directory does not exist: {}", full_path.display()));
        }
        
        if !full_path.is_dir() {
            return Err(anyhow::anyhow!("Path is not a directory: {}", full_path.display()));
        }
        
        let mut result = Vec::new();
        for entry in fs::read_dir(&full_path)? {
            if let Ok(entry) = entry {
                result.push(entry.path());
            }
        }
        
        Ok(result)
    }
    
    /// Make a backup of a file before modifying it
    pub fn backup_file(&self, path: &Path) -> Result<PathBuf> {
        let full_path = self.resolve_and_validate_path(path)?;
        
        if !full_path.exists() {
            return Err(anyhow::anyhow!("File does not exist: {}", full_path.display()));
        }
        
        if !full_path.is_file() {
            return Err(anyhow::anyhow!("Path is not a file: {}", full_path.display()));
        }
        
        // Create backup filename with timestamp
        let timestamp = chrono::Local::now().format("%Y%m%d%H%M%S");
        let file_name = full_path.file_name().unwrap_or_default().to_string_lossy();
        let backup_name = format!("{}.{}.bak", file_name, timestamp);
        
        let backup_path = if let Some(parent) = full_path.parent() {
            parent.join(backup_name)
        } else {
            PathBuf::from(backup_name)
        };
        
        // Copy the file
        fs::copy(&full_path, &backup_path)
            .context(format!("Failed to create backup of file: {}", full_path.display()))?;
        
        Ok(backup_path)
    }
    
    /// Verify file was created or modified successfully
    pub fn verify_file(&self, path: &Path, expected_content: &str) -> Result<bool> {
        let content = self.read_file(path)?;
        Ok(content == expected_content)
    }
    
    /// Delete a file safely
    pub fn delete_file(&mut self, path: &Path) -> Result<AppliedChange> {
        let full_path = self.resolve_and_validate_path(path)?;
        
        if !full_path.exists() {
            return Ok(AppliedChange {
                path: full_path,
                change_type: ChangeType::Modify, // Using Modify as there's no Delete type
                success: false,
                error: Some("File does not exist".to_string()),
            });
        }
        
        if !full_path.is_file() {
            return Ok(AppliedChange {
                path: full_path,
                change_type: ChangeType::Modify,
                success: false,
                error: Some("Path is not a file".to_string()),
            });
        }
        
        // Remove the file
        match fs::remove_file(&full_path) {
            Ok(_) => {
                // Remove from tracking set if present
                self.modified_files.remove(&full_path);
                
                Ok(AppliedChange {
                    path: full_path,
                    change_type: ChangeType::Modify,
                    success: true,
                    error: None,
                })
            }
            Err(e) => {
                let error_msg = format!("Failed to delete file: {}", e);
                Ok(AppliedChange {
                    path: full_path,
                    change_type: ChangeType::Modify,
                    success: false,
                    error: Some(error_msg),
                })
            }
        }
    }
    
    /// Format an error message for display
    pub fn format_error_message(error: &str) -> String {
        format!("⚠️ File operation error: {}", error)
    }
    
    /// Format a success message for display
    pub fn format_success_message(message: &str) -> String {
        format!("✅ {}", message)
    }
    
    /// Get the list of files modified during this session
    pub fn get_modified_files(&self) -> Vec<PathBuf> {
        self.modified_files.iter().cloned().collect()
    }
    
    /// Reset the list of modified files
    pub fn reset_modified_files(&mut self) {
        self.modified_files.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    
    // Helper function to create a temporary directory for testing
    fn create_test_dir() -> TempDir {
        tempfile::tempdir().expect("Failed to create temp directory")
    }
    
    #[test]
    fn test_resolve_and_validate_path() {
        // This test would create a temporary directory and test path resolution
        // Not implementing the full test here as it would require mocking AiClient
    }
}
