//! UI module - Rendering components

use crate::app::{App, DeployType, View};
use crate::config;
use crate::docs;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
    Frame,
};

/// Main draw function
pub fn draw(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Length(3), // Title
            Constraint::Min(10),   // Main content
            Constraint::Length(3), // Status bar
        ])
        .split(frame.area());

    draw_title(frame, chunks[0]);
    draw_main_content(frame, app, chunks[1]);
    draw_status_bar(frame, app, chunks[2]);
}

fn draw_title(frame: &mut Frame, area: Rect) {
    let title = Paragraph::new(vec![Line::from(vec![
        Span::styled(
            "🔧 AiKv Toolchain ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("v0.1.0", Style::default().fg(Color::Gray)),
        Span::raw(" - Project Management Tool"),
    ])])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
    );
    frame.render_widget(title, area);
}

fn draw_main_content(frame: &mut Frame, app: &App, area: Rect) {
    match app.current_view {
        View::MainMenu => draw_main_menu(frame, app, area),
        View::BuildOptions => draw_build_options(frame, app, area),
        View::DockerOptions => draw_docker_options(frame, app, area),
        View::DeployOptions => draw_deploy_options(frame, app, area),
        View::Config => draw_config_view(frame, app, area),
        View::Benchmark => draw_benchmark_view(frame, app, area),
        View::Optimize => draw_optimize_view(frame, app, area),
        View::Docs => draw_docs_view(frame, app, area),
        View::Status => draw_status_view(frame, app, area),
        View::Log => draw_log_view(frame, app, area),
    }
}

fn draw_main_menu(frame: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(area);

    // Menu list
    let items: Vec<ListItem> = app
        .menu_items()
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let style = if i == app.selected_menu_item {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };

            let prefix = if i == app.selected_menu_item {
                "▶ "
            } else {
                "  "
            };

            ListItem::new(Line::from(Span::styled(
                format!("{}{}", prefix, item.title()),
                style,
            )))
        })
        .collect();

    let menu = List::new(items).block(
        Block::default()
            .title(" Main Menu ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
    );
    frame.render_widget(menu, chunks[0]);

    // Description
    let selected = app.selected_menu();
    let description = Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::styled(
            selected.title(),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(selected.description()),
        Line::from(""),
        Line::from(""),
        Line::from(Span::styled(
            "Keyboard Shortcuts:",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from("  ↑/k   Move up"),
        Line::from("  ↓/j   Move down"),
        Line::from("  Enter Select"),
        Line::from("  q     Quit"),
    ])
    .block(
        Block::default()
            .title(" Description ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Blue)),
    );
    frame.render_widget(description, chunks[1]);
}

fn draw_build_options(frame: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(12), Constraint::Min(5)])
        .split(area);

    let release_status = if app.build_config.release {
        "[✓]"
    } else {
        "[ ]"
    };
    let cluster_status = if app.build_config.cluster {
        "[✓]"
    } else {
        "[ ]"
    };

    let content = Paragraph::new(vec![
        Line::from(Span::styled(
            "Build Configuration",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(format!("  {} Release mode (r)", release_status)),
        Line::from(format!("  {} Cluster feature (c)", cluster_status)),
        Line::from(""),
        Line::from(""),
        Line::from(Span::styled(
            "Commands:",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from("  r - Toggle release mode"),
        Line::from("  c - Toggle cluster feature"),
        Line::from("  b/Enter - Start build"),
        Line::from("  q/Esc - Back to menu"),
    ])
    .block(
        Block::default()
            .title(" 🔨 Build AiKv ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
    );
    frame.render_widget(content, chunks[0]);

    // Logs
    draw_logs(frame, app, chunks[1]);
}

fn draw_docker_options(frame: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(12), Constraint::Min(5)])
        .split(area);

    let cluster_status = if app.build_config.cluster {
        "[✓]"
    } else {
        "[ ]"
    };
    let tag = if app.build_config.cluster {
        "cluster"
    } else {
        "latest"
    };

    let content = Paragraph::new(vec![
        Line::from(Span::styled(
            "Docker Build Configuration",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(format!("  {} Cluster feature (c)", cluster_status)),
        Line::from(format!("  Image tag: aikv:{}", tag)),
        Line::from(""),
        Line::from(""),
        Line::from(Span::styled(
            "Commands:",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from("  c - Toggle cluster feature"),
        Line::from("  b/Enter - Start Docker build"),
        Line::from("  q/Esc - Back to menu"),
    ])
    .block(
        Block::default()
            .title(" 🐳 Build Docker Image ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
    );
    frame.render_widget(content, chunks[0]);

    draw_logs(frame, app, chunks[1]);
}

fn draw_deploy_options(frame: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(16), Constraint::Min(5)])
        .split(area);

    let deploy_type = match app.deploy_config.deploy_type {
        DeployType::Single => "Single Node",
        DeployType::Cluster => "Cluster",
    };

    let content = Paragraph::new(vec![
        Line::from(Span::styled(
            "Deployment Configuration",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(format!("  Deployment Type: {} (t)", deploy_type)),
        Line::from(format!(
            "  Output Directory: {:?}",
            app.deploy_config.output_dir
        )),
        Line::from(format!(
            "  Node Count: {} (+/-)",
            app.deploy_config.node_count
        )),
        Line::from(format!("  Base Port: {}", app.deploy_config.base_port)),
        Line::from(format!(
            "  Storage Engine: {}",
            app.deploy_config.storage_engine
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Generated Files:",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from("  • docker-compose.yml"),
        Line::from("  • aikv.toml / aikv-cluster.toml"),
        Line::from("  • README.md"),
        Line::from(""),
        Line::from(Span::styled(
            "Commands:",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from("  t - Toggle deployment type | +/- - Adjust node count"),
        Line::from("  g/Enter - Generate | q/Esc - Back"),
    ])
    .block(
        Block::default()
            .title(" 📦 Generate Deployment ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
    );
    frame.render_widget(content, chunks[0]);

    draw_logs(frame, app, chunks[1]);
}

fn draw_config_view(frame: &mut Frame, app: &App, area: Rect) {
    let config_text = if app.config_cluster_mode {
        config::get_cluster_config_docs()
    } else {
        config::get_single_config_docs()
    };

    let lines: Vec<Line> = config_text.lines().map(Line::from).collect();
    let visible_lines = if app.scroll_offset < lines.len() {
        lines[app.scroll_offset..].to_vec()
    } else {
        vec![]
    };

    let mode_text = if app.config_cluster_mode {
        "Cluster"
    } else {
        "Single Node"
    };

    let content = Paragraph::new(visible_lines)
        .block(
            Block::default()
                .title(format!(" ⚙️  Configuration Documentation ({}) ", mode_text))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap {
            trim: false,
        });
    frame.render_widget(content, area);
}

fn draw_benchmark_view(frame: &mut Frame, _app: &App, area: Rect) {
    let content = Paragraph::new(vec![
        Line::from(Span::styled(
            "Performance Benchmarks",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Available Benchmarks:",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("  1 - Quick Benchmark"),
        Line::from("      Run basic performance tests (SET, GET, PING)"),
        Line::from("      Approximate time: 30 seconds"),
        Line::from(""),
        Line::from("  2 - Full Benchmark"),
        Line::from("      Run comprehensive tests using cargo bench"),
        Line::from("      Approximate time: 5-10 minutes"),
        Line::from(""),
        Line::from(""),
        Line::from(Span::styled(
            "Expected Performance Targets:",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from("  • SET: ~80,000 ops/s"),
        Line::from("  • GET: ~100,000 ops/s"),
        Line::from("  • LPUSH: ~75,000 ops/s"),
        Line::from("  • HSET: ~70,000 ops/s"),
        Line::from(""),
        Line::from(Span::styled(
            "Latency Targets:",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from("  • P50: < 1ms"),
        Line::from("  • P99: < 5ms"),
        Line::from("  • P99.9: < 10ms"),
        Line::from(""),
        Line::from("Press q/Esc to return"),
    ])
    .block(
        Block::default()
            .title(" 📊 Benchmarks ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
    );
    frame.render_widget(content, area);
}

fn draw_optimize_view(frame: &mut Frame, app: &App, area: Rect) {
    let content_text = docs::get_optimization_text();
    let lines: Vec<Line> = content_text.lines().map(Line::from).collect();
    let visible_lines = if app.scroll_offset < lines.len() {
        lines[app.scroll_offset..].to_vec()
    } else {
        vec![]
    };

    let content = Paragraph::new(visible_lines)
        .block(
            Block::default()
                .title(" 🚀 Optimization Suggestions ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap {
            trim: false,
        });
    frame.render_widget(content, area);
}

fn draw_docs_view(frame: &mut Frame, app: &App, area: Rect) {
    let content_text = docs::get_documentation_text();
    let lines: Vec<Line> = content_text.lines().map(Line::from).collect();
    let visible_lines = if app.scroll_offset < lines.len() {
        lines[app.scroll_offset..].to_vec()
    } else {
        vec![]
    };

    let content = Paragraph::new(visible_lines)
        .block(
            Block::default()
                .title(" 📖 Documentation ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap {
            trim: false,
        });
    frame.render_widget(content, area);
}

fn draw_status_view(frame: &mut Frame, app: &App, area: Rect) {
    let status_info = get_status_info(app);
    let lines: Vec<Line> = status_info.lines().map(Line::from).collect();
    let visible_lines = if app.scroll_offset < lines.len() {
        lines[app.scroll_offset..].to_vec()
    } else {
        vec![]
    };

    let content = Paragraph::new(visible_lines)
        .block(
            Block::default()
                .title(" ℹ️  Project Status ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap {
            trim: false,
        });
    frame.render_widget(content, area);
}

fn draw_log_view(frame: &mut Frame, app: &App, area: Rect) {
    draw_logs(frame, app, area);
}

fn draw_logs(frame: &mut Frame, app: &App, area: Rect) {
    let log_lines: Vec<Line> = app
        .logs
        .iter()
        .rev()
        .take(50)
        .rev()
        .map(|s| Line::from(s.as_str()))
        .collect();

    let logs = Paragraph::new(log_lines).block(
        Block::default()
            .title(" Logs ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    frame.render_widget(logs, area);
}

fn draw_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let status_text = match &app.status_message {
        Some(msg) => msg.clone(),
        None => match app.current_view {
            View::MainMenu => "Use ↑/↓ to navigate, Enter to select, q to quit".to_string(),
            _ => "Press q/Esc to go back, ↑/↓ to scroll".to_string(),
        },
    };

    let status = Paragraph::new(status_text)
        .style(Style::default().fg(Color::White))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        );
    frame.render_widget(status, area);
}

fn get_status_info(app: &App) -> String {
    use sysinfo::System;

    let mut sys = System::new_all();
    sys.refresh_all();

    let total_memory = sys.total_memory() / 1024 / 1024;
    let used_memory = sys.used_memory() / 1024 / 1024;
    let cpu_count = sys.cpus().len();

    let project_exists = app.project_dir.join("Cargo.toml").exists();
    let has_target = app.project_dir.join("target").exists();
    let has_release = app.project_dir.join("target/release/aikv").exists();

    format!(
        r#"Project Status
==============

Project Directory: {:?}
Cargo.toml exists: {}
Target directory exists: {}
Release binary exists: {}

System Information
==================

CPU Cores: {}
Total Memory: {} MB
Used Memory: {} MB
Available Memory: {} MB

AiKv Features
=============

• Single Node Mode: Supported
• Cluster Mode: Supported (with --features cluster)
• Storage Engines: memory, aidb
• Supported Commands: 100+
• Tests: 96 passing

Build Commands
==============

• cargo build                   - Debug build
• cargo build --release         - Release build
• cargo build --release --features cluster - Cluster build

Press q/Esc to return to main menu"#,
        app.project_dir,
        project_exists,
        has_target,
        has_release,
        cpu_count,
        total_memory,
        used_memory,
        total_memory - used_memory
    )
}
