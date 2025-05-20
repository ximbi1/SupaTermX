use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use log::info;
use tui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame, Terminal as RatatuiTerminal,
};
use std::io::{self, Stdout};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{Receiver, Sender};

use crate::terminal::{Config as TerminalConfig, Terminal, TerminalOutput};

/// Mode of operation for the input handling
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    /// Normal terminal mode - input goes to the shell
    Normal,
    /// AI interaction mode - input goes to the AI assistant
    Ai,
    /// Scaffolding mode - input is used for project scaffolding
    Scaffold,
}

/// Draw the AI chat overlay
/// Dibuja el overlay de AI usando solo el estado clonado (ai_state), nunca &mut self
fn draw_ai_overlay(
    ai_state: &AiChatState,
    f: &mut Frame<CrosstermBackend<Stdout>>,
    size: Rect,
) {
    // Calcula área del overlay
    let overlay_area = Rect {
        x: size.width / 10,
        y: size.height / 10,
        width: size.width * 8 / 10,
        height: size.height * 8 / 10,
    };

    // Layout interno
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(3)])
        .split(overlay_area);

    // Fondo
    f.render_widget(Block::default().style(Style::default().bg(Color::Black)), overlay_area);

    // Mensajes
    let block = Block::default().borders(Borders::ALL).title("AI Chat");
    let lines: Vec<Line> = ai_state.messages.iter().map(|msg| {
        let prefix = match msg.sender {
            ChatSender::User => Span::styled("You: ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            ChatSender::Ai   => Span::styled("AI:  ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        };
        Line::from(vec![prefix, Span::raw(&msg.content)])
    }).collect();
    let widget = Paragraph::new(lines).block(block).wrap(Wrap{trim:false});
    f.render_widget(widget, chunks[0]);

    // Input de chat
    let input_block = Block::default().borders(Borders::ALL).title("Enter message");
    let input_widget = Paragraph::new(ai_state.input.as_str())
        .block(input_block)
        .style(Style::default().fg(Color::White));
    f.render_widget(input_widget, chunks[1]);

    // Cursor en el input
    f.set_cursor(chunks[1].x + 1 + ai_state.input.len() as u16,
                 chunks[1].y + 1);
}

/// State of the AI chat overlay
#[derive(Debug, Clone)]
pub struct AiChatState {
    /// Whether the chat overlay is visible
    pub visible: bool,
    /// Current messages in the chat
    pub messages: Vec<ChatMessage>,
    /// Current input buffer for AI interactions
    pub input: String,
}

impl Default for AiChatState {
    fn default() -> Self {
        Self {
            visible: false,
            messages: Vec::new(),
            input: String::new(),
        }
    }
}

/// Represents a message in the AI chat
#[derive(Debug, Clone)]
pub struct ChatMessage {
    /// Who sent this message (user or AI)
    pub sender: ChatSender,
    /// Content of the message
    pub content: String,
    /// When the message was sent
    pub timestamp: chrono::DateTime<chrono::Local>,
}

/// Sender of a chat message
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatSender {
    User,
    Ai,
}

/// Main application state
pub struct App {
    /// The terminal backend
    terminal: RatatuiTerminal<CrosstermBackend<Stdout>>,
    /// The terminal emulator
    terminal_emu: Terminal,
    /// Receiver for terminal output
    terminal_rx: Receiver<TerminalOutput>,
    /// Sender for terminal input
    terminal_tx: Sender<String>,
    /// Current buffer for terminal output
    terminal_buffer: Vec<String>,
    /// Current input buffer for shell commands
    input_buffer: String,
    /// Current input mode
    input_mode: InputMode,
    /// State of the AI chat
    ai_chat: AiChatState,
    /// Flag indicating if the application should exit
    should_quit: bool,
    /// Cursor position in the input field
    cursor_position: usize,
}

impl App {
    /// Create a new application instance
    pub fn new(terminal_config: TerminalConfig) -> Result<Self> {
        // Setup terminal
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = RatatuiTerminal::new(backend)?;

        // Create terminal emulator
        let (terminal_emu, terminal_rx, terminal_tx) = Terminal::new(terminal_config)?;

        Ok(Self {
            terminal,
            terminal_emu,
            terminal_rx,
            terminal_tx,
            terminal_buffer: Vec::new(),
            input_buffer: String::new(),
            input_mode: InputMode::Normal,
            ai_chat: AiChatState::default(),
            should_quit: false,
            cursor_position: 0,
        })
    }

    /// Run the application
    pub async fn run(&mut self) -> Result<()> {
        // Initialize the terminal emulator
        self.terminal_emu.init().await?;
        self.terminal_emu.run().await?;

        // Start the event loop
        let tick_rate = Duration::from_millis(100);
        let mut last_tick = Instant::now();

        while !self.should_quit {
            // Handle terminal output
            self.process_terminal_output().await;

            // Calculate timeout based on tick rate
            let timeout = tick_rate
                .checked_sub(last_tick.elapsed())
                .unwrap_or_else(|| Duration::from_secs(0));

            // Handle input events
            if crossterm::event::poll(timeout)? {
                self.handle_events().await?;
            }

            // Render UI
            self.draw()?;

            // Check if we should tick
            if last_tick.elapsed() >= tick_rate {
                self.tick().await?;
                last_tick = Instant::now();
            }
        }

        // Shut down properly
        self.shutdown().await?;

        Ok(())
    }

    /// Process terminal output from the PTY
    async fn process_terminal_output(&mut self) {
        // Drain the channel
        while let Ok(output) = self.terminal_rx.try_recv() {
            // In a real implementation, this would properly handle terminal escape sequences
            // For now, we'll just split on newlines and add to our buffer
            let lines = output.content.split('\n').map(|s| s.to_string());
            self.terminal_buffer.extend(lines);

            // Keep buffer at a reasonable size
            while self.terminal_buffer.len() > 1000 {
                self.terminal_buffer.remove(0);
            }
        }
    }

    /// Handle input events
    async fn handle_events(&mut self) -> Result<()> {
        if let Event::Key(key) = event::read()? {
            match self.input_mode {
                InputMode::Normal => self.handle_normal_mode_input(key).await?,
                InputMode::Ai => self.handle_ai_mode_input(key).await?,
                InputMode::Scaffold => self.handle_scaffold_mode_input(key).await?,
            }
        }
        Ok(())
    }

    /// Handle input in normal mode
    async fn handle_normal_mode_input(&mut self, key: KeyEvent) -> Result<()> {
        match key {
            // Quit
            KeyEvent {
                code: KeyCode::Char('c'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                // In a real implementation, this would be handled better
                // by differentiating between app-level and terminal-level Ctrl+C
                self.should_quit = true;
            }

            // Enter AI mode
            KeyEvent {
                code: KeyCode::Char('i'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                self.input_mode = InputMode::Ai;
                self.ai_chat.visible = true;
            }

            // Enter sends a command to the shell
            KeyEvent {
                code: KeyCode::Enter,
                ..
            } => {
                if !self.input_buffer.is_empty() {
                    // Add command to terminal buffer
                    self.terminal_buffer.push(format!("> {}", self.input_buffer));
                    
                    // Special command handling
                    if self.input_buffer.starts_with("@new") {
                        self.input_mode = InputMode::Scaffold;
                        self.ai_chat.visible = true;
                        self.ai_chat.input = self.input_buffer.clone();
                    } else {
                        // Send command to terminal
                        self.terminal_tx.send(self.input_buffer.clone()).await?;
                    }
                    
                    // Clear input buffer
                    self.input_buffer.clear();
                    self.cursor_position = 0;
                }
            }

            // Handle backspace
            KeyEvent {
                code: KeyCode::Backspace,
                ..
            } => {
                if self.cursor_position > 0 {
                    self.input_buffer.remove(self.cursor_position - 1);
                    self.cursor_position -= 1;
                }
            }

            // Handle delete
            KeyEvent {
                code: KeyCode::Delete,
                ..
            } => {
                if self.cursor_position < self.input_buffer.len() {
                    self.input_buffer.remove(self.cursor_position);
                }
            }

            // Handle left arrow
            KeyEvent {
                code: KeyCode::Left,
                ..
            } => {
                if self.cursor_position > 0 {
                    self.cursor_position -= 1;
                }
            }

            // Handle right arrow
            KeyEvent {
                code: KeyCode::Right,
                ..
            } => {
                if self.cursor_position < self.input_buffer.len() {
                    self.cursor_position += 1;
                }
            }

            // Handle other characters
            KeyEvent {
                code: KeyCode::Char(c),
                ..
            } => {
                self.input_buffer.insert(self.cursor_position, c);
                self.cursor_position += 1;
            }

            _ => {}
        }
        Ok(())
    }

    /// Handle input in AI mode
    async fn handle_ai_mode_input(&mut self, key: KeyEvent) -> Result<()> {
        match key {
            // Exit AI mode
            KeyEvent {
                code: KeyCode::Esc,
                ..
            } => {
                self.input_mode = InputMode::Normal;
                self.ai_chat.visible = false;
            }

            // Send message to AI
            KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                if !self.ai_chat.input.is_empty() {
                    // Add user message to chat
                    self.ai_chat.messages.push(ChatMessage {
                        sender: ChatSender::User,
                        content: self.ai_chat.input.clone(),
                        timestamp: chrono::Local::now(),
                    });

                    // In a real implementation, we would send the message to the AI
                    // and get a response. For now, we'll add a placeholder response.
                    self.ai_chat.messages.push(ChatMessage {
                        sender: ChatSender::Ai,
                        content: "This is a placeholder AI response.".to_string(),
                        timestamp: chrono::Local::now(),
                    });

                    // Clear input
                    self.ai_chat.input.clear();
                }
            }

            // Handle backspace
            KeyEvent {
                code: KeyCode::Backspace,
                ..
            } => {
                if !self.ai_chat.input.is_empty() {
                    self.ai_chat.input.pop();
                }
            }

            // Handle typing
            KeyEvent {
                code: KeyCode::Char(c),
                ..
            } => {
                self.ai_chat.input.push(c);
            }

            _ => {}
        }
        Ok(())
    }

    /// Handle input in scaffold mode
    async fn handle_scaffold_mode_input(&mut self, key: KeyEvent) -> Result<()> {
        // For now, scaffold mode input handling is similar to AI mode
        self.handle_ai_mode_input(key).await
    }




    /// Periodic tick for animations and updates
    async fn tick(&mut self) -> Result<()> {
        // This is where we would handle periodic updates
        // For now, we don't have any animations or updates that need
        // to happen on a timer, but we keep the method for future extensions
        Ok(())
    }


    /// Draw the user interface
    /// Draw the user interface
    fn draw(&mut self) -> Result<()> {
        // 1) Extract and clone everything we need before any borrows
        let terminal_output = self.render_terminal_output();
        let input_mode = self.input_mode;
        let cursor_pos = self.cursor_position;
        let input_buffer = self.input_buffer.clone();
        let ai_state = self.ai_chat.clone();

        // 2) Now do the mutable borrow for the terminal draw
        self.terminal.draw(|f| {
            let size = f.size();

            // Layout principal
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(1),
                    Constraint::Length(1),
                    Constraint::Length(3),
                ])
                .split(size);

            // 3) Use only the cloned data in the closure
            f.render_widget(terminal_output, chunks[0]);

            let status = match input_mode {
                InputMode::Normal => "NORMAL",
                InputMode::Ai => "AI",
                InputMode::Scaffold => "SCAFFOLD",
            };
            let status_line = Paragraph::new(format!("Mode: {}", status))
                .style(Style::default().fg(Color::White).bg(Color::Blue));
            f.render_widget(status_line, chunks[1]);

            let input_block = Block::default()
                .borders(Borders::ALL)
                .title(Span::styled("Input", Style::default().fg(Color::Yellow)));

            // Use the cloned input buffer
            let input_text = match input_mode {
                InputMode::Normal => &input_buffer,
                InputMode::Ai | InputMode::Scaffold => &ai_state.input,
            };
            let input_widget = Paragraph::new(input_text.as_str())
                .block(input_block)
                .style(Style::default().fg(Color::White));
            f.render_widget(input_widget, chunks[2]);

            // Use cursor_pos directly since it's copied
            let x0 = chunks[2].x + 1;
            let y0 = chunks[2].y + 1;
            if input_mode == InputMode::Normal {
                f.set_cursor(x0 + cursor_pos as u16, y0);
            } else {
                f.set_cursor(x0 + input_text.len() as u16, y0);
            }

            // Draw AI overlay if visible
            if ai_state.visible {
                draw_ai_overlay(&ai_state, f, size);
            }
        })?;

        Ok(())
    }
    

    /// Render the terminal output area
    fn render_terminal_output(&self) -> Paragraph {
        // Combine terminal buffer into text
        let text: Vec<Line> = self
            .terminal_buffer
            .iter()
            .map(|line| {
                Line::from(vec![Span::raw(line)])
            })
            .collect();

        Paragraph::new(text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(Span::styled(
                        "Terminal",
                        Style::default().fg(Color::Green),
                    )),
            )
            .wrap(Wrap { trim: false })
    }

    /// Shutdown the application and clean up resources
    async fn shutdown(&mut self) -> Result<()> {
        // Shutdown terminal emulator
        self.terminal_emu.shutdown().await?;

        // Restore terminal
        disable_raw_mode()?;
        execute!(
            self.terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        )?;
        self.terminal.show_cursor()?;

        info!("Application shutdown complete");
        Ok(())
    }

    /// Format and render a diff between two texts
    /// 
    /// Takes borrowed string references with explicit lifetime parameters to ensure
    /// proper borrowing relationships in the returned Vec<Line>
    fn render_diff<'a>(&self, original: &'a str, modified: &'a str) -> Vec<Line<'a>> {
        // This is a placeholder for actual diff rendering
        // In a real implementation, we would use the similar crate to generate
        // a proper colorized diff
        vec![
            Line::from(vec![Span::raw("--- Original")]),
            Line::from(vec![Span::raw("+++ Modified")]),
            Line::from(vec![Span::raw("")]),
            Line::from(vec![Span::raw(original)]),
            Line::from(vec![Span::raw("")]),
            Line::from(vec![Span::raw(modified)]),
        ]
    }

    /// Send a message to the AI and handle the response
    async fn send_ai_message(&mut self, message: String) -> Result<()> {
        // In a real implementation, this would send the message to the AI service
        // and handle the streaming response
        // For now, we'll just add a placeholder message

        // Add user message to the chat
        self.ai_chat.messages.push(ChatMessage {
            sender: ChatSender::User,
            content: message,
            timestamp: chrono::Local::now(),
        });

        // Simulate AI response (would be replaced with actual API call)
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Add AI response to the chat
        self.ai_chat.messages.push(ChatMessage {
            sender: ChatSender::Ai,
            content: "I'm a placeholder AI response. In the real implementation, this would be an actual response from the OpenAI API.".to_string(),
            timestamp: chrono::Local::now(),
        });

        Ok(())
    }

    /// Execute a command in the shell and capture the output
    async fn execute_shell_command(&mut self, command: String) -> Result<String> {
        // Send the command to the terminal
        self.terminal_tx.send(command.clone()).await?;

        // In a real implementation, we would capture and return the command output
        // For now, we'll return a placeholder
        Ok(format!("Executed command: {}", command))
    }
}
