//! TUI module - Terminal UI runner

use crate::app::{App, MenuItem, View};
use crate::commands;
use crate::deploy;
use crate::ui;
use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io;
use std::path::Path;
use std::time::Duration;

/// Run the TUI application
pub async fn run(project_dir: &Path) -> Result<()> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app state
    let mut app = App::new(project_dir.to_path_buf());

    // Main loop
    let result = run_app(&mut terminal, &mut app).await;

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    result
}

async fn run_app<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
) -> Result<()> {
    while app.is_running {
        // Draw UI
        terminal.draw(|f| ui::draw(f, app))?;

        // Handle input with timeout to allow for async operations
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    handle_key_event(app, key.code).await?;
                }
            }
        }
    }

    Ok(())
}

async fn handle_key_event(app: &mut App, key: KeyCode) -> Result<()> {
    match app.current_view {
        View::MainMenu => handle_main_menu_key(app, key).await?,
        View::BuildOptions => handle_build_options_key(app, key).await?,
        View::DockerOptions => handle_docker_options_key(app, key).await?,
        View::DeployOptions => handle_deploy_options_key(app, key).await?,
        View::Config => handle_config_view_key(app, key)?,
        View::Benchmark => handle_benchmark_view_key(app, key).await?,
        View::Optimize | View::Docs | View::Status | View::Log => {
            handle_content_view_key(app, key)?;
        }
    }

    Ok(())
}

async fn handle_main_menu_key(app: &mut App, key: KeyCode) -> Result<()> {
    match key {
        KeyCode::Char('q') | KeyCode::Esc => {
            app.is_running = false;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.prev_menu_item();
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.next_menu_item();
        }
        KeyCode::Enter | KeyCode::Char(' ') => {
            let selected = app.selected_menu();
            match selected {
                MenuItem::Build => {
                    app.current_view = View::BuildOptions;
                    app.reset_scroll();
                }
                MenuItem::Docker => {
                    app.current_view = View::DockerOptions;
                    app.reset_scroll();
                }
                MenuItem::Deploy => {
                    app.current_view = View::DeployOptions;
                    app.reset_scroll();
                }
                MenuItem::Config => {
                    app.current_view = View::Config;
                    app.reset_scroll();
                }
                MenuItem::Benchmark => {
                    app.current_view = View::Benchmark;
                    app.reset_scroll();
                }
                MenuItem::Optimize => {
                    app.current_view = View::Optimize;
                    app.reset_scroll();
                }
                MenuItem::Docs => {
                    app.current_view = View::Docs;
                    app.reset_scroll();
                }
                MenuItem::Status => {
                    app.current_view = View::Status;
                    app.reset_scroll();
                }
                MenuItem::Quit => {
                    app.is_running = false;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

async fn handle_build_options_key(app: &mut App, key: KeyCode) -> Result<()> {
    match key {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.current_view = View::MainMenu;
        }
        KeyCode::Char('r') => {
            app.toggle_build_release();
        }
        KeyCode::Char('c') => {
            app.toggle_build_cluster();
        }
        KeyCode::Enter | KeyCode::Char('b') => {
            app.set_status("Building AiKv...");
            app.add_log("Starting build process...");

            // Run build command
            match commands::build(
                &app.project_dir,
                app.build_config.cluster,
                app.build_config.release,
            )
            .await
            {
                Ok(()) => {
                    app.add_log("Build completed successfully!");
                    app.set_status("Build completed successfully!");
                }
                Err(e) => {
                    app.add_log(&format!("Build failed: {}", e));
                    app.set_status(&format!("Build failed: {}", e));
                }
            }
        }
        _ => {}
    }
    Ok(())
}

async fn handle_docker_options_key(app: &mut App, key: KeyCode) -> Result<()> {
    match key {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.current_view = View::MainMenu;
        }
        KeyCode::Char('c') => {
            app.toggle_build_cluster();
        }
        KeyCode::Enter | KeyCode::Char('b') => {
            app.set_status("Building Docker image...");
            app.add_log("Starting Docker build process...");

            let tag = if app.build_config.cluster {
                "cluster"
            } else {
                "latest"
            };

            match commands::build_docker(&app.project_dir, app.build_config.cluster, tag).await {
                Ok(()) => {
                    app.add_log("Docker image built successfully!");
                    app.set_status("Docker image built successfully!");
                }
                Err(e) => {
                    app.add_log(&format!("Docker build failed: {}", e));
                    app.set_status(&format!("Docker build failed: {}", e));
                }
            }
        }
        _ => {}
    }
    Ok(())
}

async fn handle_deploy_options_key(app: &mut App, key: KeyCode) -> Result<()> {
    match key {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.current_view = View::MainMenu;
        }
        KeyCode::Char('t') => {
            app.toggle_deploy_type();
        }
        KeyCode::Char('+') => {
            if app.deploy_config.node_count < 9 {
                app.deploy_config.node_count += 1;
            }
        }
        KeyCode::Char('-') => {
            if app.deploy_config.node_count > 1 {
                app.deploy_config.node_count -= 1;
            }
        }
        KeyCode::Enter | KeyCode::Char('g') => {
            app.set_status("Generating deployment files...");
            app.add_log("Starting deployment generation...");

            match deploy::generate(
                &app.project_dir,
                app.deploy_config.deploy_type.as_str(),
                &app.deploy_config.output_dir,
                None,
            )
            .await
            {
                Ok(()) => {
                    app.add_log(&format!(
                        "Deployment files generated in {:?}",
                        app.deploy_config.output_dir
                    ));
                    app.set_status("Deployment files generated successfully!");
                }
                Err(e) => {
                    app.add_log(&format!("Deployment generation failed: {}", e));
                    app.set_status(&format!("Deployment failed: {}", e));
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn handle_config_view_key(app: &mut App, key: KeyCode) -> Result<()> {
    match key {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.current_view = View::MainMenu;
        }
        KeyCode::Char('c') => {
            app.config_cluster_mode = !app.config_cluster_mode;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.scroll_up();
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.scroll_down();
        }
        KeyCode::PageUp => {
            for _ in 0..10 {
                app.scroll_up();
            }
        }
        KeyCode::PageDown => {
            for _ in 0..10 {
                app.scroll_down();
            }
        }
        _ => {}
    }
    Ok(())
}

async fn handle_benchmark_view_key(app: &mut App, key: KeyCode) -> Result<()> {
    match key {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.current_view = View::MainMenu;
        }
        KeyCode::Char('1') => {
            app.set_status("Running quick benchmark...");
            match commands::run_benchmark(&app.project_dir, "quick").await {
                Ok(()) => {
                    app.set_status("Quick benchmark completed!");
                }
                Err(e) => {
                    app.set_status(&format!("Benchmark failed: {}", e));
                }
            }
        }
        KeyCode::Char('2') => {
            app.set_status("Running full benchmark...");
            match commands::run_benchmark(&app.project_dir, "full").await {
                Ok(()) => {
                    app.set_status("Full benchmark completed!");
                }
                Err(e) => {
                    app.set_status(&format!("Benchmark failed: {}", e));
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn handle_content_view_key(app: &mut App, key: KeyCode) -> Result<()> {
    match key {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.current_view = View::MainMenu;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.scroll_up();
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.scroll_down();
        }
        KeyCode::PageUp => {
            for _ in 0..10 {
                app.scroll_up();
            }
        }
        KeyCode::PageDown => {
            for _ in 0..10 {
                app.scroll_down();
            }
        }
        _ => {}
    }
    Ok(())
}
