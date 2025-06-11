use anyhow::{Context, Result};
use log::{error, warn};
use std::sync::Arc;
use openai_api_rs::v1::{
    api::Client,
    chat_completion::{
        ChatCompletionMessage,
        ChatCompletionRequest,
        Content, // Updated from ChatMessageContent
        MessageRole, // Updated from ChatRole
    },
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::env;
use std::time::Duration;
use tokio::sync::mpsc::{self, Receiver, Sender};

/// Maximum number of retries for API calls
const MAX_RETRIES: usize = 3;
/// Initial backoff time in milliseconds
const INITIAL_BACKOFF_MS: u64 = 500;
/// Message history context window size
const MAX_CONTEXT_MESSAGES: usize = 10;
/// Default model to use
const DEFAULT_MODEL: &str = "gpt-4";
/// Default timeout for streaming requests in seconds
const STREAMING_TIMEOUT_SECS: u64 = 60;
/// Max number of command suggestions to generate
const MAX_COMMAND_SUGGESTIONS: usize = 5;

/// Response from the AI assistant
#[derive(Debug, Clone)]
pub struct AiResponse {
    /// The actual text response
    pub content: String,
    /// Whether this is a streaming chunk or complete response
    pub is_chunk: bool,
    /// Type of response (general, code, error analysis, etc.)
    pub response_type: ResponseType,
    /// Token count for this response (if available)
    pub tokens: Option<usize>,
}

/// Type of AI response
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResponseType {
    /// General text response
    General,
    /// Code or command suggestion
    Code,
    /// Project scaffold suggestion
    Scaffold,
    /// Error analysis
    ErrorAnalysis,
    /// Diff suggestion
    Diff,
    /// Command suggestion
    CommandSuggestion,
    /// Autocompletion
    Autocompletion,
}

/// Configuration for the AI client
#[derive(Debug, Clone)]
pub struct Config {
    /// OpenAI API key
    pub api_key: String,
    /// Model to use
    pub model: String,
    /// Organization ID (optional)
    pub organization_id: Option<String>,
    /// Maximum tokens to generate
    pub max_tokens: Option<u32>,
    /// Temperature (randomness)
    pub temperature: Option<f64>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            api_key: env::var("OPENAI_API_KEY").unwrap_or_default(),
            model: DEFAULT_MODEL.to_string(),
            organization_id: env::var("OPENAI_ORG_ID").ok(),
            max_tokens: Some(2048),
            temperature: Some(0.7),
        }
    }
}

/// Shell command suggestion
#[derive(Debug, Clone)]
pub struct CommandSuggestion {
    /// The suggested command
    pub command: String,
    /// Description of what the command does
    pub description: String,
    /// Whether this is a safe command to run
    pub is_safe: bool,
    /// Estimated confidence level (0.0-1.0)
    pub confidence: f32,
}

/// Error analysis result
#[derive(Debug, Clone)]
pub struct ErrorAnalysis {
    /// Original error message
    pub error_message: String,
    /// Explanation of the error
    pub explanation: String,
    /// Suggested solutions
    pub suggestions: Vec<String>,
    /// Relevant code snippet if available
    pub code_context: Option<String>,
}

/// Message in a conversation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationMessage {
    /// Role of the sender
    pub role: String,
    /// Content of the message
    pub content: String,
}

impl std::fmt::Display for ConversationMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Role: {}, Content: {}", self.role, self.content)
    }
}

impl From<ConversationMessage> for ChatCompletionMessage {
    fn from(msg: ConversationMessage) -> Self {
        let role = match msg.role.as_str() {
            "user" => MessageRole::user,
            "assistant" => MessageRole::assistant,
            "system" => MessageRole::system,
            _ => MessageRole::user, // Default to user if unknown
        };
        
        ChatCompletionMessage {
            role,
            content: Content::Text(msg.to_string()),
            name: None,
        }
    }
}

/// AI client for interacting with OpenAI API
#[derive(Clone)]
pub struct AiClient {
    /// OpenAI client
    client: Arc<Client>,
    /// Configuration
    config: Config,
}

impl AiClient {
    /// Create a new AI client
    pub fn new(config: Config) -> Result<Self> {
        let api_key = config.api_key.clone();
        if api_key.is_empty() {
            warn!(
                "OpenAI API key not found. AI features will be disabled."
            );
        }

        // Create client with authentication even if the key is empty so the
        // rest of the application can run without AI features.
        let client = if let Some(org_id) = &config.organization_id {
            Arc::new(Client::new_with_organization(api_key.clone(), org_id.clone()))
        } else {
            Arc::new(Client::new(api_key.clone()))
        };
        Ok(Self { client, config })
    }

    /// Create a new AI client with default configuration
    pub fn new_with_defaults() -> Result<Self> {
        let config = Config::default();
        Self::new(config)
    }

    /// Process a user query and generate a response
    pub async fn process_query(
        &self,
        query: &str,
        conversation_history: &[ConversationMessage],
    ) -> Result<(String, Receiver<AiResponse>)> {
        // Create channels for streaming response
        let (tx, rx) = mpsc::channel(100);

        // Take recent conversation history to stay within context limits
        let recent_history: Vec<ConversationMessage> = conversation_history
            .iter()
            .rev()
            .take(MAX_CONTEXT_MESSAGES)
            .rev()
            .cloned()
            .collect();

        // Add user's new query
        let mut messages: Vec<ChatCompletionMessage> = recent_history
            .into_iter()
            .map(|msg| msg.into())
            .collect();

        messages.push(ChatCompletionMessage {
            role: MessageRole::user,
            content: Content::Text(query.to_string()),
            name: None,
        });

        // Create chat completion request
        let request = ChatCompletionRequest {
            model: self.config.model.clone(),
            messages,
            temperature: self.config.temperature,
            top_p: None,
            n: None,
            stream: Some(true),
            stop: None,
            max_tokens: self.config.max_tokens.map(|m| m as i64),
            presence_penalty: None,
            frequency_penalty: None,
            logit_bias: None,
            user: None,
            response_format: None,
            seed: None,
            tool_choice: None,
            tools: None,
        };

        // Clone what we need for the async task
        let client = Arc::clone(&self.client);
        let tx_clone = tx.clone();

        // Process in background
        tokio::spawn(async move {
            let result = Self::streaming_completion_with_retry(client, request, tx_clone.clone()).await;
            if let Err(e) = result {
                error!("Error in streaming completion: {}", e);
                // Send error message to stream
                let _ = tx_clone
                    .send(AiResponse {
                        content: format!("Error: {}", e),
                        is_chunk: false,
                        response_type: ResponseType::General,
                        tokens: None,
                    })
                    .await;
            }
        });

        Ok((query.to_string(), rx))
    }

    /// Generate a project scaffold from a description
    pub async fn generate_scaffold(
        &self,
        description: &str,
    ) -> Result<(String, Receiver<AiResponse>)> {
        let (tx, rx) = mpsc::channel(100);
        
        // Create a system prompt for scaffolding
        let system_prompt = "You are an assistant specialized in creating project scaffolds. \
            Generate a complete directory structure and file contents based on the user's description. \
            For each file, include the full path and content. Format your response as JSON with the following structure: \
            { \"files\": [ { \"path\": \"file/path.ext\", \"content\": \"file content\" } ] }";
        
        let messages = vec![
            ChatCompletionMessage {
            role: MessageRole::system,
                content: Content::Text(system_prompt.to_string()),
                name: None,
            },
            ChatCompletionMessage {
                role: MessageRole::user,
                content: Content::Text(description.to_string()),
                name: None,
            },
        ];

        let request = ChatCompletionRequest {
            model: self.config.model.clone(),
            messages,
            temperature: Some(0.3), // Lower temperature for more deterministic results
            top_p: None,
            n: None,
            stream: Some(true),
            stop: None,
            max_tokens: Some(4000), // Higher token limit for scaffold generation
            presence_penalty: None,
            frequency_penalty: None,
            logit_bias: None,
            user: None,
            response_format: None,
            seed: None,
            tool_choice: None,
            tools: None,
        };

        let client = Arc::clone(&self.client);
        let tx_clone = tx.clone();

        tokio::spawn(async move {
            let result = Self::streaming_completion_with_retry(client, request, tx_clone.clone()).await;
            if let Err(e) = result {
                error!("Error in scaffold generation: {}", e);
                let _ = tx_clone
                    .send(AiResponse {
                        content: format!("Error generating scaffold: {}", e),
                        is_chunk: false,
                        response_type: ResponseType::Scaffold,
                        tokens: None,
                    })
                    .await;
            }
        });

        Ok((description.to_string(), rx))
    }

    /// Analyze build/test output to identify and fix errors
    pub async fn analyze_output(
        &self,
        output: &str,
        context: &str,
    ) -> Result<(String, Receiver<AiResponse>)> {
        let (tx, rx) = mpsc::channel(100);
        
        // Create system prompt for error analysis
        let system_prompt = "You are an assistant specialized in analyzing build and test output. \
            Identify errors, explain their cause, and suggest fixes. \
            Format your response in sections: ERROR SUMMARY, ROOT CAUSES, and SUGGESTED FIXES. \
            If there are code changes, format them as diffs with clear file paths.";
        
        let user_prompt = format!("Here is the output to analyze:\n\n```\n{}\n```\n\nAdditional context:\n{}", 
            output, context);
        
        let messages = vec![
            ChatCompletionMessage {
                role: MessageRole::system,
                content: Content::Text(system_prompt.to_string()),
                name: None,
            },
            ChatCompletionMessage {
                role: MessageRole::user,
                content: Content::Text(user_prompt),
                name: None,
            },
        ];

        let request = ChatCompletionRequest {
            model: self.config.model.clone(),
            messages,
            temperature: Some(0.5),
            top_p: None,
            n: None,
            stream: Some(true),
            stop: None,
            max_tokens: self.config.max_tokens.map(|m| m as i64),
            presence_penalty: None,
            frequency_penalty: None,
            logit_bias: None,
            user: None,
            response_format: None,
            seed: None,
            tool_choice: None,
            tools: None,
        };

        let client = Arc::clone(&self.client);
        let tx_clone = tx.clone();

        tokio::spawn(async move {
            let result = Self::streaming_completion_with_retry(client, request, tx_clone.clone()).await;
            if let Err(e) = result {
                error!("Error in output analysis: {}", e);
                let _ = tx_clone
                    .send(AiResponse {
                        content: format!("Error analyzing output: {}", e),
                        is_chunk: false,
                        response_type: ResponseType::ErrorAnalysis,
                        tokens: None,
                    })
                    .await;
            }
        });

        Ok(("Analysis".to_string(), rx))
    }

    /// Suggest commands based on user input and history
    pub async fn suggest_commands(
        &self,
        partial_command: &str,
        command_history: &[String],
        current_dir: Option<&str>,
    ) -> Result<(String, Receiver<AiResponse>)> {
        let (tx, rx) = mpsc::channel(100);

        // Create system prompt for command suggestions
        let system_prompt = "You are a terminal command suggestion assistant. \
            Based on the user's partial command and command history, suggest relevant commands. \
            For each suggestion, provide the command and a brief description of what it does. \
            Format your response as JSON: { \"suggestions\": [ { \"command\": \"...\", \"description\": \"...\", \"is_safe\": true|false } ] }";

        // Prepare context with command history and working directory
        let mut context = String::new();
        if !command_history.is_empty() {
            context.push_str("Recent command history:\n");
            for (i, cmd) in command_history.iter().rev().take(10).enumerate() {
                context.push_str(&format!("{}. {}\n", i + 1, cmd));
            }
            context.push_str("\n");
        }

        if let Some(dir) = current_dir {
            context.push_str(&format!("Current directory: {}\n\n", dir));
        }

        let user_prompt = format!("{}Partial command: '{}'\n\nSuggest commands to complete or replace this input with more appropriate commands.", 
            context, partial_command);

        let messages = vec![
            ChatCompletionMessage {
                role: MessageRole::system,
                content: Content::Text(system_prompt.to_string()),
                name: None,
            },
            ChatCompletionMessage {
                role: MessageRole::user,
                content: Content::Text(user_prompt),
                name: None,
            },
        ];

        let request = ChatCompletionRequest {
            model: self.config.model.clone(),
            messages,
            temperature: Some(0.4),
            top_p: None,
            n: None,
            stream: Some(true),
            stop: None,
            max_tokens: Some(1000),
            presence_penalty: None,
            frequency_penalty: None,
            logit_bias: None,
            user: None,
            response_format: None,
            seed: None,
            tool_choice: None,
            tools: None,
        };

        let client = Arc::clone(&self.client);
        let tx_clone = tx.clone();

        tokio::spawn(async move {
            let result = Self::streaming_completion_with_retry(client, request, tx_clone.clone()).await;
            if let Err(e) = result {
                error!("Error in command suggestion: {}", e);
                let _ = tx_clone
                    .send(AiResponse {
                        content: format!("Error suggesting commands: {}", e),
                        is_chunk: false,
                        response_type: ResponseType::CommandSuggestion,
                        tokens: None,
                    })
                    .await;
            }
        });

        Ok(("Command Suggestions".to_string(), rx))
    }

    /// Provide autocompletion for partial input
    pub async fn autocomplete(
        &self,
        partial_input: &str,
        context: &str,
    ) -> Result<String> {
        // For autocompletion, we use a non-streaming quick completion to minimize latency
        let system_prompt = "You are an autocompletion assistant. Complete the user's partial input with the most likely completion. Respond with only the completion, no explanations.";
        
        let messages = vec![
            ChatCompletionMessage {
                role: MessageRole::system,
                content: Content::Text(system_prompt.to_string()),
                name: None,
            },
            ChatCompletionMessage {
                role: MessageRole::user,
                content: Content::Text(format!("Context: {}\n\nPartial input: {}\n\nComplete this input:", context, partial_input)),
                name: None,
            },
        ];

        let request = ChatCompletionRequest {
            model: self.config.model.clone(),  // Could use a smaller model for autocompletion
            messages,
            temperature: Some(0.2),  // Low temperature for deterministic completions
            top_p: None,
            n: None,
            stream: Some(false),  // Non-streaming for low latency
            stop: None,
            max_tokens: Some(100),  // Short completions only
            presence_penalty: None,
            frequency_penalty: None,
            logit_bias: None,
            user: None,
            response_format: None,
            seed: None,
            tool_choice: None,
            tools: None,
        };

        // Use a timeout to ensure autocompletion doesn't lag the UI
        let client_ref: Arc<Client> = Arc::clone(&self.client);
        let result = tokio::task::spawn_blocking(move || {
            client_ref.chat_completion(request)
        })
        .await
        .map_err(|e| anyhow::anyhow!("Task join error: {}", e))??;

        if let Some(choice) = result.choices.first() {
            if let Some(content) = &choice.message.content {
                        // Clean up the response to get just the completion
                        let completion = content.trim();
                        
                        // If the model includes the partial input in the response, remove it
                        let result = if completion.starts_with(partial_input) {
                            let remaining = &completion[partial_input.len()..];
                            remaining.trim().to_string()
                        } else {
                            completion.to_string()
                        };
                        
                        return Ok(result);
                    }
                }
                Ok("".to_string())
        }
    

    /// Analyze shell error messages with context
    pub async fn analyze_shell_error(
        &self,
        error_message: &str,
        command: &str,
        directory: Option<&str>,
    ) -> Result<ErrorAnalysis> {
        // Create system prompt for shell error analysis
        let system_prompt = "You are an assistant specialized in analyzing shell errors and providing solutions. Respond with a JSON object containing: explanation, suggestions array, and optional code_context.";
        
        let user_prompt = format!(
            "Command: {}\nError: {}\n{}",
            command,
            error_message,
            directory.map(|d| format!("Directory: {}", d)).unwrap_or_default()
        );
        
        let messages = vec![
            ChatCompletionMessage {
                role: MessageRole::system,
                content: Content::Text(system_prompt.to_string()),
                name: None,
            },
            ChatCompletionMessage {
                role: MessageRole::user,
                content: Content::Text(user_prompt),
                name: None,
            },
        ];

        let request = ChatCompletionRequest {
            model: self.config.model.clone(),
            messages,
            temperature: Some(0.3),
            top_p: None,
            n: None,
            stream: Some(false),  // Non-streaming for this analytical task
            stop: None,
            max_tokens: Some(1000),
            presence_penalty: None,
            frequency_penalty: None,
            logit_bias: None,
            user: None,
            response_format: None,
            seed: None,
            tool_choice: None,
            tools: None,
        };

        // Execute the completion request with timeout
        let client_ref: Arc<Client> = Arc::clone(&self.client);
        let result = tokio::task::spawn_blocking(move || {
            client_ref.chat_completion(request)
        })
        .await
        .map_err(|e| anyhow::anyhow!("Task join error: {}", e))??;

        if let Some(choice) = result.choices.first() {
            if let Some(content) = &choice.message.content {
                        // Parse the JSON response
                        let json_start = content.find('{').unwrap_or(0);
                        let json_end = content.rfind('}').map(|pos| pos + 1).unwrap_or(content.len());
                        let json_content = &content[json_start..json_end];
                        
                        let parsed: Value = match serde_json::from_str(json_content) {
                            Ok(parsed) => parsed,
                            Err(_) => {
                                // Fallback for non-JSON responses
                                return Ok(ErrorAnalysis {
                                    error_message: error_message.to_string(),
                                    explanation: content.to_string(),
                                    suggestions: vec![],
                                    code_context: None,
                                });
                            }
                        };
                        
                        // Extract fields from JSON
                        let explanation = parsed.get("explanation")
                            .and_then(|e| e.as_str())
                            .unwrap_or("No explanation provided")
                            .to_string();
                            
                        let mut suggestions = Vec::new();
                        if let Some(suggs) = parsed.get("suggestions").and_then(|s| s.as_array()) {
                            for sugg in suggs {
                                if let Some(s) = sugg.as_str() {
                                    suggestions.push(s.to_string());
                                }
                            }
                        }
                        
                        // Extract code context if available
                        let code_context = parsed.get("code_context")
                            .and_then(|c| c.as_str())
                            .map(|s| s.to_string());
                        
                        return Ok(ErrorAnalysis {
                            error_message: error_message.to_string(),
                            explanation,
                            suggestions,
                            code_context,
                        });
                    }
                }
                
                // Fallback for unexpected response format
                Ok(ErrorAnalysis {
                    error_message: error_message.to_string(),
                    explanation: "Could not parse AI response.".to_string(),
                    suggestions: vec!["Try a different command.".to_string()],
                    code_context: None,
                })
        }
    

    /// Generate a diff between original code and requested changes
    pub async fn generate_diff(
        &self,
        original: &str,
        description: &str,
    ) -> Result<(String, Receiver<AiResponse>)> {
        let (tx, rx) = mpsc::channel(100);
        
        // Create system prompt for diff generation
        let system_prompt = "You are an assistant specialized in modifying code. \
            Generate a unified diff that applies the requested changes to the provided code. \
            Format your response as a proper diff with --- and +++ headers, @@ line numbers, and - or + line prefixes.";
        
        let user_prompt = format!("Original code:\n\n```\n{}\n```\n\nChanges to make:\n{}", 
            original, description);
        
        let messages = vec![
            ChatCompletionMessage {
                role: MessageRole::system,
                content: Content::Text(system_prompt.to_string()),
                name: None,
            },
            ChatCompletionMessage {
                role: MessageRole::user,
                content: Content::Text(user_prompt),
                name: None,
            },
        ];

        let request = ChatCompletionRequest {
            model: self.config.model.clone(),
            messages,
            temperature: Some(0.3),
            top_p: None,
            n: None,
            stream: Some(true),
            stop: None,
            max_tokens: self.config.max_tokens.map(|m| m as i64),
            presence_penalty: None,
            frequency_penalty: None,
            logit_bias: None,
            user: None,
            response_format: None,
            seed: None,
            tool_choice: None,
            tools: None,
        };

        let client = Arc::clone(&self.client);
        let tx_clone = tx.clone();

        tokio::spawn(async move {
            let result = Self::streaming_completion_with_retry(client, request, tx_clone.clone()).await;
            if let Err(e) = result {
                error!("Error in diff generation: {}", e);
                let _ = tx_clone
                    .send(AiResponse {
                        content: format!("Error generating diff: {}", e),
                        is_chunk: false,
                        response_type: ResponseType::Diff,
                        tokens: None,
                    })
                    .await;
            }
        });

        Ok(("Diff".to_string(), rx))
    }

    /// Execute a streaming chat completion with retry logic
    async fn streaming_completion_with_retry(
        client: Arc<Client>,
        request: ChatCompletionRequest,
        tx: Sender<AiResponse>,
    ) -> Result<()> {
        let mut backoff = INITIAL_BACKOFF_MS;
        let mut retries = 0;
        
        loop {
            // Try to get streaming completion
            let request_clone = request.clone();
            // Clone the client for this iteration
            let client_clone = Arc::clone(&client);
            let result = tokio::task::spawn_blocking(move || {
                client_clone.chat_completion(request_clone)
            })
            .await
            .map_err(|e| anyhow::anyhow!("Task join error: {}", e))??;
            
            // With the new API, we need to extract content from the completion
            if let Some(choice) = result.choices.first() {
                if let Some(content) = &choice.message.content {
                            // Send the full content as a response
                    let response_type = Self::determine_response_type(&content);
                    if let Err(e) = tx.send(AiResponse {
                        content: content.to_string(),
                        is_chunk: false,
                        response_type,
                        tokens: None,
                    }).await {
                        warn!("Failed to send AI response: {}", e);
                    }
                }
                
                return Ok(());
            } else {
                // Handle error case - no choices returned
                error!("No choices returned from API");
                
                if retries >= MAX_RETRIES {
                    return Err(anyhow::anyhow!("Max retries exceeded: No choices returned"));
                }
                
                // Exponential backoff
                retries += 1;
                tokio::time::sleep(Duration::from_millis(backoff)).await;
                backoff *= 2; // Double the backoff time for next retry
                
                warn!("Retrying request (attempt {}/{})", retries, MAX_RETRIES);
            }
            }
        }


    /// Determine the type of response based on content
    fn determine_response_type(content: &str) -> ResponseType {
        // Check for code blocks or inline code
        if content.contains("```") || content.contains("`") {
            return ResponseType::Code;
        }
        
        // Check for diff markers
        if (content.contains("---") && content.contains("+++")) || 
           content.contains("@@ ") || 
           (content.contains("-") && content.contains("+") && content.contains("diff")) {
            return ResponseType::Diff;
        }
        
        // Check for scaffold content (usually JSON with files)
        if content.contains("\"files\"") && 
           (content.contains("\"path\"") || content.contains("\"content\"")) {
            return ResponseType::Scaffold;
        }
        
        // Check for error analysis content
        if (content.contains("ERROR") || content.contains("Error") || content.contains("error")) &&
           (content.contains("CAUSES") || content.contains("FIXES") || content.contains("SOLUTION")) {
            return ResponseType::ErrorAnalysis;
        }
        
        // Default to general response
        ResponseType::General
    }
    
    /// Extract code blocks from a response
    pub fn extract_code_blocks(content: &str) -> Vec<String> {
        let mut blocks = Vec::new();
        let mut in_block = false;
        let mut current_block = String::new();
        
        for line in content.lines() {
            if line.trim().starts_with("```") {
                if in_block {
                    // End of block
                    blocks.push(current_block.clone());
                    current_block.clear();
                    in_block = false;
                } else {
                    // Start of block
                    in_block = true;
                }
            } else if in_block {
                current_block.push_str(line);
                current_block.push('\n');
            }
        }
        
        // Handle case where the last block wasn't closed
        if in_block && !current_block.is_empty() {
            blocks.push(current_block);
        }
        
        blocks
    }
    
    /// Parse scaffold data from JSON response
    pub fn parse_scaffold_data(json_str: &str) -> Result<Vec<(String, String)>> {
        // Try to find JSON object in the response
        let json_start = json_str.find('{').unwrap_or(0);
        let json_end = json_str.rfind('}').map(|pos| pos + 1).unwrap_or(json_str.len());
        let json_content = &json_str[json_start..json_end];
        
        // Parse JSON
        let parsed: Value = serde_json::from_str(json_content)
            .context("Failed to parse scaffold JSON")?;
        
        // Extract files
        let mut files = Vec::new();
        
        if let Some(files_array) = parsed.get("files").and_then(|f| f.as_array()) {
            for file_obj in files_array {
                if let (Some(path), Some(content)) = (
                    file_obj.get("path").and_then(|p| p.as_str()),
                    file_obj.get("content").and_then(|c| c.as_str())
                ) {
                    files.push((path.to_string(), content.to_string()));
                }
            }
        }
        
        Ok(files)
    }
    
    /// Extract commands from AI response
    pub fn extract_commands(content: &str) -> Vec<String> {
        let mut commands = Vec::new();
        
        // Look for command line patterns
        for line in content.lines() {
            let trimmed = line.trim();
            
            // Common command patterns
            if (trimmed.starts_with('$') || trimmed.starts_with('>')) && trimmed.len() > 2 {
                // Extract command without the prompt symbol
                commands.push(trimmed[1..].trim().to_string());
            } else if trimmed.starts_with("Run:") || trimmed.starts_with("Execute:") {
                // Extract command after the instruction
                if let Some(cmd) = trimmed.split(':').nth(1) {
                    commands.push(cmd.trim().to_string());
                }
            }
        }
        
        // Also look for commands in code blocks
        for block in Self::extract_code_blocks(content) {
            if !block.contains("\n") && !block.contains("=") && !block.contains("{") {
                // Likely a single-line command
                commands.push(block.trim().to_string());
            }
        }
        
        commands
    }
    
    /// Format an error message for display
    pub fn format_error(error: &str) -> String {
        format!("⚠️ AI Error: {}", error)
    }
    
    /// Check if the OpenAI API key is configured
    pub fn is_api_key_configured() -> bool {
        match env::var("OPENAI_API_KEY") {
            Ok(key) => !key.is_empty(),
            Err(_) => false,
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_determine_response_type() {
        // Test code detection
        assert_eq!(
            AiClient::determine_response_type("Here's some `inline code`"),
            ResponseType::Code
        );
        assert_eq!(
            AiClient::determine_response_type("```rust\nfn main() {}\n```"),
            ResponseType::Code
        );
        
        // Test diff detection
        assert_eq!(
            AiClient::determine_response_type("--- old\n+++ new\n@@ -1,3 +1,3 @@\n-old\n+new"),
            ResponseType::Diff
        );
        
        // Test scaffold detection
        assert_eq!(
            AiClient::determine_response_type(r#"{"files":[{"path":"src/main.rs","content":"fn main() {}"}]}"#),
            ResponseType::Scaffold
        );
        
        // Test error analysis detection
        assert_eq!(
            AiClient::determine_response_type("ERROR SUMMARY\nCompilation failed\nROOT CAUSES\nMissing semicolon"),
            ResponseType::ErrorAnalysis
        );
        
        // Test general response
        assert_eq!(
            AiClient::determine_response_type("This is a normal text response"),
            ResponseType::General
        );
    }
    
    #[test]
    fn test_extract_code_blocks() {
        let content = "Here's some code:\n```rust\nfn main() {\n    println!(\"Hello\");\n}\n```\nAnd another:\n```\nlet x = 5;\n```";
        let blocks = AiClient::extract_code_blocks(content);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0], "fn main() {\n    println!(\"Hello\");\n}\n");
        assert_eq!(blocks[1], "let x = 5;\n");
    }
}
