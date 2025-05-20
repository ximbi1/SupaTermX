use anyhow::{Context, Result};
use log::{debug, error, info, warn};
use portable_pty::{
    native_pty_system, CommandBuilder, PtyPair, PtySize,
    Child as PtyChild,
};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use termwiz::caps::Capabilities;
use termwiz::terminal::{new_terminal, Terminal as TermwizTerminal, ScreenSize};
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio::task::JoinHandle;
use vte::{Parser, Perform, Params};
use std::path::PathBuf;
use nix::sys::signal;

/// Configuration for the terminal
#[derive(Debug, Clone)]
pub struct Config {
    /// The shell to spawn (e.g., "fish", "bash", "zsh")
    pub shell: String,
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
}

/// Terminal manages the PTY and provides methods to interact with it
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
}

impl VteHandler {
    fn new(output_tx: Sender<TerminalOutput>, terminal_size: Arc<Mutex<PtySize>>) -> Self {
        Self {
            output_tx,
            terminal_size,
            current_line: String::new(),
        }
    }

    /// Flush the current line buffer
    async fn flush_line(&mut self) {
        if !self.current_line.is_empty() {
            let content = std::mem::take(&mut self.current_line);
            let _ = self.output_tx.send(TerminalOutput {
                content,
                is_control: false,
            }).await;
        }
    }
}

impl Perform for VteHandler {
    fn print(&mut self, c: char) {
        self.current_line.push(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\r' => {
                // Carriage return - move to beginning of line
                // In a full implementation, we'd handle this properly with cursor positioning
                // For now, we'll just append to the current line
                self.current_line.push('\r');
            },
            b'\n' => {
                // Line feed - move to next line
                self.current_line.push('\n');
                
                // In a more complete implementation, we'd handle this with proper line buffering
                // and terminal state tracking. For simplicity, we'll flush on newlines.
                let content = std::mem::take(&mut self.current_line);
                let _ = self.output_tx.try_send(TerminalOutput {
                    content,
                    is_control: false,
                });
            },
            _ => {
                // Handle other control characters
                let ctrl_char = format!("^{}", (byte + 64) as char);
                self.current_line.push_str(&ctrl_char);
            }
        }
    }

    fn hook(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        // The start of a control sequence - we send this as a control sequence
        // Convert from &[&[u16]] parameter format to Vec<i64>
        let params_vec: Vec<i64> = params.iter()
            .filter_map(|param| param.first().map(|&p| p as i64))
            .collect();
        let _ = self.output_tx.try_send(TerminalOutput {
            content: format!("CSI: params={:?}, intermediates={:?}, action={}", params_vec, intermediates, action),
            is_control: true,
        });
    }

    fn put(&mut self, byte: u8) {
        // Put a byte into the current state of the terminal
        self.current_line.push(byte as char);
    }

    fn unhook(&mut self) {
        // End of a control sequence
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        // Handle Operating System Command
        let _ = self.output_tx.try_send(TerminalOutput {
            content: format!("OSC: params={:?}, bell_terminated={}", params, bell_terminated),
            is_control: true,
        });
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        // Handle CSI sequences - in a full implementation we would track cursor position,
        // attributes, etc. For now, we'll just log the sequence type.
        // Convert from &[&[u16]] parameter format to Vec<i64>
        let params_vec: Vec<i64> = params.iter()
            .filter_map(|param| param.first().map(|&p| p as i64))
            .collect();
        match action {
            'm' => {
                // SGR (Select Graphic Rendition) - handle text formatting
                let _ = self.output_tx.try_send(TerminalOutput {
                    content: format!("SGR: {:?}", params_vec),
                    is_control: true,
                });
            },
            'H' | 'f' => {
                // Cursor Position
                let _ = self.output_tx.try_send(TerminalOutput {
                    content: format!("CursorPos: {:?}", params_vec),
                    is_control: true,
                });
            },
            'J' => {
                // Erase in Display
                let _ = self.output_tx.try_send(TerminalOutput {
                    content: format!("EraseInDisplay: {:?}", params_vec),
                    is_control: true,
                });
            },
            'K' => {
                // Erase in Line
                let _ = self.output_tx.try_send(TerminalOutput {
                    content: format!("EraseInLine: {:?}", params_vec),
                    is_control: true,
                });
            },
            'A' | 'B' | 'C' | 'D' => {
                // Cursor movement
                let direction = match action {
                    'A' => "Up",
                    'B' => "Down",
                    'C' => "Forward",
                    'D' => "Backward",
                    _ => unreachable!(),
                };
                let _ = self.output_tx.try_send(TerminalOutput {
                    content: format!("CursorMove{}: {:?}", direction, params_vec),
                    is_control: true,
                });
            },
            _ => {
                // Other CSI sequences
                let _ = self.output_tx.try_send(TerminalOutput {
                    content: format!("CSI: params={:?}, intermediates={:?}, action={}", params_vec, intermediates, action),
                    is_control: true,
                });
            }
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        // Handle escape sequences
        let _ = self.output_tx.try_send(TerminalOutput {
            content: format!("ESC: intermediates={:?}, byte={}", intermediates, byte as char),
            is_control: true,
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
        let screen_size = terminal.get_screen_size()?;
        let cols = screen_size.cols;
        let rows = screen_size.rows;
        let original_raw_mode = false; // termwiz is always in raw mode when created

        self.original_state = Some(OriginalState {
            raw_mode: original_raw_mode, 
            size: Some((cols as u16, rows as u16)),
        });

        // Set up raw mode - termwiz's set_raw_mode() is a toggle
        if !original_raw_mode {
            terminal.set_raw_mode()?;
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
    pub async fn run(&mut self) -> Result<()> {
        if self.terminal.is_none() || self.pty_pair.is_none() || self.pty_writer.is_none() {
            return Err(anyhow::anyhow!("Terminal not fully initialized"));
        }
        
        let running = self.running.clone();
        
        // Get PTY reader from the master
        if let Some(pty_pair) = self.pty_pair.as_mut() {
            let mut reader = pty_pair.master.try_clone_reader()
                .context("Failed to clone PTY master reader")?;
            
            // Clone channels for the spawn tasks
            let output_tx = self.output_tx.clone();
            let terminal_size = self.terminal_size.clone();
            let running_clone = running.clone();
            
            // Take the PTY writer - we'll move it into the task
            let mut pty_writer = self.pty_writer.take().unwrap();
            
            // Create a new channel for forwarding input
            let (split_tx, mut split_rx) = mpsc::channel::<String>(100);
            
            // We can't clone input_rx directly since Receiver doesn't implement Clone
            // Take input_rx - we'll move it into the task
            let mut input_rx = match self.input_rx.take() {
                Some(rx) => rx,
                None => return Ok(()),
            };
            
            // Create a clone of running for the input task
            let running_clone = running.clone();
            
            // Forward input from input_rx to split_tx
            tokio::spawn(async move {
                while *running_clone.lock().unwrap() {
                    while let Some(msg) = input_rx.recv().await {
                        if split_tx.send(msg).await.is_err() {
                            break;
                        }
                    }
                }
            });
            
            // Setup output handling task
            let output_handle = tokio::spawn(async move {
                debug!("Starting terminal output handler");
                let mut buffer = [0u8; 4096];
                // Create a VTE parser for processing terminal output
                let mut parser = Parser::new();
                // Create a handler for the VTE parser
                let mut vte_handler = VteHandler::new(output_tx.clone(), terminal_size);
                
                loop {
                    // Check if we should keep running
                    if !*running.lock().unwrap() {
                        break;
                    }
                    
                    // Read from the PTY
                    match reader.read(&mut buffer) {
                        Ok(0) => {
                            // End of file, shell has exited
                            debug!("PTY EOF reached, shell has likely exited");
                            break;
                        },
                        Ok(n) => {
                            // Process the read bytes
                            for i in 0..n {
                                // Process each byte through the VTE parser
                                parser.advance(&mut vte_handler, buffer[i]);
                            }
                            
                            // Send raw output for debugging if needed
                            if let Ok(s) = std::str::from_utf8(&buffer[..n]) {
                                if s.trim().len() > 0 {
                                    debug!("Raw PTY output: {:?}", s);
                                }
                            }
                            
                            // Flush the current line if it has content
                            vte_handler.flush_line().await;
                        },
                        Err(e) => {
                            if e.kind() == std::io::ErrorKind::WouldBlock {
                                // No data available, wait a bit
                                tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                                continue;
                            } else {
                                // Real error
                                error!("Error reading from PTY: {}", e);
                                break;
                            }
                        }
                    }
                }
                
                debug!("Terminal output handler stopped");
            });
            
            // Setup input handling task
            let input_handle = tokio::spawn(async move {
                debug!("Starting terminal input handler");
                
                while let Some(input) = split_rx.recv().await {
                    debug!("Received terminal input: {}", input);
                    
                    // Add newline to the input if not present
                    let input = if input.ends_with('\n') {
                        input
                    } else {
                        format!("{}\n", input)
                    };
                    
                    // Write the input to the PTY
                    match pty_writer.write_all(input.as_bytes()) {
                        Ok(_) => {
                            debug!("Successfully wrote input to PTY");
                            // Ensure the write is flushed
                            if let Err(e) = pty_writer.flush() {
                                error!("Failed to flush PTY writer: {}", e);
                            }
                        },
                        Err(e) => {
                            error!("Failed to write input to PTY: {}", e);
                            // If we can't write to the PTY, the shell might have exited
                            break;
                        }
                    }
                }
                
                debug!("Terminal input handler stopped");
            });
            
            // Add task handles for cleanup
            self.task_handles.push(output_handle);
            self.task_handles.push(input_handle);
            
            info!("Terminal is now running");
            Ok(())
        } else {
            Err(anyhow::anyhow!("PTY pair not initialized"))
        }
    }

    /// Send input to the terminal
    pub async fn send_input(&self, input: String) -> Result<()> {
        // Forward the input to the input channel
        // The actual writing to PTY happens in the run() task
        self.output_tx.send(TerminalOutput {
            content: format!("Input: {}", input),
            is_control: false,
        }).await.context("Failed to send terminal output")?;
        
        Ok(())
    }

    /// Resize the terminal
    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        if let Some(pty_pair) = &self.pty_pair {
            debug!("Resizing terminal to {}x{}", cols, rows);
            
            let pty_size = PtySize {
                rows,
                cols,
                pixel_width: cols * 8,  // assuming 8 pixels per character
                pixel_height: rows * 16, // assuming 16 pixels per character
            };
            
            // Convert size before borrowing terminal
            let screen_size = self.convert_pty_size(&pty_size);
            
            // Apply the resize to the PTY
            pty_pair.master.resize(pty_size.clone())
                .context("Failed to resize PTY")?;
            
            // Update our size state
            {
                let mut size = self.terminal_size.lock().unwrap();
                *size = pty_size.clone();
            }
            
            // Also resize the termwiz terminal if we have one
            if let Some(term) = &mut self.terminal {
                term.set_screen_size(screen_size)?;
            }
            
            Ok(())
        } else {
            Err(anyhow::anyhow!("PTY not initialized"))
        }
    }

    /// Gracefully shut down the terminal
    pub async fn shutdown(&mut self) -> Result<()> {
        debug!("Terminal shutdown initiated");
        
        // Set running flag to false to signal tasks to stop
        {
            let mut running = self.running.lock().unwrap();
            *running = false;
        }
        
        // Wait for all task handles to complete
        for handle in self.task_handles.drain(..) {
            match handle.await {
                Ok(_) => debug!("Task completed successfully"),
                Err(e) => warn!("Task did not complete cleanly: {}", e),
            }
        }
        
        // Restore terminal state
        if let Some(terminal) = &mut self.terminal {
            // Restore original raw mode state if we have it
            if let Some(original_state) = &self.original_state {
                // Restore raw mode to original state if needed
                let current_raw_mode = true; // termwiz is always in raw mode initially
                if original_state.raw_mode != current_raw_mode {
                    terminal.set_raw_mode()?; // Toggle raw mode
                }
            } else {
                // No state saved, just toggle raw mode to restore
                terminal.set_raw_mode()?;
            }
        }
        
        // Clean up child process if it's still running
        if let Some(child) = &mut self.child {
            // Try to terminate gracefully first
            if let Err(e) = child.kill() {
                warn!("Failed to kill child process: {}", e);
            }
            
            // Wait for the process to exit
            match child.wait() {
                Ok(status) => debug!("Child process exited with status: {:?}", status),
                Err(e) => warn!("Failed to wait for child process: {}", e),
            }
        }
        
        // Clean up PTY resources
        self.pty_writer = None;
        
        info!("Terminal shutdown complete");
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
        
        Ok(())
    }
}

/// Global terminal cleanup function (called on application exit or panic)
pub fn cleanup() -> Result<()> {
    // This function is called from main when the application exits abnormally
    // It attempts to restore the terminal to a usable state
    
    // Use crossterm as a backup to ensure terminal is restored
    crossterm::terminal::disable_raw_mode()
        .context("Failed to disable raw mode with crossterm")?;
    
    // Execute terminal restoration commands
    let mut stdout = std::io::stdout();
    crossterm::execute!(
        stdout,
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture,
        crossterm::cursor::Show
    ).context("Failed to restore terminal state with crossterm")?;
    
    Ok(())
}

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

/// A panic hook to ensure the terminal is restored even after a panic
pub fn set_panic_hook() {
    let original_hook = std::panic::take_hook();
    
    std::panic::set_hook(Box::new(move |panic_info| {
        // Try to restore the terminal
        let _ = cleanup();
        
        // Print panic info to stderr
        eprintln!("\n\n--- PANIC ---");
        eprintln!("SupaTerm has encountered a fatal error and must exit.");
        eprintln!("Panic details: {}", panic_info);
        eprintln!("The terminal has been restored to a usable state.");
        
        // Call the original panic hook
        original_hook(panic_info);
    }));
}

