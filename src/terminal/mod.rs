use anyhow::{Context, Result};
use log::{debug, error, info, warn};
use portable_pty::{
    native_pty_system, CommandBuilder, PtyPair, PtySize,
    Child as PtyChild,
};
use std::io::{Read, Write};
use std::any::Any;
use std::sync::{Arc, Mutex};
use termwiz::caps::Capabilities;
use termwiz::terminal::{new_terminal, Terminal as TermwizTerminal, ScreenSize};
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio::task::JoinHandle;
use vte::{Parser, Perform, Params};
use std::path::PathBuf;
use nix::sys::signal;
use downcast_rs::Downcast;


/// Configuration for the terminal
#[derive(Debug, Clone)]
pub struct Config {
    /// The shell to spawn (e.g., "fish", "bash", "zsh")
/// The shell to spawn (e.g., "fish", "bash", "zsh")
pub shell: String,
}

/// A helper trait to allow downcasting of `Box<dyn Write + Send>`
trait AsAny {
    fn as_any(&self) -> &dyn Any;
}

impl<T: Any + Write + Send> AsAny for T {
    fn as_any(&self) -> &dyn Any {
        self
    }
}


impl Default for Config {
    fn default() -> Self {
        Self {
            shell: "fish".to_string(),
        }
    }
}

/// Represents terminal output data
#[derive(Debug, Clone)]
pub struct TerminalOutput {
    /// Raw content received from the PTY
    pub content: String,
    /// Whether this is a control sequence or regular text
    pub is_control: bool,
    /// Optional CSI parameters for control sequences
    pub csi_params: Option<Vec<u16>>,
    /// Optional action character for CSI sequences
    pub csi_action: Option<char>,
}

// A writer adapter that forwards data to a channel
struct WriterAdapter {
    tx: tokio::sync::mpsc::Sender<Vec<u8>>,
}

impl WriterAdapter {
    fn new(tx: tokio::sync::mpsc::Sender<Vec<u8>>) -> Self {
        Self { tx }
    }
}

impl Write for WriterAdapter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        // Create a copy of the buffer
        let data = buf.to_vec();
        let len = data.len();
        
        // Try to send it to the channel
        if let Err(_) = self.tx.try_send(data) {
            return Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "Channel closed"));
        }
        
        Ok(len)
    }
    
    fn flush(&mut self) -> std::io::Result<()> {
        // No way to flush a channel
        Ok(())
    }
}

// Terminal manages the PTY and provides methods to interact with it
pub struct Terminal {
    /// Configuration for this terminal instance
    config: Config,
    /// The underlying termwiz terminal (for terminal capabilities)
    terminal: Option<Box<dyn TermwizTerminal>>,
    /// Channel to send terminal output
    output_tx: Sender<TerminalOutput>,
    /// Channel to receive terminal input
    input_rx: Option<Receiver<String>>,
    /// Flag to indicate if the terminal is running
    running: Arc<Mutex<bool>>,
    /// The original terminal state, saved for restoration
    original_state: Option<OriginalState>,
    /// The PTY pair (master/slave)
    pty_pair: Option<PtyPair>,
    /// The spawned child process
    child: Option<Box<dyn PtyChild>>,
    /// Writer to the PTY master
    pty_writer: Option<Box<dyn Write + Send>>,
    /// Task handles for I/O processing
    task_handles: Vec<JoinHandle<()>>,
    /// VTE parser for terminal control sequence processing
    vte_parser: Option<Parser>,
    /// Current terminal size
    terminal_size: Arc<Mutex<PtySize>>,
}

/// Holds the original terminal state for restoration
#[derive(Debug)]
struct OriginalState {
    /// Whether the terminal was in raw mode
    raw_mode: bool,
    /// Original terminal size
    size: Option<(u16, u16)>,
}

/// VTE sequence handler that processes terminal control sequences
#[derive(Clone)]
struct VteHandler {
    output_tx: Sender<TerminalOutput>,
    terminal_size: Arc<Mutex<PtySize>>,
    current_line: String,
    /// Buffer for CSI sequences
    csi_buffer: String,
    /// Buffer for collecting ANSI escape sequences
    ansi_buffer: String,
    /// Current cursor position (row, column)
    cursor_pos: (u16, u16),
    /// Tracks if we're in the middle of a control sequence
    in_control_sequence: bool,
}
pub fn cleanup() -> Result<(), Box<dyn std::error::Error>> {
    use crossterm::{terminal, execute, cursor, style};
    use std::io::stdout;

    terminal::disable_raw_mode()?;
    execute!(
        stdout(),
        terminal::LeaveAlternateScreen,
        cursor::Show,
        style::ResetColor
    )?;
    Ok(())
}


impl VteHandler {
    fn new(output_tx: Sender<TerminalOutput>, terminal_size: Arc<Mutex<PtySize>>) -> Self {
        Self {
            output_tx,
            terminal_size,
            current_line: String::new(),
            csi_buffer: String::new(),
            ansi_buffer: String::new(),
            cursor_pos: (0, 0),
            in_control_sequence: false,
        }
    }

    /// Flush the current line buffer
    async fn flush_line(&mut self) {
        if !self.current_line.is_empty() {
            let content = std::mem::take(&mut self.current_line);
            // Use try_send instead of send for immediate flushing (non-blocking)
            let _ = self.output_tx.try_send(TerminalOutput {
                content,
                is_control: false,
                csi_params: None,
                csi_action: None,
            });
        }
    }

    /// Synchronous version of flush_line for immediate use
    fn flush_line_sync(&mut self) {
        if !self.current_line.is_empty() {
            let content = std::mem::take(&mut self.current_line);
            let _ = self.output_tx.try_send(TerminalOutput {
                content,
                is_control: false,
                csi_params: None,
                csi_action: None,
            });
        }
    }

    /// Send a control sequence to the output channel
    async fn send_control_sequence(&mut self, seq: String, params: Option<Vec<u16>>, action: Option<char>) {
        // Use try_send for immediate processing to prevent hanging
        let _ = self.output_tx.try_send(TerminalOutput {
            content: seq,
            is_control: true,
            csi_params: params,
            csi_action: action,
        });
    }

    /// Synchronous version of send_control_sequence for immediate use
    fn send_control_sequence_sync(&mut self, seq: String, params: Option<Vec<u16>>, action: Option<char>) {
        let _ = self.output_tx.try_send(TerminalOutput {
            content: seq,
            is_control: true,
            csi_params: params,
            csi_action: action,
        });
    }

    /// Process a CSI sequence and update terminal state
    fn process_csi(&mut self, params: &Params, action: char) -> Option<(Vec<u16>, char)> {
        // Convert Params to Vec<u16>
        let mut parsed_params = Vec::new();
        for param in params.iter() {
            if let Some(num) = param.first() {
                parsed_params.push(*num as u16);
            } else {
                parsed_params.push(0); // Default value for empty param
            }
        }

        // Debug output for CSI sequences
        debug!("Processing CSI: {:?} {}", parsed_params, action);

        // Handle cursor position updates based on the sequence
        match action {
            'A' => { // Cursor Up
                let n = if parsed_params.is_empty() { 1 } else { parsed_params[0] };
                if n > 0 {
                    self.cursor_pos.0 = self.cursor_pos.0.saturating_sub(n);
                }
            },
            'B' => { // Cursor Down
                let n = if parsed_params.is_empty() { 1 } else { parsed_params[0] };
                if n > 0 {
                    let size = self.terminal_size.lock().unwrap();
                    self.cursor_pos.0 = std::cmp::min(self.cursor_pos.0 + n, size.rows - 1);
                }
            },
            'C' => { // Cursor Forward
                let n = if parsed_params.is_empty() { 1 } else { parsed_params[0] };
                if n > 0 {
                    let size = self.terminal_size.lock().unwrap();
                    self.cursor_pos.1 = std::cmp::min(self.cursor_pos.1 + n, size.cols - 1);
                }
            },
            'D' => { // Cursor Back
                let n = if parsed_params.is_empty() { 1 } else { parsed_params[0] };
                if n > 0 {
                    self.cursor_pos.1 = self.cursor_pos.1.saturating_sub(n);
                }
            },
            'H' | 'f' => { // Cursor Position
                let row = if parsed_params.is_empty() { 1 } else { parsed_params[0] };
                let col = if parsed_params.len() < 2 { 1 } else { parsed_params[1] };
                self.cursor_pos = (row.saturating_sub(1), col.saturating_sub(1)); // Convert from 1-based to 0-based
            },
            'J' => { // Erase in Display
                // Implementation depends on your display model
            },
            'K' => { // Erase in Line
                // Implementation depends on your display model
            },
            'm' => { // SGR - Select Graphic Rendition
                // Color and text attribute changes
                // These should be passed through to the TUI layer
            },
            's' => { // Save Cursor Position
                // Save current position (implementation depends on your state model)
            },
            'u' => { // Restore Cursor Position
                // Restore saved position (implementation depends on your state model)
            },
            _ => {
                // Other CSI sequences
            }
        }

        Some((parsed_params, action))
    }
}
    
impl Perform for VteHandler {
    fn print(&mut self, c: char) {
        if self.in_control_sequence {
            // We're inside a control sequence - accumulate in the ANSI buffer
            self.ansi_buffer.push(c);
        } else {
            // Regular printable character - add to current line
            self.current_line.push(c);
            
            // Update cursor position
            self.cursor_pos.1 += 1;
            let size = self.terminal_size.lock().unwrap();
            if self.cursor_pos.1 >= size.cols {
                // Line wrap
                self.cursor_pos.1 = 0;
                self.cursor_pos.0 += 1;
                if self.cursor_pos.0 >= size.rows {
                    self.cursor_pos.0 = size.rows - 1; // Scrolling behavior handled elsewhere
                }
            }
        }
    }
    fn execute(&mut self, byte: u8) {
        // Handle control characters
        match byte {
            b'\r' => { // Carriage Return
                self.current_line.push('\r');
                self.cursor_pos.1 = 0; // Move cursor to start of line
        }
        b'\n' => { // Line Feed
            self.current_line.push('\n');
            let content = std::mem::take(&mut self.current_line);
            let _ = self.output_tx.try_send(TerminalOutput {
                content,
                is_control: false,
                csi_params: None,
                csi_action: None,
            });
            
            // Update cursor position
            self.cursor_pos.0 += 1;
            let size = self.terminal_size.lock().unwrap();
            if self.cursor_pos.0 >= size.rows {
                self.cursor_pos.0 = size.rows - 1; // Scrolling behavior handled elsewhere
            }
        }
        b'\t' => { // Tab
            // Add spaces to simulate tab (usually 8 spaces, but this can be configurable)
            let spaces = 8 - (self.cursor_pos.1 % 8);
            for _ in 0..spaces {
                self.current_line.push(' ');
            }
            
            // Update cursor position
            self.cursor_pos.1 += spaces;
            let size = self.terminal_size.lock().unwrap();
            if self.cursor_pos.1 >= size.cols {
                self.cursor_pos.1 = self.cursor_pos.1 % size.cols;
                self.cursor_pos.0 += 1;
                if self.cursor_pos.0 >= size.rows {
                    self.cursor_pos.0 = size.rows - 1;
                }
            }
        }
        b'\x07' => { // Bell
            // Could trigger a visual bell or sound if needed
        }
        b'\x08' => { // Backspace
            if !self.current_line.is_empty() {
                self.current_line.pop();
            }
            
            // Update cursor position
            if self.cursor_pos.1 > 0 {
                self.cursor_pos.1 -= 1;
            }
        }
        b'\x0b' | b'\x0c' => { // Vertical Tab or Form Feed
            // Treat similar to newline
            self.current_line.push('\n');
            let content = std::mem::take(&mut self.current_line);
            let _ = self.output_tx.try_send(TerminalOutput {
                content,
                is_control: false,
                csi_params: None,
                csi_action: None,
            });
            
            // Update cursor position
            self.cursor_pos.0 += 1;
            let size = self.terminal_size.lock().unwrap();
            if self.cursor_pos.0 >= size.rows {
                self.cursor_pos.0 = size.rows - 1;
            }
        }
        _ => {
            // For other control characters, add a visible representation
            if byte < 32 {
                self.current_line.push('^');
                self.current_line.push((byte + 64) as char);
                
                // Update cursor position for the two chars
                self.cursor_pos.1 += 2;
                let size = self.terminal_size.lock().unwrap();
                if self.cursor_pos.1 >= size.cols {
                    self.cursor_pos.1 %= size.cols;
                    self.cursor_pos.0 += 1;
                    if self.cursor_pos.0 >= size.rows {
                        self.cursor_pos.0 = size.rows - 1;
                    }
                }
            }
        }
    }
}

    fn hook(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        // Beginning of a device control string or other complex sequence
        self.in_control_sequence = true;
        self.ansi_buffer.clear();
        self.ansi_buffer.push_str("\x1b"); // ESC
        
        // Add the action character (e.g., 'P' for DCS)
        self.ansi_buffer.push(action);
        
        // Add any intermediates
        for &b in intermediates {
            self.ansi_buffer.push(b as char);
        }
        
        // Add parameters
        if !params.is_empty() {
            let mut param_strs = Vec::new();
            for param in params.iter() {
                let param_str: String = param.iter()
                    .map(|&n| n.to_string())
                    .collect::<Vec<_>>()
                    .join(";");
                param_strs.push(param_str);
            }
            self.ansi_buffer.push_str(&param_strs.join(";"));
        }
    }

    fn put(&mut self, byte: u8) {
        if self.in_control_sequence {
            // Add to ANSI buffer if we're in a control sequence
            self.ansi_buffer.push(byte as char);
        } else {
            // Otherwise add to current line
            self.current_line.push(byte as char);
            
            // Update cursor position
            self.cursor_pos.1 += 1;
            let size = self.terminal_size.lock().unwrap();
            if self.cursor_pos.1 >= size.cols {
                self.cursor_pos.1 = 0;
                self.cursor_pos.0 += 1;
                if self.cursor_pos.0 >= size.rows {
                    self.cursor_pos.0 = size.rows - 1;
                }
            }
        }
    }

    fn unhook(&mut self) {
        // End of a device control string or other complex sequence
        self.in_control_sequence = false;
        
        // Dispatch the accumulated control sequence
        let _ = self.output_tx.try_send(TerminalOutput {
            content: std::mem::take(&mut self.ansi_buffer),
            is_control: true,
            csi_params: None,
            csi_action: None,
        });
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        // Flush any pending content before handling the OSC sequence
        self.flush_line_sync();
        
        // Operating System Command
        let mut osc_string = String::from("\x1b]"); // ESC ]
        
        // Convert parameters to strings and join them
        for (i, param) in params.iter().enumerate() {
            if i > 0 {
                osc_string.push(';');
            }
            
            if let Ok(param_str) = std::str::from_utf8(param) {
                osc_string.push_str(param_str);
            }
        }
        
        // Add terminator
        if bell_terminated {
            osc_string.push('\x07'); // BEL
        } else {
            osc_string.push_str("\x1b\\"); // ESC \
        }
        
        // Handle specific OSC sequences
        let has_handled = match params.first() {
            Some(&[b'0']) | Some(&[b'2']) => {
                // Change window title or icon name
                if params.len() > 1 {
                    if let Ok(title) = std::str::from_utf8(params[1]) {
                        debug!("Terminal title change: {}", title);
                        // Handle title change if needed
                    }
                }
                true
            },
            Some(&[b'4']) => {
                // Change/query color palette
                true
            },
            Some(&[b'1', b'0', b'4']) => {
                // Reset color palette
                true
            },
            _ => false
        };
        
        // Send the OSC sequence to the output channel
        if !has_handled {
            let _ = self.output_tx.try_send(TerminalOutput {
                content: osc_string,
                is_control: true,
                csi_params: None,
                csi_action: None,
            });
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        // Flush any pending content before handling the CSI sequence
        self.flush_line_sync();
        
        // Control Sequence Introducer
        let mut csi_string = String::from("\x1b["); // ESC [
        
        // Add parameters
        let mut first = true;
        for param in params.iter() {
            if !first {
                csi_string.push(';');
            }
            first = false;
            
            let mut param_first = true;
            for &value in param {
                if !param_first {
                    csi_string.push(':');
                }
                param_first = false;
                csi_string.push_str(&value.to_string());
            }
        }
        
        // Add intermediates
        for &byte in intermediates {
            csi_string.push(byte as char);
        }
        
        // Add the final character
        csi_string.push(action);
        
        // Process the CSI sequence and update internal state
        let result = self.process_csi(params, action);
        
        // Send the CSI sequence to the output channel
        if let Some((params_vec, action_char)) = result {
            let _ = self.output_tx.try_send(TerminalOutput {
                content: csi_string,
                is_control: true,
                csi_params: Some(params_vec),
                csi_action: Some(action_char),
            });
        } else {
            let _ = self.output_tx.try_send(TerminalOutput {
                content: csi_string,
                is_control: true,
                csi_params: None,
                csi_action: None,
            });
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        // Flush any pending content before handling the escape sequence
        self.flush_line_sync();
        
        // Escape sequence
        let mut esc_string = String::from("\x1b"); // ESC
        
        // Add intermediates
        for &b in intermediates {
            esc_string.push(b as char);
        }
        
        // Add final byte
        esc_string.push(byte as char);
        
        // Handle specific escape sequences
        match byte {
            b'7' => {
                // Save cursor position
                debug!("Saving cursor position: {:?}", self.cursor_pos);
                // State saving implementation if needed
            },
            b'8' => {
                // Restore cursor position
                debug!("Restoring cursor position");
                // State restoration implementation if needed
            },
            b'D' => {
                // Index - move cursor down, scroll if at bottom
                self.cursor_pos.0 += 1;
                let size = self.terminal_size.lock().unwrap();
                if self.cursor_pos.0 >= size.rows {
                    self.cursor_pos.0 = size.rows - 1;
                }
            },
            b'M' => {
                // Reverse Index - move cursor up, scroll if at top
                if self.cursor_pos.0 > 0 {
                    self.cursor_pos.0 -= 1;
                }
            },
            b'c' => {
                // Reset terminal to initial state
                self.cursor_pos = (0, 0);
                // Other reset actions if needed
            },
            _ => {
                // Handle other escape sequences if needed
            }
        }
        
        // Send the escape sequence to the output channel
        let _ = self.output_tx.try_send(TerminalOutput {
            content: esc_string,
            is_control: true,
            csi_params: None,
            csi_action: None,
        });
    }
}


impl Terminal {
    /// Convert between terminal size types
    fn convert_screen_size(&self, size: &ScreenSize) -> PtySize {
        PtySize {
            rows: size.rows as u16,
            cols: size.cols as u16,
            pixel_width: size.xpixel as u16,
            pixel_height: size.ypixel as u16,
        }
    }
    pub async fn shutdown(&mut self) -> anyhow::Result<()> {
        // Add any cleanup logic here if needed
        Ok(())
    }


    /// Convert between PTY size types
    fn convert_pty_size(&self, size: &PtySize) -> ScreenSize {
        ScreenSize {
            rows: size.rows as usize,
            cols: size.cols as usize,
            xpixel: size.pixel_width as usize,
            ypixel: size.pixel_height as usize,
        }
    }
    /// Create a new terminal with the given configuration
    pub fn new(config: Config) -> Result<(Self, Receiver<TerminalOutput>, Sender<String>)> {
        let (output_tx, output_rx) = mpsc::channel(100);
        let (input_tx, input_rx) = mpsc::channel(100);

        // Default terminal size (will be updated later)
        let terminal_size = Arc::new(Mutex::new(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        }));

        let terminal = Self {
            config,
            terminal: None,
            output_tx,
            input_rx: Some(input_rx),
            running: Arc::new(Mutex::new(true)),
            original_state: None,
            pty_pair: None,
            child: None,
            pty_writer: None,
            task_handles: Vec::new(),
            vte_parser: Some(Parser::new()),
            terminal_size,
        };

        Ok((terminal, output_rx, input_tx))
    }

    /// Initialize the terminal and spawn the shell
    pub async fn init(&mut self) -> Result<()> {
        // Create a new terminal instance to get capabilities
        let caps = Capabilities::new_from_env().context("Failed to get terminal capabilities")?;
        let mut terminal = Box::new(new_terminal(caps).context("Failed to create terminal")?);

        // Save original state before making changes
        let screen_size = match terminal.get_screen_size() {
            Ok(size) => size,
            Err(e) => {
                warn!("Failed to get screen size: {}, using default", e);
                ScreenSize {
                    rows: 24,
                    cols: 80,
                    xpixel: 0,
                    ypixel: 0,
                }
            }
        };
        let cols = screen_size.cols;
        let rows = screen_size.rows;
        let original_raw_mode = false; // termwiz is always in raw mode when created

        self.original_state = Some(OriginalState {
            raw_mode: original_raw_mode, 
            size: Some((cols as u16, rows as u16)),
        });

        // Set up raw mode - termwiz's set_raw_mode() is a toggle
        if !original_raw_mode {
            match terminal.set_raw_mode() {
                Ok(_) => debug!("Successfully entered raw mode"),
                Err(e) => {
                    warn!("Failed to set raw mode: {}, terminal may not display correctly", e);
                    // Try with crossterm as a fallback
                    let _ = crossterm::terminal::enable_raw_mode();
                }
            }
        }

        // Use the size we already got above
        let (cols, rows) = (screen_size.cols, screen_size.rows);

        // Update our terminal size state
        {
            let mut size = self.terminal_size.lock().unwrap();
            size.rows = rows as u16;
            size.cols = cols as u16;
            // Pixel dimensions aren't critical, but we'll set them proportionally
            size.pixel_width = (cols as u16) * 8;  // assuming 8 pixels per character
            size.pixel_height = (rows as u16) * 16; // assuming 16 pixels per character
        }

        // Set up the PTY
        let pty_system = native_pty_system();
        
        // Create the PTY with the configured size
        let pty_size = self.terminal_size.lock().unwrap().clone();
        let pty_pair = pty_system.openpty(pty_size)
            .context("Failed to open PTY")?;
        
        // Find the path to the shell
        let shell_path = which::which(&self.config.shell)
            .unwrap_or_else(|_| {
                warn!("Could not find shell: {}, falling back to /bin/sh", self.config.shell);
                PathBuf::from("/bin/sh")
            });

        // Create command builder with the shell
        let mut cmd = CommandBuilder::new(shell_path);
        cmd.env("TERM", "xterm-256color");
        
        // Add LANG environment variable to ensure proper UTF-8 support
        cmd.env("LANG", "en_US.UTF-8");
        cmd.env("LC_ALL", "en_US.UTF-8");
        
        // Add working directory and any other needed environment variables
        if let Ok(cwd) = std::env::current_dir() {
            cmd.cwd(cwd);
        }

        // For login shell
        cmd.arg("--login");
        
        // Spawn the child process in the PTY
        let child = pty_pair.slave.spawn_command(cmd)
            .context("Failed to spawn shell process")?;
        
        // Store the child process
        self.child = Some(child);
        
        // Create a writer to the PTY master
        let writer = pty_pair.master.take_writer()
            .context("Failed to get PTY master writer")?;
        
        // Store components
        self.pty_writer = Some(writer);
        self.pty_pair = Some(pty_pair);
        self.terminal = Some(terminal);
        
        info!("Terminal initialized with {} shell", self.config.shell);
        Ok(())
    }

    /// Start processing input and output for the terminal
        /// Start processing input and output for the terminal
        pub async fn run(&mut self) -> Result<()> {
            if self.terminal.is_none() || self.pty_pair.is_none() || self.pty_writer.is_none() {
                return Err(anyhow::anyhow!("Terminal not fully initialized"));
            }
    
            let running = self.running.clone();
    
            // Obtenemos el reader del PTY
            let (mut reader, mut pty_writer) = {
                let pty_pair = self.pty_pair.as_mut().unwrap();
                let reader = pty_pair.master.try_clone_reader()
                    .context("Failed to clone PTY master reader")?;
                let writer = self.pty_writer.take().unwrap();
                (reader, writer)
            };
    
            let output_tx    = self.output_tx.clone();
            let terminal_size= self.terminal_size.clone();
    
            // 1) Forwarder de input → split_tx
            let (split_tx, mut split_rx) = mpsc::channel::<String>(100);
            let mut input_rx = self.input_rx.take().unwrap();
            {
                let running_fwd = running.clone();
                tokio::spawn(async move {
                    while *running_fwd.lock().unwrap() {
                        match input_rx.recv().await {
                            Some(cmd) => {
                                if split_tx.send(cmd).await.is_err() {
                                    break;
                                }
                            }
                            None => break,
                        }
                    }
                });
            }
    
            // 2) Task de salida (PTY → VTE → output_tx)
            let output_handle = {
                let running_out = running.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let mut parser = Parser::new();
                    let mut handler = VteHandler::new(output_tx.clone(), terminal_size.clone());
    
                    loop {
                        if !*running_out.lock().unwrap() {
                            break;
                        }
                        match reader.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                for &b in &buf[..n] {
                                    parser.advance(&mut handler, b);
                                }
                                handler.flush_line_sync();
                            }
                            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                            }
                            Err(_) => break,
                        }
                    }
                })
            };
    
            // 3) Task de input  (split_rx → PTY)
            let input_handle = {
                let running_in = running.clone();
                tokio::spawn(async move {
                    while let Some(line) = split_rx.recv().await {
                        if !*running_in.lock().unwrap() {
                            break;
                        }
                        let mut to_write = if line.ends_with('\n') {
                            line
                        } else {
                            format!("{}\n", line)
                        };
                        if let Err(_) = pty_writer.write_all(to_write.as_bytes()) {
                            break;
                        }
                        let _ = pty_writer.flush();
                    }
                })
            };
    
            // Guardamos los handles para shutdown()
            self.task_handles.push(output_handle);
            self.task_handles.push(input_handle);
    
            info!("Terminal is now running");
            Ok(())
        }
    
    /// Restore terminal to a safe state (for emergency recovery)
    pub fn emergency_restore(&mut self) -> Result<()> {
        info!("Performing emergency terminal restoration");
        
        if let Some(mut terminal) = self.terminal.take() {
            // Always try to disable raw mode - only toggle once
            match terminal.set_raw_mode() {
                Ok(_) => debug!("Successfully toggled raw mode"),
                Err(e) => warn!("Failed to toggle raw mode: {}", e),
            }
            
            // Try to reset the terminal in other ways if available
            // termwiz doesn't have clear_screen() or show_cursor() methods directly
            // but the raw mode toggle above should help restore a usable state
        }
        
        // Ensure the running flag is set to false
        {
            let mut running = self.running.lock().unwrap();
            *running = false;
        }
        
        // Force terminal restoration using crossterm as a backup
        let _ = crossterm::terminal::disable_raw_mode();
        let mut stdout = std::io::stdout();
        let _ = crossterm::execute!(
            stdout,
            crossterm::terminal::LeaveAlternateScreen,
            crossterm::cursor::Show
        );
        
        // For display corruption issues, send reset command to terminal
        // This sends the ANSI reset sequence
        let _ = crossterm::execute!(
            stdout,
            crossterm::terminal::Clear(crossterm::terminal::ClearType::All),
            crossterm::style::ResetColor
        );
        
        // As a last resort, write the reset command directly
        let _ = stdout.write_all(b"\x1bc");  // Full terminal reset
        let _ = stdout.flush();
        
        Ok(())
    }
}

/// A panic hook to ensure the terminal is restored even after a panic
pub fn set_panic_hook() {
    let original_hook = std::panic::take_hook();
    
    std::panic::set_hook(Box::new(move |panic_info| {
        // For maximum safety, use direct ANSI escape sequences to reset the terminal
        // This bypasses crossterm and ensures we're sending raw escape sequences
        let reset_sequence = "\x1b[0m\x1b[?25h\x1b[2J\x1b[H\x1bc";
        
        // Try multiple output streams to maximize chance of successful reset
        // First try stderr
        let _ = std::io::stderr().write_all(reset_sequence.as_bytes());
        let _ = std::io::stderr().flush();
    }));
    info!("Panic hook set to restore terminal on panic");
    // Call the original panic hook to preserve its behavior
    std::panic::set_hook(original_hook);
}


// Move the Drop implementation outside the inherent impl block
impl Drop for Terminal {
    fn drop(&mut self) {
        if let Some(mut terminal) = self.terminal.take() {
            // Always try to disable raw mode in the destructor
            let _ = terminal.set_raw_mode();
            
            // Try to restore original size if we have it
            if let Some(original_state) = &self.original_state {
                if let Some((cols, rows)) = original_state.size {
                    let pty_size = PtySize {
                        cols,
                        rows,
                        pixel_width: cols * 8,
                        pixel_height: rows * 16,
                    };
                    let _ = terminal.set_screen_size(self.convert_pty_size(&pty_size));
                }
            }
            
            debug!("Terminal resources cleaned up in drop");
        }
    }

    }
