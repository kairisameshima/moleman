use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::app::{App, Focus};

/// Translate a key press into an action on the app. Async because starting
/// tunnels and discovery await AWS calls.
pub async fn handle_key(app: &mut App, key: KeyEvent) {
    if key.kind != KeyEventKind::Press {
        return;
    }

    // Global quit.
    if matches!(key.code, KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL)) {
        app.should_quit = true;
        return;
    }

    // The RDS picker overlay captures input while open.
    if app.picker.is_some() {
        handle_picker_key(app, key).await;
        return;
    }

    match key.code {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Tab | KeyCode::BackTab => app.toggle_focus(),
        KeyCode::Up | KeyCode::Char('k') => app.move_up(),
        KeyCode::Down | KeyCode::Char('j') => app.move_down(),
        KeyCode::Enter => match app.focus {
            Focus::Profiles => app.set_active_to_selected(),
            Focus::Tunnels => app.start_selected().await,
        },
        KeyCode::Char('s') => app.start_selected().await,
        KeyCode::Char('x') => app.stop_selected(),
        KeyCode::Char('S') => {
            if let Some(g) = app.selected_group() {
                app.start_group(g).await;
            }
        }
        KeyCode::Char('X') => {
            if let Some(g) = app.selected_group() {
                app.stop_group(g);
            }
        }
        KeyCode::Char('d') => app.open_rds_picker().await,
        KeyCode::Char('l') => app.trigger_login(),
        KeyCode::Char('r') => app.refresh().await,
        _ => {}
    }
}

async fn handle_picker_key(app: &mut App, key: KeyEvent) {
    let Some(picker) = app.picker.as_mut() else {
        return;
    };
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => app.picker = None,
        KeyCode::Up | KeyCode::Char('k') => {
            if picker.selected > 0 {
                picker.selected -= 1;
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if picker.selected + 1 < picker.items.len() {
                picker.selected += 1;
            }
        }
        KeyCode::Enter => app.confirm_rds_pick().await,
        _ => {}
    }
}
