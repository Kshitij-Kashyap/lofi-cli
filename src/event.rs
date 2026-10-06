use crossterm::event::{KeyCode, KeyEvent};

use crate::app::PlayerCommand;

/// Map a crossterm key event to an application command.
/// Returns None for unhandled keys. The TUI never controls audio directly.
pub fn key_to_command(key: KeyEvent, selected: usize, stations_len: usize) -> Option<PlayerCommand> {
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => {
            if stations_len == 0 {
                return None;
            }
            let prev = if selected == 0 {
                stations_len - 1
            } else {
                selected - 1
            };
            Some(PlayerCommand::SelectStation(prev))
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if stations_len == 0 {
                return None;
            }
            Some(PlayerCommand::SelectStation(
                (selected + 1) % stations_len,
            ))
        }
        KeyCode::Enter => Some(PlayerCommand::Play),
        KeyCode::Char(' ') => Some(PlayerCommand::TogglePause),
        KeyCode::Left => Some(PlayerCommand::VolumeDown),
        KeyCode::Right => Some(PlayerCommand::VolumeUp),
        KeyCode::Char('m') | KeyCode::Char('M') => Some(PlayerCommand::ToggleMute),
        KeyCode::Char('r') | KeyCode::Char('R') => Some(PlayerCommand::Reconnect),
        KeyCode::Char('v') | KeyCode::Char('V') => Some(PlayerCommand::CycleVis),
        KeyCode::Char('c') | KeyCode::Char('C') => Some(PlayerCommand::CyclePalette),
        KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => Some(PlayerCommand::Quit),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        }
    }

    #[test]
    fn maps_all_spec_keys() {
        assert_eq!(
            key_to_command(key(KeyCode::Enter), 0, 3),
            Some(PlayerCommand::Play)
        );
        assert_eq!(
            key_to_command(key(KeyCode::Char(' ')), 0, 3),
            Some(PlayerCommand::TogglePause)
        );
        assert_eq!(
            key_to_command(key(KeyCode::Left), 0, 3),
            Some(PlayerCommand::VolumeDown)
        );
        assert_eq!(
            key_to_command(key(KeyCode::Right), 0, 3),
            Some(PlayerCommand::VolumeUp)
        );
        assert_eq!(
            key_to_command(key(KeyCode::Char('m')), 0, 3),
            Some(PlayerCommand::ToggleMute)
        );
        assert_eq!(
            key_to_command(key(KeyCode::Char('r')), 0, 3),
            Some(PlayerCommand::Reconnect)
        );
        assert_eq!(
            key_to_command(key(KeyCode::Char('q')), 0, 3),
            Some(PlayerCommand::Quit)
        );
        assert_eq!(
            key_to_command(key(KeyCode::Char('v')), 0, 3),
            Some(PlayerCommand::CycleVis)
        );
        assert_eq!(
            key_to_command(key(KeyCode::Char('c')), 0, 3),
            Some(PlayerCommand::CyclePalette)
        );
    }

    #[test]
    fn up_down_move_selection() {
        assert_eq!(
            key_to_command(key(KeyCode::Down), 0, 3),
            Some(PlayerCommand::SelectStation(1))
        );
        assert_eq!(
            key_to_command(key(KeyCode::Up), 0, 3),
            Some(PlayerCommand::SelectStation(2))
        );
    }
}
