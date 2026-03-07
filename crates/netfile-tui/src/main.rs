use anyhow::Result;
use clap::Parser;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use netfile_core::{Config, Device, DiscoveryService, TransferProgress, TransferService};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Gauge},
    Frame, Terminal,
};
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

#[derive(Parser)]
#[command(name = "netfile-tui")]
#[command(about = "NetFile TUI - 终端界面文件传输工具", long_about = None)]
struct Cli {
    #[arg(long, help = "配置文件路径")]
    config: Option<PathBuf>,

    #[arg(long, help = "实例名称")]
    name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Devices,
    Transfers,
    Settings,
}

struct App {
    current_tab: Tab,
    devices: Arc<RwLock<Vec<Device>>>,
    transfers: Arc<RwLock<Vec<TransferProgress>>>,
    config: Config,
    should_quit: bool,
}

impl App {
    fn new(config: Config) -> Self {
        Self {
            current_tab: Tab::Devices,
            devices: Arc::new(RwLock::new(Vec::new())),
            transfers: Arc::new(RwLock::new(Vec::new())),
            config,
            should_quit: false,
        }
    }

    fn next_tab(&mut self) {
        self.current_tab = match self.current_tab {
            Tab::Devices => Tab::Transfers,
            Tab::Transfers => Tab::Settings,
            Tab::Settings => Tab::Devices,
        };
    }

    fn previous_tab(&mut self) {
        self.current_tab = match self.current_tab {
            Tab::Devices => Tab::Settings,
            Tab::Transfers => Tab::Devices,
            Tab::Settings => Tab::Transfers,
        };
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();

    let config_path = cli.config.unwrap_or_else(|| Config::default_path());
    let mut config = if config_path.exists() {
        Config::load(&config_path)?
    } else {
        let config = Config::default();
        config.save(&config_path)?;
        config
    };

    if let Some(name) = cli.name {
        config.instance.instance_name = name;
        config.instance.instance_id = uuid::Uuid::new_v4().to_string();
        config.save(&config_path)?;
    }

    let data_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".netfile")
        .join("data");

    let download_dir = dirs::download_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
        .join("NetFile");

    tokio::fs::create_dir_all(&data_dir).await?;
    tokio::fs::create_dir_all(&download_dir).await?;

    let transfer_service = Arc::new(
        TransferService::new_with_compression(
            config.network.transfer_port,
            config.transfer.max_concurrent,
            config.transfer.chunk_size,
            data_dir,
            download_dir,
            config.transfer.enable_compression,
            config.transfer.speed_limit_mbps as u64 * 1024 * 1024,
            config.transfer.quic_stream_window_mb,
        )
        .await?,
    );

    let transfer_port = transfer_service.local_port();

    let discovery_service = Arc::new(
        DiscoveryService::new(
            config.network.discovery_port,
            config.instance.device_name.clone(),
            config.instance.instance_id.clone(),
            config.instance.device_name.clone(),
            config.instance.instance_name.clone(),
            transfer_port,
            config.network.broadcast_interval,
        )
        .await?,
    );

    let _discovery_handle = {
        let service = discovery_service.clone();
        tokio::spawn(async move {
            service.start().await;
        })
    };

    let _transfer_handle = {
        let service = transfer_service.clone();
        tokio::spawn(async move {
            service.start().await;
        })
    };

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(config);

    let devices_handle = {
        let devices = app.devices.clone();
        let discovery = discovery_service.clone();
        tokio::spawn(async move {
            loop {
                let device_list = discovery.get_devices().await;
                *devices.write().await = device_list;
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        })
    };

    let transfers_handle = {
        let transfers = app.transfers.clone();
        let tracker = transfer_service.progress_tracker();
        tokio::spawn(async move {
            loop {
                let transfer_list = tracker.list_all().await;
                *transfers.write().await = transfer_list;
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        })
    };

    let res = run_app(&mut terminal, &mut app).await;

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    devices_handle.abort();
    transfers_handle.abort();

    if let Err(err) = res {
        println!("Error: {:?}", err);
    }

    Ok(())
}

async fn run_app<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
) -> Result<()> {
    loop {
        terminal.draw(|f| ui(f, app))?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') => {
                        app.should_quit = true;
                    }
                    KeyCode::Tab => {
                        app.next_tab();
                    }
                    KeyCode::BackTab => {
                        app.previous_tab();
                    }
                    KeyCode::Char('1') => {
                        app.current_tab = Tab::Devices;
                    }
                    KeyCode::Char('2') => {
                        app.current_tab = Tab::Transfers;
                    }
                    KeyCode::Char('3') => {
                        app.current_tab = Tab::Settings;
                    }
                    _ => {}
                }
            }
        }

        if app.should_quit {
            break;
        }
    }

    Ok(())
}

fn ui(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(3),
        ])
        .split(f.area());

    render_header(f, chunks[0], app);
    render_content(f, chunks[1], app);
    render_footer(f, chunks[2]);
}

fn render_header(f: &mut Frame, area: Rect, app: &App) {
    let tabs = vec!["[1] Devices", "[2] Transfers", "[3] Settings"];
    let selected = match app.current_tab {
        Tab::Devices => 0,
        Tab::Transfers => 1,
        Tab::Settings => 2,
    };

    let tab_spans: Vec<Span> = tabs
        .iter()
        .enumerate()
        .map(|(i, &tab)| {
            if i == selected {
                Span::styled(
                    format!(" {} ", tab),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::styled(format!(" {} ", tab), Style::default().fg(Color::Gray))
            }
        })
        .collect();

    let header = Paragraph::new(Line::from(tab_spans))
        .block(Block::default().borders(Borders::ALL).title("NetFile TUI"));

    f.render_widget(header, area);
}

fn render_content(f: &mut Frame, area: Rect, app: &App) {
    match app.current_tab {
        Tab::Devices => render_devices(f, area, app),
        Tab::Transfers => render_transfers(f, area, app),
        Tab::Settings => render_settings(f, area, app),
    }
}

fn render_devices(f: &mut Frame, area: Rect, app: &App) {
    let devices = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            app.devices.read().await.clone()
        })
    });

    let items: Vec<ListItem> = devices
        .iter()
        .map(|device| {
            let content = format!(
                "{} - {} ({}:{})",
                device.device_name, device.instance_name, device.ip, device.port
            );
            ListItem::new(content)
        })
        .collect();

    let title = format!("Online Devices ({})", devices.len());
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .style(Style::default().fg(Color::White));

    f.render_widget(list, area);
}

fn render_transfers(f: &mut Frame, area: Rect, app: &App) {
    let transfers = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            app.transfers.read().await.clone()
        })
    });

    if transfers.is_empty() {
        let text = Paragraph::new("No active transfers")
            .block(Block::default().borders(Borders::ALL).title("Transfers"))
            .style(Style::default().fg(Color::Gray));
        f.render_widget(text, area);
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(vec![Constraint::Length(4); transfers.len().min(10)])
        .split(area);

    for (i, transfer) in transfers.iter().take(10).enumerate() {
        let progress = transfer.progress_percent();
        let speed = transfer.speed_mbps();

        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!("{} ({:.1}%)", transfer.file_name, progress));

        let gauge = Gauge::default()
            .block(block)
            .gauge_style(Style::default().fg(Color::Green))
            .percent(progress as u16)
            .label(format!("{:.2} MB/s", speed));

        f.render_widget(gauge, chunks[i]);
    }
}

fn render_settings(f: &mut Frame, area: Rect, app: &App) {
    let settings_text = vec![
        format!("Instance: {}", app.config.instance.instance_name),
        format!("Device: {}", app.config.instance.device_name),
        format!("Discovery Port: {}", app.config.network.discovery_port),
        format!("Transfer Port: {}", app.config.network.transfer_port),
        format!("Chunk Size: {} bytes", app.config.transfer.chunk_size),
        format!("Max Concurrent: {}", app.config.transfer.max_concurrent),
        format!("Compression: {}", app.config.transfer.enable_compression),
        format!("Require Auth: {}", app.config.security.require_auth),
        format!("TLS Enabled: {}", app.config.security.enable_tls),
    ];

    let items: Vec<ListItem> = settings_text
        .iter()
        .map(|text| ListItem::new(text.as_str()))
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Settings"))
        .style(Style::default().fg(Color::White));

    f.render_widget(list, area);
}

fn render_footer(f: &mut Frame, area: Rect) {
    let footer = Paragraph::new("Press 'q' to quit | Tab/Shift+Tab to switch tabs | 1/2/3 for direct tab selection")
        .block(Block::default().borders(Borders::ALL))
        .style(Style::default().fg(Color::Gray));

    f.render_widget(footer, area);
}
