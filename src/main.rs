use clap::{Parser, Subcommand};
use crossterm::{
    event::{self, Event},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use lofi::app::{App, PlayerCommand, PlayerState};
use lofi::audio::{AudioCommand, AudioEvent, AudioWorker, probe_audio_output};
use lofi::config::Config;
use lofi::event::key_to_command;
use lofi::station::find_station;
use lofi::stream::StreamSource;
use lofi::ui;
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io::{self, IsTerminal};
use std::time::Duration;
use tracing::warn;

#[derive(Parser)]
#[command(name = "lofi", version, about = "Terminal lo-fi / ambient internet radio")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// List available stations and exit.
    List,
    /// Start playback of a station (headless; streams until Ctrl-C).
    Play {
        /// Station name (case-insensitive).
        station: String,
    },
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "lofi=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let config = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("lofi: {e}");
            eprintln!("Using built-in stations.");
            Config::default()
        }
    };

    match cli.command {
        Some(Commands::List) => {
            for s in &config.stations {
                println!("{}\n  {} — {}", s.name, s.description, s.url);
            }
        }
        Some(Commands::Play { station }) => {
            let Some(st) = find_station(&config.stations, &station) else {
                eprintln!("lofi: unknown station '{station}'. Try `lofi list`.");
                std::process::exit(1);
            };
            if let Err(e) = run_headless(st.url.clone(), config.volume).await {
                eprintln!("lofi: {e}");
                std::process::exit(1);
            }
        }
        None => {
            if !io::stdin().is_terminal() {
                eprintln!("lofi: no subcommand given and stdin is not a terminal.");
                eprintln!("Try `lofi list` or `lofi play \"<station>\"`.");
                std::process::exit(2);
            }
            // Probe for an audio device on the plain terminal, BEFORE the
            // alternate screen goes up: a missing device makes ALSA print
            // errors to stderr, which would corrupt TUI escape sequences.
            let audio_available = probe_audio_output();
            if audio_available == Some(false) {
                eprintln!("lofi: no audio output device found — showing stream info only.");
            }
            if let Err(e) = run_tui(config, audio_available).await {
                // Terminal is always restored (see run_tui); just report.
                eprintln!("lofi: {e}");
                std::process::exit(1);
            }
        }
    }
}

/// Headless playback for `lofi play <station>`: streams until Ctrl-C,
/// with the same reconnect backoff as the TUI.
async fn run_headless(url: String, volume: f32) -> Result<(), String> {
    let mut worker = AudioWorker::spawn();
    worker.send(AudioCommand::SetVolume(volume));
    worker.send(AudioCommand::Play(StreamSource::new(&url)));
    println!("Playing {url} — press Ctrl-C to stop.");
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                worker.send(AudioCommand::Stop);
                break;
            }
            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                for ev in worker.drain_events() {
                    match ev {
                        AudioEvent::Connected { icy_name } => {
                            println!("connected{}", icy_name.map(|n| format!(" ({n})")).unwrap_or_default());
                        }
                        AudioEvent::Playing => println!("playing…"),
                        AudioEvent::Metadata { title } => println!("now playing: {title}"),
                        AudioEvent::Disconnected { reason, attempt } => {
                            println!("disconnected ({reason}); retry {attempt}…");
                        }
                        AudioEvent::Failed { reason } => {
                            return Err(format!("Unable to reconnect. {reason}"));
                        }
                        AudioEvent::Levels { .. } => {}
                    }
                }
            }
        }
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
enum TuiError {
    #[error("terminal error: {0}")]
    Terminal(String),
}

/// Full TUI. Guarantees terminal restoration on exit, error, and panic:
/// - raw mode + alternate screen are set up here and torn down in a guard
/// - a panic hook restores the terminal before printing the panic
/// - no `unwrap()` on terminal operations; errors propagate as messages.
async fn run_tui(config: Config, audio_available: Option<bool>) -> Result<(), TuiError> {
    // Panic hook: restore terminal before the default panic output.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        default_hook(info);
    }));

    enable_raw_mode().map_err(|e| TuiError::Terminal(e.to_string()))?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).map_err(|e| TuiError::Terminal(e.to_string()))?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal =
        Terminal::new(backend).map_err(|e| TuiError::Terminal(e.to_string()))?;

    let result = run_app_loop(&mut terminal, config, audio_available).await;

    // Always restore, even on error.
    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    let _ = terminal.show_cursor();

    result
}

async fn run_app_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    config: Config,
    audio_available: Option<bool>,
) -> Result<(), TuiError> {
    let mut app = App::new(&config);
    app.audio_available = audio_available;
    let mut worker = AudioWorker::spawn();
    worker.send(AudioCommand::SetVolume(app.effective_volume()));

    // Auto-play default station if configured? Spec: `lofi` opens the TUI;
    // playback starts on Enter. Keep it manual for a fast, silent start.

    loop {
        terminal
            .draw(|f| ui::render(f, &app))
            .map_err(|e| TuiError::Terminal(e.to_string()))?;

        // Pump audio worker events into app state.
        for ev in worker.drain_events() {
            match ev {
                AudioEvent::Connected { icy_name } => {
                    app.on_connected();
                    if let Some(name) = icy_name {
                        if app.current_title.is_none() {
                            app.status_message = Some(name);
                        }
                    }
                }
                AudioEvent::Playing => app.on_playing(),
                AudioEvent::Disconnected { reason, attempt } => {
                    app.on_disconnected(&reason, attempt);
                }
                AudioEvent::Failed { reason } => app.on_failed(&reason),
                AudioEvent::Metadata { title } => app.on_metadata(title),
                AudioEvent::Levels { bands, wave } => app.on_levels(bands, wave),
            }
        }

        // Keyboard input (poll so the UI stays responsive during reconnects).
        if event::poll(Duration::from_millis(100))
            .map_err(|e| TuiError::Terminal(e.to_string()))?
        {
            let Event::Key(key) =
                event::read().map_err(|e| TuiError::Terminal(e.to_string()))?
            else {
                continue;
            };
            if key.kind != crossterm::event::KeyEventKind::Press {
                continue;
            }
            let Some(cmd) = key_to_command(key, app.selected, app.stations.len())
            else {
                continue;
            };
            let quit = cmd == PlayerCommand::Quit;
            // Forward playback-affecting commands to the audio worker.
            match &cmd {
                PlayerCommand::Play => {
                    if let Some(st) = app.selected_station() {
                        worker.send(AudioCommand::Play(StreamSource::new(&st.url)));
                    }
                }
                PlayerCommand::TogglePause => {
                    if app.player_state == PlayerState::Playing {
                        worker.send(AudioCommand::Pause);
                    } else if app.player_state == PlayerState::Paused {
                        worker.send(AudioCommand::Resume);
                    } else if app.current.is_none() {
                        if let Some(st) = app.selected_station() {
                            worker.send(AudioCommand::Play(StreamSource::new(&st.url)));
                        }
                    }
                }
                PlayerCommand::Pause => worker.send(AudioCommand::Pause),
                PlayerCommand::Stop => worker.send(AudioCommand::Stop),
                PlayerCommand::VolumeUp | PlayerCommand::VolumeDown | PlayerCommand::ToggleMute => {
                    // Applied to app first below; worker synced after.
                }
                PlayerCommand::Reconnect => {
                    if let Some(st) = app.current_station().cloned() {
                        worker.send(AudioCommand::Play(StreamSource::new(&st.url)));
                    }
                }
                PlayerCommand::Next
                | PlayerCommand::Previous
                | PlayerCommand::SelectStation(_)
                | PlayerCommand::CycleVis
                | PlayerCommand::CyclePalette => {
                    // UI-only state; no audio worker involvement.
                }
                PlayerCommand::Quit => {
                    worker.send(AudioCommand::Stop);
                }
            }
            app.apply(cmd);
            // Keep worker volume in sync (mute included).
            worker.send(AudioCommand::SetVolume(app.effective_volume()));

            if quit {
                app.should_quit = true;
            }
            if app.should_quit {
                // Persist volume for next launch (best-effort).
                let to_save = Config {
                    volume: app.volume,
                    default_station: config.default_station.clone(),
                    stations: app.stations.clone(),
                    file_path: None,
                };
                if let Err(e) = to_save.save() {
                    warn!("could not save config: {e}");
                }
                break;
            }
        }
    }
    Ok(())
}
