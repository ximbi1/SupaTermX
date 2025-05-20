use anyhow::{Context, Result};
use chrono::{DateTime, Local, Utc};
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::time::sleep;

use crate::terminal::{self, TerminalOutput};

/// Default directory for session storage
const DEFAULT_SESSION_DIR: &str = ".supaterm/sessions";

/// Default log file extension
const LOG_EXTENSION: &str = "term";

/// Maximum number of sessions in history (0 = unlimited)
const MAX_SESSION_HISTORY: usize = 50;

/// Default replay speed (1.0 = normal speed)
const DEFAULT_REPLAY_SPEED: f32 = 1.0;

/// Terminal session event types
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SessionEventType {
    /// Input from user
    Input,
    /// Output from terminal
    Output,
    /// Command executed
    Command,
    /// System event
    System,
    /// Resize event
    Resize,
}

/// Complete session data including events and metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionData {
    /// Session metadata and information
    pub info: SessionInfo,
    /// Session events
    pub events: Vec<SessionEvent>,
}

/// Status information for session replay
#[derive(Debug, Clone)]
pub struct ReplayStatus {
    /// Current replay state
    pub state: ReplayState,
    /// Current event position (0-based)
    pub position: usize,
    /// Total number of events
    pub total_events: usize,
    /// Session information
    pub session_info: SessionInfo,
    /// Current event being processed (if any)
    pub current_event: Option<SessionEvent>,
    /// Progress percentage (0-100)
    pub progress_percent: u8,
    /// Error message (if any)
    pub error: Option<String>,
}

/// Asciicast format header for asciinema compatibility
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsciicastHeader {
    /// Version of the asciicast format
    pub version: u8,
    /// Terminal width
    pub width: u16,
    /// Terminal height
    pub height: u16,
    /// Unix timestamp when recording started
    pub timestamp: i64,
    /// Total duration in seconds
    pub duration: f64,
    /// Title of the recording
    pub title: String,
    /// Environment variables
    pub env: HashMap<String, String>,
}

/// Single event in a terminal session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEvent {
    /// Type of event
    pub event_type: SessionEventType,
    /// Timestamp when the event occurred
    pub timestamp: DateTime<Utc>,
    /// Event content (depends on event type)
    pub content: String,
    /// Additional metadata for the event
    pub metadata: HashMap<String, String>,
}

/// Session metadata and state information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    /// Unique identifier for the session
    pub id: String,
    /// User-friendly name of the session
    pub name: String,
    /// When the session was created
    pub created_at: DateTime<Utc>,
    /// When the session was last updated
    pub updated_at: DateTime<Utc>,
    /// Total duration of the session in seconds
    pub duration: u64,
    /// Shell used in the session
    pub shell: String,
    /// Working directory when the session started
    pub working_directory: String,
    /// Tags for organizing sessions
    pub tags: Vec<String>,
    /// Whether this session is complete
    pub is_complete: bool,
    /// Number of commands in the session
    pub command_count: usize,
    /// Session file path
    #[serde(skip)]
    pub file_path: Option<PathBuf>,
}

/// Session sharing options
#[derive(Debug, Clone)]
pub struct SharingOptions {
    /// Include terminal output
    pub include_output: bool,
    /// Include timing information
    pub include_timing: bool,
    /// Include user input
    pub include_input: bool,
    /// Include system events
    pub include_system_events: bool,
    /// Export format (json, asciicast, text, etc.)
    pub format: String,
    /// Whether to encrypt the shared content
    pub encrypt: bool,
    /// Password for encryption (if enabled)
    pub password: Option<String>,
    /// Expiration time in seconds (0 = never)
    pub expiration: u64,
}

/// Terminal session export format
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportFormat {
    /// JSON format
    Json,
    /// Text format
    Text,
    /// Asciicast format (for asciinema)
    Asciicast,
    /// Markdown format
    Markdown,
    /// HTML format
    Html,
}

impl From<&str> for ExportFormat {
    fn from(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "json" => ExportFormat::Json,
            "text" => ExportFormat::Text,
            "asciicast" => ExportFormat::Asciicast,
            "markdown" | "md" => ExportFormat::Markdown,
            "html" => ExportFormat::Html,
            _ => ExportFormat::Text, // Default to text
        }
    }
}

/// Session replay state
#[derive(Debug, Clone)]
pub enum ReplayState {
    /// Not currently replaying
    Stopped,
    /// Replay is paused
    Paused,
    /// Replay is in progress
    Playing,
    /// Replay is jumping to a specific position
    Seeking,
}

/// Session manager handles terminal session recording, replaying, and sharing
pub struct SessionManager {
    /// Current session information
    current_session: Option<SessionInfo>,
    /// Events in the current session
    current_events: Vec<SessionEvent>,
    /// Path to the session directory
    session_dir: PathBuf,
    /// Whether session recording is active
    recording: bool,
    /// Whether this is a replay session
    is_replay: bool,
    /// Sender for terminal output
    output_tx: Option<Sender<TerminalOutput>>,
    /// Replay state
    replay_state: ReplayState,
    /// Replay speed multiplier
    replay_speed: f32,
    /// Current position in replay
    replay_position: usize,
    /// Start time of the session
    session_start: Option<Instant>,
    /// Lock for thread safety
    lock: Arc<Mutex<()>>,
}

impl SessionManager {
    /// Create a new session manager
    pub fn new() -> Result<Self> {
        // Get home directory
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let session_dir = home.join(DEFAULT_SESSION_DIR);
        
        // Create session directory if it doesn't exist
        fs::create_dir_all(&session_dir)
            .context(format!("Failed to create session directory: {}", session_dir.display()))?;
        
        Ok(Self {
            current_session: None,
            current_events: Vec::new(),
            session_dir,
            recording: false,
            is_replay: false,
            output_tx: None,
            replay_state: ReplayState::Stopped,
            replay_speed: DEFAULT_REPLAY_SPEED,
            replay_position: 0,
            session_start: None,
            lock: Arc::new(Mutex::new(())),
        })
    }
    
    /// Start a new recording session
    pub fn start_recording(&mut self, session_name: &str, shell: &str) -> Result<()> {
        // Use a block to limit the scope of the lock
        {
            let _lock = self.lock.lock().unwrap();
            
            if self.recording {
                return Err(anyhow::anyhow!("Already recording a session"));
            }
            
            // Generate a unique ID for the session
            let id = format!("{}-{}", 
                chrono::Utc::now().format("%Y%m%d%H%M%S"),
                uuid::Uuid::new_v4().simple());
            
            // Get current working directory
            let working_directory = std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| ".".to_string());
            
            // Create session info
            let session_info = SessionInfo {
                id,
                name: session_name.to_string(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
                duration: 0,
                shell: shell.to_string(),
                working_directory,
                tags: Vec::new(),
                is_complete: false,
                command_count: 0,
                file_path: None,
            };
            
            // Store session info
            self.current_session = Some(session_info);
            self.current_events.clear();
            self.recording = true;
            self.session_start = Some(Instant::now());
        } // Lock is released here
        
        // Log system start event - now we can borrow self mutably
        self.add_event(SessionEventType::System, "Session started", None)?;
        
        info!("Started recording session: {}", session_name);
        Ok(())
    }
    
    /// Stop the current recording session
    pub fn stop_recording(&mut self) -> Result<SessionInfo> {
        let recording;
        {
            let _lock = self.lock.lock().unwrap();
            recording = self.recording;
        }
        
        if !recording {
            return Err(anyhow::anyhow!("No active recording session"));
        }
        
        // Now we can add the event without holding the lock
        self.add_event(SessionEventType::System, "Session ended", None)?;
        
        // Get session info and save within a new lock scope
        let saved_info;
        {
            let _lock = self.lock.lock().unwrap();
            
            // Get the session name before saving
            let name = self.current_session.as_ref()
                .map(|s| s.name.clone())
                .ok_or_else(|| anyhow::anyhow!("No active session"))?;
                
            // Calculate session duration
            let duration = self.session_start
                .map(|start| start.elapsed().as_secs())
                .unwrap_or(0);
            
            // Update session info
            if let Some(session) = &mut self.current_session {
                session.updated_at = Utc::now();
                session.duration = duration;
                session.is_complete = true;
            }
            
            drop(_lock); // Drop the immutable borrow before calling save_session
            saved_info = self.save_session()?;
            
            // Update state
            self.recording = false;
            self.session_start = None;
            
            info!("Stopped recording session: {}", name);
        }
        
        Ok(saved_info)
    }
    
    /// Add an event to the current session
    pub fn add_event(
        &mut self, 
        event_type: SessionEventType, 
        content: &str,
        metadata: Option<HashMap<String, String>>,
    ) -> Result<()> {
        if !self.recording {
            // Silently ignore events if not recording
            return Ok(());
        }
        
        let metadata = metadata.unwrap_or_default();
        let event_type_clone = event_type.clone(); // Clone here since we need it twice
        
        // Create event
        let event = SessionEvent {
            event_type: event_type_clone,
            timestamp: Utc::now(),
            content: content.to_string(),
            metadata,
        };
        
        // Add to events list
        self.current_events.push(event);
        
        // Update command count if this is a command event
        if let Some(session) = &mut self.current_session {
            if event_type == SessionEventType::Command {
                session.command_count += 1;
            }
        }
        
        Ok(())
    }
    
    /// Record terminal input
    pub fn record_input(&mut self, input: &str) -> Result<()> {
        let mut metadata = HashMap::new();
        if let Ok(cwd) = std::env::current_dir() {
            metadata.insert("cwd".to_string(), cwd.to_string_lossy().to_string());
        }
        
        self.add_event(SessionEventType::Input, input, Some(metadata))
    }
    
    /// Record terminal output
    pub fn record_output(&mut self, output: &TerminalOutput) -> Result<()> {
        let mut metadata = HashMap::new();
        metadata.insert("is_control".to_string(), output.is_control.to_string());
        
        self.add_event(SessionEventType::Output, &output.content, Some(metadata))
    }
    
    /// Record a command execution
    pub fn record_command(&mut self, command: &str) -> Result<()> {
        let mut metadata = HashMap::new();
        if let Ok(cwd) = std::env::current_dir() {
            metadata.insert("cwd".to_string(), cwd.to_string_lossy().to_string());
        }
        
        // Get timestamp
        let timestamp = Local::now().format("%H:%M:%S").to_string();
        metadata.insert("timestamp".to_string(), timestamp);
        
        self.add_event(SessionEventType::Command, command, Some(metadata))
    }
    
    /// Record a terminal resize event
    pub fn record_resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        let mut metadata = HashMap::new();
        metadata.insert("cols".to_string(), cols.to_string());
        metadata.insert("rows".to_string(), rows.to_string());
        
        self.add_event(SessionEventType::Resize, &format!("{}x{}", cols, rows), Some(metadata))
    }
    
    /// Save the current session to disk
    fn save_session(&mut self) -> Result<SessionInfo> {
        if let Some(session) = &mut self.current_session {
            // Create filename
            let filename = format!("{}-{}.{}", 
                session.created_at.format("%Y%m%d%H%M%S"),
                session.id,
                LOG_EXTENSION);
            
            let file_path = self.session_dir.join(&filename);
            session.file_path = Some(file_path.clone());
            
            // Create session data structure
            let session_data = SessionData {
                info: session.clone(),
                events: self.current_events.clone(),
            };
            
            // Serialize to JSON
            let json = serde_json::to_string_pretty(&session_data)
                .context("Failed to serialize session data")?;
            
            // Write to file
            fs::write(&file_path, json)
                .context(format!("Failed to write session file: {}", file_path.display()))?;
            
            info!("Saved session to {}", file_path.display());
            Ok(session.clone())
        } else {
            Err(anyhow::anyhow!("No active session to save"))
        }
    }
    
    /// List available session files
    pub fn list_sessions(&self) -> Result<Vec<SessionInfo>> {
        let mut sessions = Vec::new();
        
        // Read directory
        for entry in fs::read_dir(&self.session_dir)? {
            if let Ok(entry) = entry {
                let path = entry.path();
                
                // Check if it's a session file
                if path.is_file() && 
                   path.extension().map_or(false, |ext| ext == LOG_EXTENSION) {
                    if let Ok(info) = self.load_session_info(&path) {
                        let mut session_info = info;
                        session_info.file_path = Some(path);
                        sessions.push(session_info);
                    }
                }
            }
        }
        
        // Sort by created date, newest first
        sessions.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        
        // Limit to max history if needed
        if MAX_SESSION_HISTORY > 0 && sessions.len() > MAX_SESSION_HISTORY {
            sessions.truncate(MAX_SESSION_HISTORY);
        }
        
        Ok(sessions)
    }
    
    /// Load session info from a file
    fn load_session_info(&self, path: &Path) -> Result<SessionInfo> {
        // Read file
        let mut content = String::new();
        File::open(path)
            .context(format!("Failed to open session file: {}", path.display()))?
            .read_to_string(&mut content)
            .context(format!("Failed to read session file: {}", path.display()))?;
        
        // Parse JSON
        let session_data: SessionData = serde_json::from_str(&content)
            .context(format!("Failed to parse session file: {}", path.display()))?;
        
        Ok(session_data.info)
    }
    
    /// Load a complete session from a file
    pub fn load_session(&self, path: &Path) -> Result<SessionData> {
        // Read file
        let mut content = String::new();
        File::open(path)
            .context(format!("Failed to open session file: {}", path.display()))?
            .read_to_string(&mut content)
            .context(format!("Failed to read session file: {}", path.display()))?;
        
        // Parse JSON
        let mut session_data: SessionData = serde_json::from_str(&content)
            .context(format!("Failed to parse session file: {}", path.display()))?;
        
        // Set file path
        session_data.info.file_path = Some(path.to_path_buf());
        
        // Validate loaded data
        self.validate_session_data(&session_data)?;
        
        Ok(session_data)
    }
    
    /// Validate session data integrity
    fn validate_session_data(&self, data: &SessionData) -> Result<()> {
        // Check for required fields
        if data.info.id.is_empty() {
            return Err(anyhow::anyhow!("Session ID is missing"));
        }
        
        // Check for events
        if data.events.is_empty() {
            warn!("Session has no events");
        }
        
        // Check event timestamps
        for event in &data.events {
            if event.timestamp > Utc::now() {
                warn!("Event has future timestamp: {:?}", event);
            }
        }
        
        Ok(())
    }
    
    /// Set the output channel for replaying sessions
    pub fn set_output_channel(&mut self, tx: Sender<TerminalOutput>) {
        self.output_tx = Some(tx);
    }
    
    /// Start replaying a session
    pub async fn start_replay(
        &mut self, 
        session_path: &Path
    ) -> Result<Receiver<ReplayStatus>> {
        let _lock = self.lock.lock().unwrap();
        
        if self.is_replay {
            return Err(anyhow::anyhow!("Already replaying a session"));
        }
        
        drop(_lock); // Drop the immutable borrow before mutably borrowing `self`
        
        // Load session
        let session_data = self.load_session(session_path)?;
        
        // Verify we have an output channel
        if self.output_tx.is_none() {
            return Err(anyhow::anyhow!("No output channel set for replay"));
        }
        
        // Create status channel
        let (status_tx, status_rx) = tokio::sync::mpsc::channel(100);
        
        // Set up for replay
        self.current_session = Some(session_data.info.clone());
        self.current_events = session_data.events.clone();
        self.is_replay = true;
        self.replay_state = ReplayState::Playing;
        self.replay_position = 0;
        
        // Clone what we need for the async task
        let events = self.current_events.clone();
        let output_tx = self.output_tx.clone().unwrap();
        let replay_speed = self.replay_speed;
        let session_info = session_data.info.clone();
        
        // Run replay in background task
        let replay_lock = self.lock.clone();
        
        tokio::spawn(async move {
            // Send initial status update
            let _ = status_tx.send(ReplayStatus {
                state: ReplayState::Playing,
                position: 0,
                total_events: events.len(),
                session_info: session_info.clone(),
                current_event: None,
                progress_percent: 0,
                error: None,
            }).await;
            
            let mut prev_time: Option<DateTime<Utc>> = None;
            
            // Process each event
            for (pos, event) in events.iter().enumerate() {
                // Check for pause/stop
                {
                    let lock = replay_lock.lock().unwrap();
                    // Drop the lock immediately after checking state
                }
                
                // Calculate delay based on timestamp difference
                if let Some(prev) = prev_time {
                    let time_diff = event.timestamp.signed_duration_since(prev);
                    let delay_ms = time_diff.num_milliseconds() as u64;
                    
                    // Scale by speed and ensure non-negative
                    if delay_ms > 0 {
                        let scaled_delay = (delay_ms as f32 / replay_speed) as u64;
                        if scaled_delay > 0 {
                            sleep(Duration::from_millis(scaled_delay)).await;
                        }
                    }
                }
                
                // Store current time for next iteration
                prev_time = Some(event.timestamp);
                
                // Process the event
                match event.event_type {
                    SessionEventType::Output => {
                        // Create a terminal output from the event
                        let is_control = event.metadata.get("is_control")
                            .map(|v| v == "true")
                            .unwrap_or(false);
                        
                        let output = TerminalOutput {
                            content: event.content.clone(),
                            is_control,
                        };
                        
                        // Send to the terminal output channel
                        let _ = output_tx.send(output).await;
                    },
                    SessionEventType::Input => {
                        // For inputs, we just send a terminal output for display
                        let output = TerminalOutput {
                            content: format!("Input: {}", event.content),
                            is_control: false,
                        };
                        
                        let _ = output_tx.send(output).await;
                    },
                    SessionEventType::Command => {
                        // For commands, send as terminal output
                        let default_timestamp = "".to_string();
                        let timestamp = event.metadata.get("timestamp")
                                                    .unwrap_or(&default_timestamp);
                        
                        let output = TerminalOutput {
                            content: format!("[{}] $ {}", timestamp, event.content),
                            is_control: false,
                        };
                        
                        let _ = output_tx.send(output).await;
                    },
                    _ => {
                        // Skip other event types
                    }
                }
                
                // Calculate progress
                let progress = ((pos as f32 + 1.0) / events.len() as f32 * 100.0) as u8;
                
                // Send status update
                let _ = status_tx.send(ReplayStatus {
                    state: ReplayState::Playing,
                    position: pos + 1,
                    total_events: events.len(),
                    session_info: session_info.clone(),
                    current_event: Some(event.clone()),
                    progress_percent: progress,
                    error: None,
                }).await;
            }
            
            // Send completion status
            let _ = status_tx.send(ReplayStatus {
                state: ReplayState::Stopped,
                position: events.len(),
                total_events: events.len(),
                session_info: session_info.clone(),
                current_event: None,
                progress_percent: 100,
                error: None,
            }).await;
        });
        
        Ok(status_rx)
    }
    
    /// Pause or resume the replay
    pub fn toggle_replay_pause(&mut self) -> Result<ReplayState> {
        let _lock = self.lock.lock().unwrap();
        
        if !self.is_replay {
            return Err(anyhow::anyhow!("No active replay session"));
        }
        
        // Toggle state
        self.replay_state = match self.replay_state {
            ReplayState::Playing => ReplayState::Paused,
            ReplayState::Paused => ReplayState::Playing,
            _ => return Err(anyhow::anyhow!("Invalid replay state for toggle")),
        };
        
        Ok(self.replay_state.clone())
    }
    
    /// Change replay speed
    pub fn set_replay_speed(&mut self, speed: f32) -> Result<f32> {
        let _lock = self.lock.lock().unwrap();
        
        if speed <= 0.0 {
            return Err(anyhow::anyhow!("Speed must be positive"));
        }
        
        self.replay_speed = speed;
        Ok(speed)
    }
    
    /// Stop the current replay
    pub fn stop_replay(&mut self) -> Result<()> {
        let _lock = self.lock.lock().unwrap();
        
        if !self.is_replay {
            return Err(anyhow::anyhow!("No active replay session"));
        }
        
        // Reset replay state
        self.is_replay = false;
        self.replay_state = ReplayState::Stopped;
        self.replay_position = 0;
        
        Ok(())
    }
    
    /// Export a session to a file in the specified format
    pub fn export_session(&self, session_path: &Path, output_path: &Path, format: ExportFormat) -> Result<PathBuf> {
        // Load the session
        let session_data = self.load_session(session_path)?;
        
        // Create output based on format
        let output = match format {
            ExportFormat::Json => self.export_as_json(&session_data)?,
            ExportFormat::Text => self.export_as_text(&session_data)?,
            ExportFormat::Asciicast => self.export_as_asciicast(&session_data)?,
            ExportFormat::Markdown => self.export_as_markdown(&session_data)?,
            ExportFormat::Html => self.export_as_html(&session_data)?,
        };
        
        // Write to file
        fs::write(output_path, output)
            .context(format!("Failed to write export file: {}", output_path.display()))?;
        
        Ok(output_path.to_path_buf())
    }
    
    // Export helper methods
    
    /// Export session as JSON
    fn export_as_json(&self, session: &SessionData) -> Result<String> {
        serde_json::to_string_pretty(session)
            .context("Failed to serialize session to JSON")
    }
    
    /// Export session as plain text
    fn export_as_text(&self, session: &SessionData) -> Result<String> {
        let mut output = String::new();
        
        // Add header
        output.push_str(&format!("Terminal Session: {}\n", session.info.name));
        output.push_str(&format!("Recorded: {}\n", session.info.created_at));
        output.push_str(&format!("Duration: {} seconds\n", session.info.duration));
        output.push_str("----------------------------------------\n\n");
        
        // Add events
        for event in &session.events {
            match event.event_type {
                SessionEventType::Command => {
                    let default_timestamp = "".to_string();
                    let timestamp = event.metadata.get("timestamp")
                                            .unwrap_or(&default_timestamp);
                    
                    output.push_str(&format!("[{}] $ {}\n", timestamp, event.content));
                },
                SessionEventType::Output => {
                    output.push_str(&format!("{}\n", event.content));
                },
                SessionEventType::Input => {
                    // Skip input events or show them differently
                },
                _ => {
                    // Skip other event types
                }
            }
        }
        
        Ok(output)
    }
    
    /// Export session as asciicast format
    fn export_as_asciicast(&self, session: &SessionData) -> Result<String> {
        let mut events = Vec::new();
        let mut prev_time: Option<DateTime<Utc>> = None;
        let start_time = session.info.created_at;
        
        // Add header
        let header = AsciicastHeader {
            version: 2,
            width: 80,
            height: 24,
            timestamp: start_time.timestamp(),
            duration: session.info.duration as f64,
            title: session.info.name.clone(),
            env: HashMap::new(),
        };
        
        // Convert events to asciicast format
        for event in &session.events {
            if ![SessionEventType::Output, SessionEventType::Command].contains(&event.event_type) {
                continue;
            }
            
            let time_offset = if let Some(prev) = prev_time {
                let diff = event.timestamp.signed_duration_since(prev);
                diff.num_milliseconds() as f64 / 1000.0
            } else {
                0.0
            };
            
            // Only include output and command events
            match event.event_type {
                SessionEventType::Output => {
                    // Format the event as an asciicast entry
                    let asciicast_event = vec![
                        time_offset,
                         0.0,
                        event.content.parse::<f64>().unwrap_or(0.0)
                    ];
                    
                    events.push(serde_json::to_string(&asciicast_event)?);
                },
                SessionEventType::Command => {
                    // Format the command as input event
                    let asciicast_event = vec![
                        time_offset,
                         1.0,
                        event.content.parse::<f64>().unwrap_or(0.0)
                    ];
                    
                    events.push(serde_json::to_string(&asciicast_event)?);
                },
                _ => {
                    // Skip other event types
                }
            }
            
            prev_time = Some(event.timestamp);
        }
        
        // Create asciicast content
        let header_json = serde_json::to_string(&header)?;
        let mut output = header_json + "\n";
        
        // Add all events
        for event in events {
            output.push_str(&event);
            output.push('\n');
        }
        
        Ok(output)
    }
    
    /// Export session as markdown
    fn export_as_markdown(&self, session: &SessionData) -> Result<String> {
        let mut output = String::new();
        
        // Add header
        output.push_str(&format!("# Terminal Session: {}\n\n", session.info.name));
        output.push_str(&format!("- **Recorded**: {}\n", session.info.created_at));
        output.push_str(&format!("- **Duration**: {} seconds\n", session.info.duration));
        output.push_str(&format!("- **Shell**: {}\n", session.info.shell));
        output.push_str(&format!("- **Working Directory**: {}\n\n", session.info.working_directory));
        
        // Add events
        output.push_str("## Terminal Transcript\n\n");
        output.push_str("```\n");
        
        for event in &session.events {
            match event.event_type {
                SessionEventType::Command => {
                    let default_timestamp = "".to_string();
                    let timestamp = event.metadata.get("timestamp")
                                            .unwrap_or(&default_timestamp);
                    
                    output.push_str(&format!("[{}] $ {}\n", timestamp, event.content));
                },
                SessionEventType::Output => {
                    output.push_str(&format!("{}\n", event.content));
                },
                _ => {
                    // Skip other event types
                }
            }
        }
        
        output.push_str("```\n");
        Ok(output)
    }
    
    /// Export session as HTML
    fn export_as_html(&self, session: &SessionData) -> Result<String> {
        let mut output = String::new();
        
        // Add HTML header
        output.push_str("<!DOCTYPE html>\n");
        output.push_str("<html lang=\"en\">\n");
        output.push_str("<head>\n");
        output.push_str("  <meta charset=\"UTF-8\">\n");
        output.push_str("  <meta name=\"viewport\" content=\"width=device-width, initial-scale=1.0\">\n");
        output.push_str(&format!("  <title>Terminal Session: {}</title>\n", session.info.name));
        output.push_str("  <style>\n");
        output.push_str("    body { font-family: monospace; background: #222; color: #eee; margin: 20px; }\n");
        output.push_str("    h1, h2 { color: #8ff; }\n");
        output.push_str("    .info { color: #9cf; margin-bottom: 20px; }\n");
        output.push_str("    .terminal { background: #000; border-radius: 5px; padding: 10px; white-space: pre-wrap; }\n");
        output.push_str("    .command { color: #ff9; font-weight: bold; }\n");
        output.push_str("    .timestamp { color: #999; }\n");
        output.push_str("  </style>\n");
        output.push_str("</head>\n");
        output.push_str("<body>\n");
        
        // Add session information
        output.push_str(&format!("  <h1>Terminal Session: {}</h1>\n", session.info.name));
        output.push_str("  <div class=\"info\">\n");
        output.push_str(&format!("    <p><strong>Recorded:</strong> {}</p>\n", session.info.created_at));
        output.push_str(&format!("    <p><strong>Duration:</strong> {} seconds</p>\n", session.info.duration));
        output.push_str(&format!("    <p><strong>Shell:</strong> {}</p>\n", session.info.shell));
        output.push_str(&format!("    <p><strong>Working Directory:</strong> {}</p>\n", session.info.working_directory));
        output.push_str("  </div>\n");
        
        // Add terminal content
        output.push_str("  <h2>Terminal Transcript</h2>\n");
        output.push_str("  <div class=\"terminal\">\n");
        
        for event in &session.events {
            match event.event_type {
                SessionEventType::Command => {
                    let default_timestamp = "".to_string();
                    let timestamp = event.metadata.get("timestamp")
                                            .unwrap_or(&default_timestamp);
                    
                    output.push_str(&format!("<span class=\"timestamp\">[{}]</span> <span class=\"command\">$ {}</span>\n",
                        timestamp, htmlescape::encode_minimal(&event.content)));
                },
                SessionEventType::Output => {
                    output.push_str(&format!("{}\n", htmlescape::encode_minimal(&event.content)));
                },
                _ => {
                    // Skip other event types
                }
            }
        }
        
        output.push_str("  </div>\n");
        output.push_str("</body>\n");
        output.push_str("</html>\n");
        
        Ok(output)
    }
    
    /// Share a session using the specified options
    pub async fn share_session(&self, session_path: &Path, options: SharingOptions) -> Result<String> {
        // Load the session
        let session_data = self.load_session(session_path)?;
        
        // Create a filtered copy of the session based on sharing options
        let mut filtered_data = session_data.clone();
        
        // Filter events based on sharing options
        if !options.include_output {
            filtered_data.events.retain(|e| e.event_type != SessionEventType::Output);
        }
        
        if !options.include_input {
            filtered_data.events.retain(|e| e.event_type != SessionEventType::Input);
        }
        
        if !options.include_system_events {
            filtered_data.events.retain(|e| e.event_type != SessionEventType::System);
        }
        
        // Export to the specified format
        let format = ExportFormat::from(options.format.as_str());
        let content = match format {
            ExportFormat::Json => self.export_as_json(&filtered_data)?,
            ExportFormat::Text => self.export_as_text(&filtered_data)?,
            ExportFormat::Asciicast => self.export_as_asciicast(&filtered_data)?,
            ExportFormat::Markdown => self.export_as_markdown(&filtered_data)?,
            ExportFormat::Html => self.export_as_html(&filtered_data)?,
        };
        
        // Encrypt if needed
        let final_content = if options.encrypt {
            if let Some(password) = &options.password {
                // This is a simplified encryption using base64 encoding
                // In a real implementation, use proper encryption
                let encoded = base64::encode(&content);
                format!("ENCRYPTED:{}", encoded)
            } else {
                return Err(anyhow::anyhow!("Password required for encryption"));
            }
        } else {
            content
        };
        
        // In a real implementation, we would upload to a sharing service
        // For now, we'll just return the content
        Ok(final_content)
    }
    
    /// Get session stats
    pub fn get_session_stats(&self, session_path: &Path) -> Result<HashMap<String, String>> {
        // Load the session
        let session_data = self.load_session(session_path)?;
        let mut stats = HashMap::new();
        
        // Basic info
        stats.insert("name".to_string(), session_data.info.name.clone());
        stats.insert("created_at".to_string(), session_data.info.created_at.to_string());
        stats.insert("duration".to_string(), session_data.info.duration.to_string());
        stats.insert("shell".to_string(), session_data.info.shell.clone());
        
        // Event counts
        let total_events = session_data.events.len();
        stats.insert("total_events".to_string(), total_events.to_string());
        
        let command_count = session_data.events.iter()
            .filter(|e| e.event_type == SessionEventType::Command)
            .count();
        stats.insert("command_count".to_string(), command_count.to_string());
        
        let output_count = session_data.events.iter()
            .filter(|e| e.event_type == SessionEventType::Output)
            .count();
        stats.insert("output_count".to_string(), output_count.to_string());
        
        // Calculate total output size in bytes
        let output_size: usize = session_data.events.iter()
            .filter(|e| e.event_type == SessionEventType::Output)
            .map(|e| e.content.len())
            .sum();
        stats.insert("output_size_bytes".to_string(), output_size.to_string());
        
        Ok(stats)
    }
    
    /// Delete a session file
    pub fn delete_session(&self, session_path: &Path) -> Result<()> {
        // Validate the path
        if !session_path.exists() {
            return Err(anyhow::anyhow!("Session file not found: {}", session_path.display()));
        }
        
        // Check that it's a session file
        if !session_path.is_file() || 
           session_path.extension().map_or(true, |ext| ext != LOG_EXTENSION) {
            return Err(anyhow::anyhow!("Not a valid session file: {}", session_path.display()));
        }
        
        // Delete the file
        fs::remove_file(session_path)
            .context(format!("Failed to delete session file: {}", session_path.display()))?;
        
        info!("Deleted session file: {}", session_path.display());
        Ok(())
    }
}
