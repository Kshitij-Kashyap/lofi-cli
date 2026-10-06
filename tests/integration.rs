use lofi::app::{fmt_elapsed, App, ConnectionState, Palette, PlayerCommand, PlayerState, VisMode};
use lofi::config::Config;
use lofi::station::{builtin_stations, find_station};
use lofi::stream::{backoff_for_attempt, parse_icy_stream_title, IcyDemuxer};
use std::time::Duration;

#[test]
fn station_lookup_case_insensitive() {
    let stations = builtin_stations();
    assert!(find_station(&stations, "groove salad").is_some());
    assert!(find_station(&stations, "DRONE ZONE").is_some());
    assert!(find_station(&stations, "missing").is_none());
}

#[test]
fn config_parsing_merges_custom_stations() {
    let toml = "volume = 0.5\n[[stations]]\nname = \"X\"\nurl = \"https://example.com/s\"\n";
    let cfg = Config::from_toml(toml, None).unwrap();
    assert_eq!(cfg.stations.len(), builtin_stations().len() + 1);
}

#[test]
fn app_volume_boundaries() {
    let mut app = App::new(&Config::default());
    app.volume = 0.0;
    app.apply(PlayerCommand::VolumeDown);
    assert_eq!(app.volume, 0.0);
    app.volume = 1.0;
    app.apply(PlayerCommand::VolumeUp);
    assert_eq!(app.volume, 1.0);
}

#[test]
fn app_mute_roundtrip() {
    let mut app = App::new(&Config::default());
    app.volume = 0.7;
    app.apply(PlayerCommand::ToggleMute);
    assert!(app.muted && app.effective_volume() == 0.0);
    app.apply(PlayerCommand::ToggleMute);
    assert!(!app.muted);
}

#[test]
fn app_state_transitions() {
    let mut app = App::new(&Config::default());
    assert_eq!(app.player_state, PlayerState::Stopped);
    app.apply(PlayerCommand::Play);
    assert_eq!(app.player_state, PlayerState::Connecting);
    app.on_connected();
    app.on_playing();
    assert_eq!(app.player_state, PlayerState::Playing);
    app.apply(PlayerCommand::TogglePause);
    assert_eq!(app.player_state, PlayerState::Paused);
    app.apply(PlayerCommand::Reconnect);
    assert_eq!(app.connection, ConnectionState::Reconnecting);
    app.on_failed("boom");
    assert_eq!(app.connection, ConnectionState::Failed);
}

#[test]
fn reconnect_backoff_schedule() {
    let secs: Vec<u64> = (1..=6).map(|a| backoff_for_attempt(a).as_secs()).collect();
    assert_eq!(secs, vec![2, 5, 10, 20, 30, 30]);
    assert_eq!(backoff_for_attempt(1), Duration::from_secs(2));
}

#[test]
fn vis_mode_and_palette_commands() {
    let mut app = App::new(&Config::default());
    app.apply(PlayerCommand::CycleVis);
    assert_eq!(app.vis, VisMode::Wave);
    app.apply(PlayerCommand::CyclePalette);
    assert_eq!(app.palette, Palette::Ember);
}

#[test]
fn session_timer_formats() {
    assert_eq!(fmt_elapsed(Duration::from_secs(7)), "00:07");
    assert_eq!(fmt_elapsed(Duration::from_secs(3600)), "1:00:00");
}

#[test]
fn icy_metadata_parsing() {
    assert_eq!(
        parse_icy_stream_title(b"StreamTitle='A - B';").as_deref(),
        Some("A - B")
    );
    assert_eq!(parse_icy_stream_title(b"StreamTitle='';"), None);
    let mut d = IcyDemuxer::new(0);
    let (audio, titles) = d.push(b"raw");
    assert_eq!(audio, b"raw");
    assert!(titles.is_empty());
}
