pub mod term;

pub mod config;
pub mod error;
pub mod format;
pub mod fs;
pub mod test;

pub fn init_logging(level: Option<log::LevelFilter>) {
    let level = level.unwrap_or(log::LevelFilter::Info);
    env_logger::Builder::new()
        .filter(None, level)
        .init();
}
