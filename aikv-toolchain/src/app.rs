//! Application state management for the TUI

use std::path::PathBuf;

/// Main menu items
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuItem {
    Build,
    Docker,
    Deploy,
    Config,
    Benchmark,
    Optimize,
    Docs,
    Status,
    Quit,
}

impl MenuItem {
    pub fn all() -> Vec<MenuItem> {
        vec![
            MenuItem::Build,
            MenuItem::Docker,
            MenuItem::Deploy,
            MenuItem::Config,
            MenuItem::Benchmark,
            MenuItem::Optimize,
            MenuItem::Docs,
            MenuItem::Status,
            MenuItem::Quit,
        ]
    }

    pub fn title(&self) -> &str {
        match self {
            MenuItem::Build => "🔨 Build AiKv",
            MenuItem::Docker => "🐳 Build Docker Image",
            MenuItem::Deploy => "📦 Generate Deployment",
            MenuItem::Config => "⚙️  Configuration Docs",
            MenuItem::Benchmark => "📊 Run Benchmarks",
            MenuItem::Optimize => "🚀 Optimization Tips",
            MenuItem::Docs => "📖 Documentation",
            MenuItem::Status => "ℹ️  Project Status",
            MenuItem::Quit => "🚪 Quit",
        }
    }

    pub fn description(&self) -> &str {
        match self {
            MenuItem::Build => "Build AiKv binary (single-node or cluster mode)",
            MenuItem::Docker => "Build Docker images for deployment",
            MenuItem::Deploy => "Generate deployment files (docker-compose, configs)",
            MenuItem::Config => "View configuration documentation and options",
            MenuItem::Benchmark => "Run performance benchmarks",
            MenuItem::Optimize => "View optimization suggestions",
            MenuItem::Docs => "Browse project documentation",
            MenuItem::Status => "Check project status and system info",
            MenuItem::Quit => "Exit the toolchain",
        }
    }
}

/// Deployment configuration
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct DeployConfig {
    pub deploy_type: DeployType,
    pub output_dir: PathBuf,
    pub node_count: u8,
    pub replicas_per_master: u8,
    pub base_port: u16,
    pub host: String,
    pub storage_engine: String,
    pub data_dir: PathBuf,
}

impl Default for DeployConfig {
    fn default() -> Self {
        Self {
            deploy_type: DeployType::Single,
            output_dir: PathBuf::from("./deploy"),
            node_count: 3,
            replicas_per_master: 1,
            base_port: 6379,
            host: "127.0.0.1".to_string(),
            storage_engine: "memory".to_string(),
            data_dir: PathBuf::from("./data"),
        }
    }
}

/// Deployment type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeployType {
    Single,
    Cluster,
}

impl DeployType {
    pub fn as_str(&self) -> &str {
        match self {
            DeployType::Single => "single",
            DeployType::Cluster => "cluster",
        }
    }
}

/// Build configuration
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct BuildConfig {
    pub release: bool,
    pub cluster: bool,
    pub target: Option<String>,
}

impl Default for BuildConfig {
    fn default() -> Self {
        Self {
            release: true,
            cluster: false,
            target: None,
        }
    }
}

/// Application view state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum View {
    MainMenu,
    BuildOptions,
    DockerOptions,
    DeployOptions,
    Config,
    Benchmark,
    Optimize,
    Docs,
    Status,
    Log,
}

/// Application state
#[allow(dead_code)]
pub struct App {
    pub project_dir: PathBuf,
    pub current_view: View,
    pub selected_menu_item: usize,
    pub build_config: BuildConfig,
    pub deploy_config: DeployConfig,
    pub logs: Vec<String>,
    pub status_message: Option<String>,
    pub is_running: bool,
    pub scroll_offset: usize,
    pub doc_topic: Option<String>,
    pub config_cluster_mode: bool,
}

impl App {
    pub fn new(project_dir: PathBuf) -> Self {
        Self {
            project_dir,
            current_view: View::MainMenu,
            selected_menu_item: 0,
            build_config: BuildConfig::default(),
            deploy_config: DeployConfig::default(),
            logs: Vec::new(),
            status_message: None,
            is_running: true,
            scroll_offset: 0,
            doc_topic: None,
            config_cluster_mode: false,
        }
    }

    pub fn menu_items(&self) -> Vec<MenuItem> {
        MenuItem::all()
    }

    pub fn selected_menu(&self) -> MenuItem {
        let items = self.menu_items();
        items[self.selected_menu_item]
    }

    pub fn next_menu_item(&mut self) {
        let items = self.menu_items();
        self.selected_menu_item = (self.selected_menu_item + 1) % items.len();
    }

    pub fn prev_menu_item(&mut self) {
        let items = self.menu_items();
        self.selected_menu_item = if self.selected_menu_item == 0 {
            items.len() - 1
        } else {
            self.selected_menu_item - 1
        };
    }

    pub fn add_log(&mut self, message: &str) {
        self.logs.push(message.to_string());
        // Keep only last 1000 log lines
        if self.logs.len() > 1000 {
            self.logs.remove(0);
        }
    }

    #[allow(dead_code)]
    pub fn clear_logs(&mut self) {
        self.logs.clear();
    }

    pub fn set_status(&mut self, message: &str) {
        self.status_message = Some(message.to_string());
    }

    #[allow(dead_code)]
    pub fn clear_status(&mut self) {
        self.status_message = None;
    }

    pub fn toggle_build_release(&mut self) {
        self.build_config.release = !self.build_config.release;
    }

    pub fn toggle_build_cluster(&mut self) {
        self.build_config.cluster = !self.build_config.cluster;
    }

    pub fn toggle_deploy_type(&mut self) {
        self.deploy_config.deploy_type = match self.deploy_config.deploy_type {
            DeployType::Single => DeployType::Cluster,
            DeployType::Cluster => DeployType::Single,
        };
    }

    pub fn scroll_up(&mut self) {
        if self.scroll_offset > 0 {
            self.scroll_offset -= 1;
        }
    }

    pub fn scroll_down(&mut self) {
        self.scroll_offset += 1;
    }

    pub fn reset_scroll(&mut self) {
        self.scroll_offset = 0;
    }
}
