/// Devuelve true si el shell existe en $PATH
pub fn is_shell_available(shell: &str) -> bool {
    which::which(shell).is_ok()
}

/// Devuelve la ruta al shell por defecto (o "bash")
pub fn get_default_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into())
}

/// Comprueba si la terminal actual soporta ANSI (colores)
pub fn supports_ansi() -> bool {
    atty::is(atty::Stream::Stdout) && std::env::var("TERM").map(|t| t != "dumb").unwrap_or(false)
}

