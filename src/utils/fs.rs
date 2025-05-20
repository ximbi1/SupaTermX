use std::fs::{self, create_dir_all, copy, metadata, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use anyhow::{Context, Result};

/// Create a file with all parent directories
pub fn create_file_with_parents(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            create_dir_all(parent)
                .context("Failed to create parent directories")?;
        }
    }
    
    let mut file = File::create(path)
        .context("Failed to create file")?;
    
    file.write_all(content.as_bytes())
        .context("Failed to write file content")?;
    
    Ok(())
}

/// Read a file to string
pub fn read_file_to_string(path: &Path) -> Result<String> {
    let mut content = String::new();
    let mut file = File::open(path)
        .context("Failed to open file")?;
    
    file.read_to_string(&mut content)
        .context("Failed to read file")?;
    
    Ok(content)
}

/// Check if a path is a child of another path
pub fn is_child_path(parent: &Path, child: &Path) -> bool {
    match (parent.canonicalize(), child.canonicalize()) {
        (Ok(parent), Ok(child)) => child.starts_with(parent),
        _ => false,
    }
}

/// Find files matching a pattern
pub fn find_files(dir: &Path, pattern: &str) -> Result<Vec<PathBuf>> {
    use glob::glob;
    
    let pattern = format!("{}/{}", dir.display(), pattern);
    let mut files = Vec::new();
    
    for entry in glob(&pattern)
        .context("Failed to parse glob pattern")?
    {
        match entry {
            Ok(path) => files.push(path),
            Err(e) => log::warn!("Glob error: {}", e),
        }
    }
    
    Ok(files)
}

/// Find directories matching a pattern
pub fn find_directories(dir: &Path, pattern: &str) -> Result<Vec<PathBuf>> {
    let files = find_files(dir, pattern)?;
    Ok(files.into_iter().filter(|p| p.is_dir()).collect())
}

/// Get file extension as a string
pub fn get_file_extension(path: &Path) -> Option<String> {
    path.extension().map(|ext| ext.to_string_lossy().to_string())
}

/// Check if a file is older than a given duration
pub fn is_file_older_than(path: &Path, duration: std::time::Duration) -> Result<bool> {
    let metadata = metadata(path)
        .context("Failed to get file metadata")?;
    
    let modified = metadata.modified()
        .context("Failed to get modification time")?;
    
    let now = std::time::SystemTime::now();
    let age = now.duration_since(modified)
        .context("Failed to calculate file age")?;
    
    Ok(age > duration)
}

/// Create a temporary file with content
pub fn create_temp_file(content: &str) -> Result<(PathBuf, tempfile::NamedTempFile)> {
    let file = tempfile::NamedTempFile::new()
        .context("Failed to create temporary file")?;
    
    let path = file.path().to_path_buf();
    
    file.as_file().write_all(content.as_bytes())
        .context("Failed to write to temporary file")?;
    
    Ok((path, file))
}

/// Create a backup of a file
pub fn backup_file(path: &Path) -> Result<PathBuf> {
    if !path.exists() {
        return Err(anyhow::anyhow!("File does not exist: {}", path.display()));
    }
    
    let timestamp = chrono::Local::now().format("%Y%m%d%H%M%S");
    let file_name = path.file_name().unwrap_or_default().to_string_lossy();
    let backup_name = format!("{}.{}.bak", file_name, timestamp);
    
    let backup_path = if let Some(parent) = path.parent() {
        parent.join(backup_name)
    } else {
        PathBuf::from(backup_name)
    };
    
    copy(path, &backup_path)
        .context(format!("Failed to create backup of: {}", path.display()))?;
    
    Ok(backup_path)
}

