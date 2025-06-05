use crossterm::style::Color;

/// Text style for TUI rendering
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    fg: Option<Color>,
    bg: Option<Color>,
    bold: bool,
    underline: bool,
}

impl Style {
    pub fn default() -> Self {
        Self {
            fg: None,
            bg: None,
            bold: false,
            underline: false,
        }
    }
    
    pub fn fg(mut self, color: Color) -> Self {
        self.fg = Some(color);
        self
    }
}

/// Text formatting style
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatStyle {
    /// Regular text
    Regular,
    /// Heading text
    Heading,
    /// Error text
    Error,
    /// Success text
    Success,
    /// Warning text
    Warning,
    /// Info text
    Info,
    /// Command text
    Command,
    /// Output text
    Output,
}

impl FormatStyle {
    /// Get the appropriate style for TUI rendering
    pub fn to_style(self) -> Style {
        match self {
            FormatStyle::Regular => Style::default(),
            FormatStyle::Heading => Style::default().fg(Color::Yellow),
            FormatStyle::Error => Style::default().fg(Color::Red),
            FormatStyle::Success => Style::default().fg(Color::Green),
            FormatStyle::Warning => Style::default().fg(Color::Yellow),
            FormatStyle::Info => Style::default().fg(Color::Blue),
            FormatStyle::Command => Style::default().fg(Color::Cyan),
            FormatStyle::Output => Style::default().fg(Color::White),
        }
    }
}

/// Format markdown-style text
pub fn format_markdown(text: &str) -> String {
    // A very simple markdown formatter for demonstration
    let mut result = String::new();
    
    for line in text.lines() {
        if line.starts_with("# ") {
            result.push_str(&format!("\n{}\n", &line[2..]));
        } else if line.starts_with("## ") {
            result.push_str(&format!("\n{}\n", &line[3..]));
        } else if line.starts_with("- ") {
            result.push_str(&format!("• {}\n", &line[2..]));
        } else if line.starts_with("```") {
            result.push_str("\n");
        } else {
            result.push_str(&format!("{}\n", line));
        }
    }
    
    result
}
