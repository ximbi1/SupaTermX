use anyhow::Result;
use std::path::{Path, PathBuf};
use std::fs::create_dir_all;
use std::process::{Output, ExitStatus};
use std::os::unix::process::ExitStatusExt;

use crate::utils::fs;

/// Create a test file with content
pub fn create_test_file(dir: &Path, name: &str, content: &str) -> Result<PathBuf> {
    let path = dir.join(name);
    fs::create_file_with_parents(&path, content)?;
    Ok(path)
}

/// Create a test project structure
pub fn create_test_project(dir: &Path) -> Result<()> {
    // Create a simple Rust project structure
    create_dir_all(dir.join("src"))?;
    create_dir_all(dir.join("tests"))?;
    
    // Create basic files
    create_test_file(
        dir,
        "Cargo.toml",
        r#"[package]
name = "test_project"
version = "0.1.0"
edition = "2021"

[dependencies]
"#,
    )?;
    
    create_test_file(
        dir,
        "src/main.rs",
        r#"fn main() {
    println!("Hello, world!");
}
"#,
    )?;
    
    create_test_file(
        dir,
        "src/lib.rs",
        r#"pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_add() {
        assert_eq!(add(2, 2), 4);
    }
}
"#,
    )?;
    
    create_test_file(
        dir,
        "tests/integration_test.rs",
        r#"#[test]
fn test_integration() {
    assert_eq!(2 + 2, 4);
}
"#,
    )?;
    
    Ok(())
}

/// Mock command execution for testing
pub struct MockCommand {
    /// The command being mocked
    pub command: String,
    /// The arguments passed to the command
    pub args: Vec<String>,
    /// The stdout output to return
    pub stdout: String,
    /// The stderr output to return
    pub stderr: String,
    /// The exit code to return
    pub exit_code: i32,
}

impl Default for MockCommand {
    fn default() -> Self {
        Self {
            command: String::new(),
            args: Vec::new(),
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 0,
        }
    }
}

impl MockCommand {
    /// Create a new MockCommand
    pub fn new(command: &str) -> Self {
        Self {
            command: command.to_string(),
            ..Default::default()
        }
    }
    
    /// Set the arguments for the command
    pub fn with_args(mut self, args: &[&str]) -> Self {
        self.args = args.iter().map(|s| s.to_string()).collect();
        self
    }
    
    /// Set the stdout output
    pub fn with_stdout(mut self, stdout: &str) -> Self {
        self.stdout = stdout.to_string();
        self
    }
    
    /// Set the stderr output
    pub fn with_stderr(mut self, stderr: &str) -> Self {
        self.stderr = stderr.to_string();
        self
    }
    
    /// Set the exit code
    pub fn with_exit_code(mut self, code: i32) -> Self {
        self.exit_code = code;
        self
    }
    
    /// Execute the mock command and return the output
    pub fn execute(&self) -> Result<Output> {
        Ok(Output {
            status: ExitStatus::from_raw(self.exit_code),
            stdout: self.stdout.as_bytes().to_vec(),
            stderr: self.stderr.as_bytes().to_vec(),
        })
    }
}

/// Mock file for testing
pub struct MockFile {
    /// Path to the mock file
    pub path: PathBuf,
    /// Content of the mock file
    pub content: String,
    /// Whether the file exists
    pub exists: bool,
}

impl MockFile {
    /// Create a new MockFile
    pub fn new(path: &Path, content: &str, exists: bool) -> Self {
        Self {
            path: path.to_path_buf(),
            content: content.to_string(),
            exists,
        }
    }
    
    /// Read the content of the mock file
    pub fn read(&self) -> Result<String> {
        if !self.exists {
            return Err(anyhow::anyhow!("File does not exist: {}", self.path.display()));
        }
        
        Ok(self.content.clone())
    }
    
    /// Write content to the mock file
    pub fn write(&mut self, content: &str) -> Result<()> {
        self.content = content.to_string();
        self.exists = true;
        
        Ok(())
    }
}

