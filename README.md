# SupaTerm

**SupaTerm** es un terminal ligero e inteligente para Linux, que combina la potencia de la línea de comandos con asistentes de inteligencia artificial para mejorar la productividad.

---

## Características principales

* **Autocompletado inteligente**: Sugiere y completa comandos basados en tu historial y contexto actual.
* **Análisis de errores de shell**: Interpreta mensajes de error y ofrece explicaciones y soluciones.
* **Generador de scaffolds**: Crea estructuras de proyecto completas a partir de una descripción.
* **Sugerencias de comandos**: Propone comandos alternativos y optimizados.
* **Generación de diffs de código**: Produce diffs aplicables para modificar fragmentos de código.
* **Interfaz TUI**: Experiencia de usuario enriquecida con una terminal basada en text user interface.

---

## Requisitos

* **Rust** ≥ 1.60
* **Cargo**
* **OpenAI API Key** (para funciones AI)
* Terminal compatible con ANSI

---

## Instalación

1. Clona el repositorio:

   ```bash
   git clone https://github.com/tu-usuario/supaterm.git
   cd supaterm
   ```

2. Compila con Cargo:

   ```bash
   cargo build --release
   ```

3. (Opcional) Instálalo globalmente:

   ```bash
   cargo install --path .
   ```

---

## Uso

### Variables de entorno

* `OPENAI_API_KEY`: Clave de API de OpenAI para habilitar las funciones de IA.
* `OPENAI_ORG_ID`: (Opcional) ID de organización de OpenAI.

### Comando principal

```bash
supaterm [options]
```

**Opciones**:

* `-s, --shell <shell>`: Shell a utilizar (por defecto: detectado del entorno).
* `--api_key <key>`: Proporciona la clave de OpenAI directamente.
* `--config <path>`: Ruta a un archivo de configuración personalizado.
* `-w, --workdir <path>`: Directorio de trabajo inicial.
* `--log_level <level>`: Nivel de log (`trace`, `debug`, `info`, `warn`, `error`).

### Subcomandos

* `new <name> [--description <desc>]`: Genera un scaffold de proyecto.

---

## Configuración

El archivo de configuración (TOML) permite ajustar:

```toml
[terminal]
shell = "/bin/bash"

[ai]
api_key = ""
model = "gpt-4"
org_id = ""
max_tokens = 2048
temperature = 0.7
```

---

## Desarrollo

1. Ejecuta los tests:

   ```bash
   cargo test
   ```

2. Análisis de código y formateo:

   ```bash
   cargo fmt
   cargo clippy
   ```

3. Contribuye con PRs al repositorio.

---

## Contribuir

¡Las contribuciones son bienvenidas! Por favor:

1. Haz un *fork* del repositorio.
2. Crea una *branch* para tu feature o bugfix.
3. Envía un *pull request* describiendo tus cambios.

---

## Licencia

Este proyecto está licenciado bajo **MIT License**. Consulta el archivo [LICENSE](LICENSE) para más detalles.

