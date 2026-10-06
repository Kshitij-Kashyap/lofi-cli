use crate::audio::{N_BANDS, N_WAVE};
use crate::config::{Config, VOLUME_STEP};
use crate::station::Station;
use std::time::{Duration, Instant};

/// Commands produced by keyboard input / CLI. The TUI never touches audio directly.
#[derive(Debug, Clone, PartialEq)]
pub enum PlayerCommand {
    Play,
    Pause,
    TogglePause,
    Stop,
    Next,
    Previous,
    VolumeUp,
    VolumeDown,
    ToggleMute,
    Reconnect,
    SelectStation(usize),
    CycleVis,
    CyclePalette,
    Quit,
}

/// Visualizer panel mode. `V` cycles Bars → Wave → Off → Bars.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VisMode {
    #[default]
    Bars,
    Wave,
    Off,
}

impl VisMode {
    pub fn next(self) -> Self {
        match self {
            Self::Bars => Self::Wave,
            Self::Wave => Self::Off,
            Self::Off => Self::Bars,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Bars => "bars",
            Self::Wave => "wave",
            Self::Off => "off",
        }
    }
}

/// Bar/wave color theme. `C` cycles through all four.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Palette {
    #[default]
    Neon,
    Ember,
    Forest,
    Mono,
}

impl Palette {
    pub fn next(self) -> Self {
        match self {
            Self::Neon => Self::Ember,
            Self::Ember => Self::Forest,
            Self::Forest => Self::Mono,
            Self::Mono => Self::Neon,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Neon => "neon",
            Self::Ember => "ember",
            Self::Forest => "forest",
            Self::Mono => "mono",
        }
    }
}

/// Format a session duration as MM:SS or H:MM:SS.
pub fn fmt_elapsed(d: Duration) -> String {
    let s = d.as_secs();
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m:02}:{sec:02}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlayerState {
    #[default]
    Stopped,
    Connecting,
    Buffering,
    Playing,
    Paused,
    Reconnecting,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConnectionState {
    #[default]
    Disconnected,
    Connecting,
    Connected,
    Reconnecting,
    Failed,
}

#[derive(Debug, Clone)]
pub struct App {
    pub stations: Vec<Station>,
    pub selected: usize,
    pub current: Option<usize>,

    pub player_state: PlayerState,
    pub connection: ConnectionState,

    pub volume: f32,
    pub muted: bool,
    prev_volume: f32,

    pub current_title: Option<String>,
    pub status_message: Option<String>,
    pub reconnect_attempt: u32,

    /// None = silent build; Some(true/false) = audio device probe result.
    /// Set by main() before the TUI loop; purely informational for the UI.
    pub audio_available: Option<bool>,

    pub vis: VisMode,
    pub palette: Palette,
    pub session_start: Instant,

    /// Latest spectrum levels from the decode thread + arrival time.
    /// Stale levels fade to zero in `display_levels()`.
    pub bands: [f32; N_BANDS],
    pub wave: [f32; N_WAVE],
    pub levels_at: Option<Instant>,

    pub should_quit: bool,
}

impl App {
    pub fn new(config: &Config) -> Self {
        // Pre-select default station if configured.
        let mut selected = 0;
        if let Some(want) = &config.default_station {
            let needle = want.to_lowercase();
            if let Some(i) = config
                .stations
                .iter()
                .position(|s| s.name.to_lowercase() == needle)
            {
                selected = i;
            }
        }
        Self {
            stations: config.stations.clone(),
            selected,
            current: None,
            player_state: PlayerState::Stopped,
            connection: ConnectionState::Disconnected,
            volume: config.volume.clamp(0.0, 1.0),
            muted: false,
            prev_volume: config.volume.clamp(0.0, 1.0),
            current_title: None,
            status_message: None,
            reconnect_attempt: 0,
            audio_available: None,
            vis: VisMode::default(),
            palette: Palette::default(),
            session_start: Instant::now(),
            bands: [0.0; N_BANDS],
            wave: [0.0; N_WAVE],
            levels_at: None,
            should_quit: false,
        }
    }

    pub fn selected_station(&self) -> Option<&Station> {
        self.stations.get(self.selected)
    }

    pub fn current_station(&self) -> Option<&Station> {
        self.current.and_then(|i| self.stations.get(i))
    }

    pub fn effective_volume(&self) -> f32 {
        if self.muted {
            0.0
        } else {
            self.volume
        }
    }

    pub fn apply(&mut self, cmd: PlayerCommand) {
        match cmd {
            PlayerCommand::Quit => self.should_quit = true,
            PlayerCommand::SelectStation(i) => {
                if i < self.stations.len() {
                    self.selected = i;
                }
            }
            PlayerCommand::Next => {
                if !self.stations.is_empty() {
                    self.selected = (self.selected + 1) % self.stations.len();
                }
            }
            PlayerCommand::Previous => {
                if !self.stations.is_empty() {
                    self.selected = if self.selected == 0 {
                        self.stations.len() - 1
                    } else {
                        self.selected - 1
                    };
                }
            }
            PlayerCommand::Play => {
                if !self.stations.is_empty() {
                    self.current = Some(self.selected);
                    self.player_state = PlayerState::Connecting;
                    self.connection = ConnectionState::Connecting;
                    self.status_message = None;
                    self.reconnect_attempt = 0;
                }
            }
            PlayerCommand::Pause => {
                if self.player_state == PlayerState::Playing {
                    self.player_state = PlayerState::Paused;
                }
            }
            PlayerCommand::TogglePause => {
                match self.player_state {
                    PlayerState::Playing => self.player_state = PlayerState::Paused,
                    PlayerState::Paused => self.player_state = PlayerState::Playing,
                    _ => {
                        // Space on a stopped player starts playback (convenience).
                        if !self.stations.is_empty() && self.current.is_none() {
                            self.current = Some(self.selected);
                            self.player_state = PlayerState::Connecting;
                            self.connection = ConnectionState::Connecting;
                        }
                    }
                }
            }
            PlayerCommand::Stop => {
                self.player_state = PlayerState::Stopped;
                self.connection = ConnectionState::Disconnected;
                self.current = None;
                self.current_title = None;
            }
            PlayerCommand::VolumeUp => {
                self.volume = (self.volume + VOLUME_STEP).clamp(0.0, 1.0);
                if self.volume > 0.0 {
                    self.muted = false;
                }
            }
            PlayerCommand::VolumeDown => {
                self.volume = (self.volume - VOLUME_STEP).clamp(0.0, 1.0);
                if self.volume > 0.0 {
                    // keep muted flag as-is when lowering? Standard: unmute on change.
                    self.muted = false;
                }
            }
            PlayerCommand::ToggleMute => {
                if self.muted {
                    self.muted = false;
                    self.volume = self.prev_volume;
                } else {
                    self.prev_volume = self.volume;
                    self.muted = true;
                }
            }
            PlayerCommand::Reconnect => {
                if self.current.is_some() {
                    self.player_state = PlayerState::Reconnecting;
                    self.connection = ConnectionState::Reconnecting;
                    self.reconnect_attempt = 0;
                    self.status_message = Some("Reconnecting…".to_string());
                }
            }
            PlayerCommand::CycleVis => {
                self.vis = self.vis.next();
            }
            PlayerCommand::CyclePalette => {
                self.palette = self.palette.next();
            }
        }
    }

    // --- events coming back from the audio/stream worker ---

    pub fn on_connected(&mut self) {
        self.connection = ConnectionState::Connected;
        if self.player_state == PlayerState::Connecting
            || self.player_state == PlayerState::Reconnecting
        {
            self.player_state = PlayerState::Buffering;
        }
        self.status_message = None;
        self.reconnect_attempt = 0;
    }

    pub fn on_playing(&mut self) {
        self.connection = ConnectionState::Connected;
        self.player_state = PlayerState::Playing;
        self.status_message = None;
    }

    pub fn on_disconnected(&mut self, reason: &str, attempt: u32) {
        self.connection = ConnectionState::Reconnecting;
        self.player_state = PlayerState::Reconnecting;
        self.reconnect_attempt = attempt;
        self.status_message = Some(format!("Reconnecting… ({reason})"));
    }

    pub fn on_failed(&mut self, reason: &str) {
        self.connection = ConnectionState::Failed;
        self.player_state = PlayerState::Error;
        self.status_message = Some(format!("Unable to reconnect. {reason} Press R to retry."));
    }

    pub fn on_metadata(&mut self, title: String) {
        if !title.trim().is_empty() {
            self.current_title = Some(title);
        }
    }

    pub fn elapsed(&self) -> Duration {
        self.session_start.elapsed()
    }

    pub fn on_levels(&mut self, bands: Vec<f32>, wave: Vec<f32>) {
        for (i, slot) in self.bands.iter_mut().enumerate() {
            *slot = bands.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0);
        }
        for (i, slot) in self.wave.iter_mut().enumerate() {
            *slot = wave.get(i).copied().unwrap_or(0.0).clamp(-1.0, 1.0);
        }
        self.levels_at = Some(Instant::now());
    }

    /// Levels for display, faded toward zero when stale (stream stalled,
    /// unsupported codec, or idle). Fresh data (<250ms) passes through.
    pub fn display_levels(&self) -> (Vec<f32>, Vec<f32>) {
        let fade = match self.levels_at {
            None => 0.0,
            Some(t) => {
                let age_ms = t.elapsed().as_millis() as f32;
                (1.0 - (age_ms - 250.0) / 600.0).clamp(0.0, 1.0)
            }
        };
        (
            self.bands.iter().map(|b| b * fade).collect(),
            self.wave.iter().map(|w| w * fade).collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app() -> App {
        App::new(&Config::default())
    }

    #[test]
    fn volume_stays_in_bounds() {
        let mut app = test_app();
        app.volume = 1.0;
        app.apply(PlayerCommand::VolumeUp);
        assert!((app.volume - 1.0).abs() < f32::EPSILON);
        app.volume = 0.0;
        app.apply(PlayerCommand::VolumeDown);
        assert!((app.volume - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn mute_restores_previous_volume() {
        let mut app = test_app();
        app.volume = 0.7;
        app.apply(PlayerCommand::ToggleMute);
        assert!(app.muted);
        assert_eq!(app.effective_volume(), 0.0);
        app.apply(PlayerCommand::ToggleMute);
        assert!(!app.muted);
        assert!((app.volume - 0.7).abs() < f32::EPSILON);
    }

    #[test]
    fn selection_wraps() {
        let mut app = test_app();
        let n = app.stations.len();
        app.selected = n - 1;
        app.apply(PlayerCommand::Next);
        assert_eq!(app.selected, 0);
        app.apply(PlayerCommand::Previous);
        assert_eq!(app.selected, n - 1);
    }

    #[test]
    fn play_sets_connecting() {
        let mut app = test_app();
        app.apply(PlayerCommand::Play);
        assert_eq!(app.player_state, PlayerState::Connecting);
        assert_eq!(app.current, Some(0));
        app.on_connected();
        assert_eq!(app.connection, ConnectionState::Connected);
        app.on_playing();
        assert_eq!(app.player_state, PlayerState::Playing);
    }

    #[test]
    fn reconnect_flow() {
        let mut app = test_app();
        app.apply(PlayerCommand::Play);
        app.on_disconnected("timeout", 1);
        assert_eq!(app.connection, ConnectionState::Reconnecting);
        app.on_failed("timeout");
        assert_eq!(app.connection, ConnectionState::Failed);
        assert!(app.status_message.as_ref().unwrap().contains('R'));
    }

    #[test]
    fn vis_and_palette_cycle() {
        let mut app = test_app();
        assert_eq!(app.vis, VisMode::Bars);
        app.apply(PlayerCommand::CycleVis);
        assert_eq!(app.vis, VisMode::Wave);
        app.apply(PlayerCommand::CycleVis);
        assert_eq!(app.vis, VisMode::Off);
        app.apply(PlayerCommand::CycleVis);
        assert_eq!(app.vis, VisMode::Bars);

        assert_eq!(app.palette, Palette::Neon);
        for _ in 0..4 {
            app.apply(PlayerCommand::CyclePalette);
        }
        assert_eq!(app.palette, Palette::Neon);
    }

    #[test]
    fn fmt_elapsed_cases() {
        assert_eq!(fmt_elapsed(Duration::from_secs(0)), "00:00");
        assert_eq!(fmt_elapsed(Duration::from_secs(65)), "01:05");
        assert_eq!(fmt_elapsed(Duration::from_secs(3723)), "1:02:03");
    }

    #[test]
    fn stale_levels_fade_to_zero() {
        use std::time::Duration as StdDuration;
        let mut app = test_app();
        app.on_levels(vec![1.0; 32], vec![0.5; 48]);
        let (bands, _) = app.display_levels();
        assert!(bands.iter().all(|b| *b > 0.9));
        app.levels_at = Some(
            app.levels_at.unwrap() - StdDuration::from_secs(10),
        );
        let (bands, wave) = app.display_levels();
        assert!(bands.iter().all(|b| *b == 0.0));
        assert!(wave.iter().all(|w| *w == 0.0));
    }
}
