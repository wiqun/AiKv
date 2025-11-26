use aikv::{Server, StorageEngine};
use serde::Deserialize;
use std::fs;
use tracing::{info, warn};
use tracing_subscriber::{self, filter::LevelFilter, EnvFilter};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const LOGO: &str = r#"
     _    _ _  __
    / \  (_) |/ /_   __
   / _ \ | | ' /\ \ / /
  / ___ \| | . \ \ V /
 /_/   \_\_|_|\_\ \_/
"#;

/// Server section of the configuration file
#[derive(Deserialize, Default)]
struct ServerConfig {
    #[serde(default = "default_host")]
    host: String,
    #[serde(default = "default_port")]
    port: u16,
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn default_port() -> u16 {
    6379
}

/// Storage section of the configuration file
#[derive(Deserialize, Default)]
struct StorageConfig {
    /// Storage engine type: "memory" or "aidb"
    #[serde(default = "default_engine")]
    engine: String,
    /// Data directory for AiDb storage
    #[serde(default = "default_data_dir")]
    data_dir: String,
    /// Number of databases (default: 16)
    #[serde(default = "default_databases")]
    databases: usize,
}

fn default_engine() -> String {
    "memory".to_string()
}

fn default_data_dir() -> String {
    "./data".to_string()
}

fn default_databases() -> usize {
    16
}

/// Logging section of the configuration file
#[derive(Deserialize, Default)]
struct LoggingConfig {
    /// Log level: trace, debug, info, warn, error
    #[serde(default = "default_log_level")]
    level: String,
}

fn default_log_level() -> String {
    "info".to_string()
}

/// Root configuration structure
#[derive(Deserialize, Default)]
struct Config {
    #[serde(default)]
    server: ServerConfig,
    #[serde(default)]
    storage: StorageConfig,
    #[serde(default)]
    logging: LoggingConfig,
}

/// Command line arguments structure
struct CliArgs {
    config_path: Option<String>,
    host: Option<String>,
    port: Option<u16>,
    show_help: bool,
    show_version: bool,
}

fn print_help() {
    println!("{}", LOGO);
    println!(
        "AiKv v{} - Redis protocol compatible key-value store",
        VERSION
    );
    println!();
    println!("USAGE:");
    println!("    aikv [OPTIONS]");
    println!("    aikv [ADDRESS]");
    println!();
    println!("OPTIONS:");
    println!("    -c, --config <FILE>    Path to configuration file (TOML format)");
    println!("    -H, --host <HOST>      Bind address (default: 127.0.0.1)");
    println!("    -p, --port <PORT>      Bind port (default: 6379)");
    println!("    -h, --help             Print help information");
    println!("    -v, --version          Print version information");
    println!();
    println!("EXAMPLES:");
    println!("    # Start with default settings (127.0.0.1:6379)");
    println!("    aikv");
    println!();
    println!("    # Start with configuration file");
    println!("    aikv --config config.toml");
    println!("    aikv -c config/aikv.toml");
    println!();
    println!("    # Start with custom host and port");
    println!("    aikv --host 0.0.0.0 --port 6380");
    println!("    aikv -H 0.0.0.0 -p 6380");
    println!();
    println!("    # Start with address directly (legacy mode)");
    println!("    aikv 127.0.0.1:6379");
    println!();
    println!("CONFIGURATION FILE:");
    println!("    See config/aikv.toml for a complete configuration template.");
    println!("    Implemented configuration options:");
    println!();
    println!("    [server]");
    println!("    host = \"127.0.0.1\"");
    println!("    port = 6379");
    println!();
    println!("    [storage]");
    println!("    engine = \"memory\"    # or \"aidb\"");
    println!("    data_dir = \"./data\"  # for aidb engine");
    println!("    databases = 16");
    println!();
    println!("    [logging]");
    println!("    level = \"info\"       # trace, debug, info, warn, error");
    println!();
    println!("For more information, visit: https://github.com/Genuineh/AiKv");
}

fn print_version() {
    println!("aikv {}", VERSION);
}

/// Parse command line arguments
fn parse_args() -> CliArgs {
    let args: Vec<String> = std::env::args().collect();
    let mut cli = CliArgs {
        config_path: None,
        host: None,
        port: None,
        show_help: false,
        show_version: false,
    };

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                cli.show_help = true;
                return cli;
            }
            "-v" | "--version" => {
                cli.show_version = true;
                return cli;
            }
            "-c" | "--config" => {
                if i + 1 < args.len() {
                    cli.config_path = Some(args[i + 1].clone());
                    i += 1;
                } else {
                    eprintln!("Error: {} requires a file path argument", args[i]);
                    std::process::exit(1);
                }
            }
            "-H" | "--host" => {
                if i + 1 < args.len() {
                    cli.host = Some(args[i + 1].clone());
                    i += 1;
                } else {
                    eprintln!("Error: {} requires a host argument", args[i]);
                    std::process::exit(1);
                }
            }
            "-p" | "--port" => {
                if i + 1 < args.len() {
                    match args[i + 1].parse::<u16>() {
                        Ok(port) => cli.port = Some(port),
                        Err(_) => {
                            eprintln!("Error: Invalid port number '{}'", args[i + 1]);
                            std::process::exit(1);
                        }
                    }
                    i += 1;
                } else {
                    eprintln!("Error: {} requires a port argument", args[i]);
                    std::process::exit(1);
                }
            }
            arg => {
                // Legacy mode: first argument is the address
                if cli.config_path.is_none() && cli.host.is_none() && cli.port.is_none() {
                    // Check if it looks like an address (contains ':')
                    if arg.contains(':') {
                        let parts: Vec<&str> = arg.split(':').collect();
                        if parts.len() == 2 {
                            cli.host = Some(parts[0].to_string());
                            if let Ok(port) = parts[1].parse::<u16>() {
                                cli.port = Some(port);
                            } else {
                                eprintln!(
                                    "Error: Invalid port in address '{}'. Expected HOST:PORT",
                                    arg
                                );
                                std::process::exit(1);
                            }
                        } else {
                            eprintln!(
                                "Error: Invalid address format '{}'. Expected HOST:PORT",
                                arg
                            );
                            std::process::exit(1);
                        }
                    } else {
                        eprintln!("Error: Unknown option '{}'. Use --help for usage.", arg);
                        std::process::exit(1);
                    }
                } else {
                    eprintln!(
                        "Error: Unexpected argument '{}'. Use --help for usage.",
                        arg
                    );
                    std::process::exit(1);
                }
            }
        }
        i += 1;
    }

    cli
}

/// Load configuration from file and merge with CLI arguments
fn load_config(cli: &CliArgs) -> (String, u16, StorageConfig, LoggingConfig) {
    let mut config = Config::default();

    // Load from config file if specified
    if let Some(ref path) = cli.config_path {
        match fs::read_to_string(path) {
            Ok(content) => match toml::from_str::<Config>(&content) {
                Ok(cfg) => config = cfg,
                Err(e) => {
                    eprintln!("Failed to parse config file '{}': {}", path, e);
                    std::process::exit(1);
                }
            },
            Err(e) => {
                eprintln!("Failed to read config file '{}': {}", path, e);
                std::process::exit(1);
            }
        }
    }

    // CLI arguments override config file
    let host = cli.host.clone().unwrap_or(config.server.host);
    let port = cli.port.unwrap_or(config.server.port);

    (host, port, config.storage, config.logging)
}

/// Create storage engine based on configuration
fn create_storage_engine(storage_config: &StorageConfig) -> StorageEngine {
    match storage_config.engine.to_lowercase().as_str() {
        "aidb" => {
            info!(
                "Using AiDb storage engine with data directory: {}",
                storage_config.data_dir
            );
            match StorageEngine::new_aidb(&storage_config.data_dir, storage_config.databases) {
                Ok(engine) => engine,
                Err(e) => {
                    eprintln!(
                        "Failed to initialize AiDb storage at '{}': {}",
                        storage_config.data_dir, e
                    );
                    std::process::exit(1);
                }
            }
        }
        "memory" => {
            info!("Using in-memory storage engine");
            StorageEngine::new_memory(storage_config.databases)
        }
        other => {
            warn!("Unknown storage engine '{}', falling back to memory", other);
            StorageEngine::new_memory(storage_config.databases)
        }
    }
}

#[tokio::main]
async fn main() {
    // Parse command line arguments
    let cli = parse_args();

    // Handle help and version
    if cli.show_help {
        print_help();
        return;
    }
    if cli.show_version {
        print_version();
        return;
    }

    // Load configuration
    let (host, port, storage_config, logging_config) = load_config(&cli);

    // Initialize logging with configured level
    let log_level = logging_config.level.to_lowercase();
    let level_filter = log_level.parse::<LevelFilter>().unwrap_or_else(|_| {
        eprintln!(
            "Warning: Invalid log level '{}', using 'info'",
            logging_config.level
        );
        LevelFilter::INFO
    });
    let filter = EnvFilter::builder()
        .with_default_directive(level_filter.into())
        .from_env_lossy();
    tracing_subscriber::fmt()
        .with_target(false)
        .with_level(true)
        .with_env_filter(filter)
        .init();

    let addr = format!("{}:{}", host, port);

    // Print startup banner
    println!("{}", LOGO);
    println!(
        "AiKv v{} - Redis protocol compatible key-value store",
        VERSION
    );
    println!();

    // Create storage engine based on configuration
    let storage = create_storage_engine(&storage_config);

    // Create and run server
    let server = Server::new(addr, storage);

    if let Err(e) = server.run().await {
        eprintln!("Server error: {}", e);
        std::process::exit(1);
    }
}
