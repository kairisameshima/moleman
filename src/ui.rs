use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap,
};
use ratatui::Frame;

use crate::app::{App, Focus};
use crate::aws::sso::TokenStatus;
use crate::config;
use crate::tunnel::{Group, Status};

const ACTIVE: Color = Color::Cyan;
const DIM: Color = Color::DarkGray;

pub fn render(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title
            Constraint::Min(0),    // body
            Constraint::Length(1), // toast
            Constraint::Length(1), // keys
        ])
        .split(frame.area());

    render_title(frame, app, chunks[0]);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(34), Constraint::Min(0)])
        .split(chunks[1]);

    render_profiles(frame, app, body[0]);

    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(58), Constraint::Min(0)])
        .split(body[1]);

    render_tunnels(frame, app, right[0]);
    render_detail(frame, app, right[1]);

    render_toast(frame, app, chunks[2]);
    render_keys(frame, chunks[3]);

    if app.picker.is_some() {
        render_picker(frame, app, chunks[1]);
    }
}

fn render_title(frame: &mut Frame, app: &App, area: Rect) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(40)])
        .split(area);

    let status = app.status_for_profile(app.active_profile);
    let (sym, color, text) = badge(status);
    let session = app.active_session_name().unwrap_or_else(|| "—".into());
    let role = app
        .aws
        .profiles
        .get(app.active_profile)
        .and_then(|p| p.role.clone())
        .unwrap_or_default();

    let mut spans = vec![
        Span::styled(
            " moleman ",
            Style::default()
                .fg(Color::Black)
                .bg(ACTIVE)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  active: "),
        Span::styled(
            app.active_profile_name().to_string(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    if !role.is_empty() {
        spans.push(Span::styled(format!(" · {role}"), Style::default().fg(DIM)));
    }
    spans.push(Span::raw(format!("  [{session} ")));
    spans.push(Span::styled(
        format!("{sym} {text}"),
        Style::default().fg(color),
    ));
    spans.push(Span::raw("]"));

    frame.render_widget(Paragraph::new(Line::from(spans)), cols[0]);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("cfg: {} ", config::display_path(&app.config_path)),
            Style::default().fg(DIM),
        )))
        .alignment(Alignment::Right),
        cols[1],
    );
}

fn render_profiles(frame: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Profiles;
    let items: Vec<ListItem> = app
        .aws
        .profiles
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let (sym, color, text) = badge(app.status_for_profile(i));
            let marker = if i == app.active_profile {
                "▶ "
            } else {
                "  "
            };
            let line = Line::from(vec![
                Span::styled(marker, Style::default().fg(ACTIVE)),
                Span::raw(format!("{:<24}", truncate(&p.name, 24))),
                Span::styled(format!("{sym} {text}"), Style::default().fg(color)),
            ]);
            ListItem::new(line)
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(
        app.profile_sel
            .min(app.aws.profiles.len().saturating_sub(1)),
    ));

    let list = List::new(items)
        .block(panel_block(
            "Profiles  (Enter = make active, l = login)",
            focused,
        ))
        .highlight_style(highlight(focused))
        .highlight_symbol(if focused { "│" } else { " " });

    frame.render_stateful_widget(list, area, &mut state);
}

fn render_tunnels(frame: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Tunnels;
    let ordered = app.ordered();
    let selected_tunnel = app.selected_tunnel();

    let mut items: Vec<ListItem> = Vec::new();
    let mut selected_row: Option<usize> = None;

    for group in Group::ALL {
        let in_group: Vec<usize> = app
            .tunnels
            .iter()
            .enumerate()
            .filter(|(_, t)| t.group == group)
            .map(|(i, _)| i)
            .collect();

        items.push(ListItem::new(Line::from(Span::styled(
            format!("── {} ──", group.title()),
            Style::default().fg(DIM).add_modifier(Modifier::BOLD),
        ))));

        if in_group.is_empty() {
            items.push(ListItem::new(Line::from(Span::styled(
                "   (none — press d to add a database)".to_string(),
                Style::default().fg(DIM),
            ))));
            continue;
        }

        for ti in in_group {
            if Some(ti) == selected_tunnel {
                selected_row = Some(items.len());
            }
            items.push(tunnel_item(&app.tunnels[ti]));
        }
    }

    let _ = ordered; // ordering already reflected by group iteration

    let mut state = ListState::default();
    state.select(selected_row);

    let list = List::new(items)
        .block(panel_block(
            "Tunnels  (s start · x stop · S/X group · d RDS)",
            focused,
        ))
        .highlight_style(highlight(focused))
        .highlight_symbol(if focused { "│ " } else { "  " });

    frame.render_stateful_widget(list, area, &mut state);
}

fn tunnel_item(t: &crate::tunnel::Tunnel) -> ListItem<'static> {
    let (sym, color) = status_glyph(&t.status);
    let profile = t.profile.as_deref().unwrap_or("ssh");
    let line = Line::from(vec![
        Span::styled(format!("{sym} "), Style::default().fg(color)),
        Span::raw(format!("{:<22}", truncate(&t.name, 22))),
        Span::styled(
            format!(":{:<6}", t.local_port),
            Style::default().fg(Color::White),
        ),
        Span::styled(
            format!("{:<9}", t.status.label()),
            Style::default().fg(color),
        ),
        Span::styled(profile.to_string(), Style::default().fg(DIM)),
    ]);
    ListItem::new(line)
}

fn render_detail(frame: &mut Frame, app: &App, area: Rect) {
    let block = panel_block("Detail", false);
    let Some(i) = app.selected_tunnel() else {
        frame.render_widget(Paragraph::new("No tunnel selected").block(block), area);
        return;
    };
    let t = &app.tunnels[i];
    let (sym, color) = status_glyph(&t.status);

    let mut lines = vec![
        Line::from(vec![
            Span::styled(format!("{sym} "), Style::default().fg(color)),
            Span::styled(
                t.name.clone(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(
                "   {}  →  localhost:{}",
                t.group.title(),
                t.local_port
            )),
        ]),
        Line::from(Span::styled(t.command_preview(), Style::default().fg(DIM))),
    ];

    if let Status::Failed(msg) = &t.status {
        lines.push(Line::from(Span::styled(
            format!("error: {msg}"),
            Style::default().fg(Color::Red),
        )));
    }
    lines.push(Line::from(Span::styled(
        "── log ──",
        Style::default().fg(DIM),
    )));

    if let Ok(log) = t.log.lock() {
        let tail = log.iter().rev().take(8).rev();
        for entry in tail {
            lines.push(Line::from(Span::raw(entry.clone())));
        }
    }

    frame.render_widget(
        Paragraph::new(lines).block(block).wrap(Wrap { trim: true }),
        area,
    );
}

fn render_toast(frame: &mut Frame, app: &App, area: Rect) {
    if let Some(toast) = &app.toast {
        let color = if toast.error {
            Color::Red
        } else {
            Color::Green
        };
        let line = Line::from(Span::styled(
            format!(" {} ", toast.message),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
        frame.render_widget(Paragraph::new(line), area);
    }
}

fn render_keys(frame: &mut Frame, area: Rect) {
    let keys = "Tab focus · ↑↓ move · Enter select/start · s start · x stop · S/X group · d RDS · l login · r refresh · q quit";
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(keys, Style::default().fg(DIM))))
            .alignment(Alignment::Left),
        area,
    );
}

fn render_picker(frame: &mut Frame, app: &App, area: Rect) {
    let Some(picker) = &app.picker else {
        return;
    };
    let rect = centered_rect(70, 60, area);
    frame.render_widget(Clear, rect);

    let items: Vec<ListItem> = picker
        .items
        .iter()
        .map(|inst| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{:<28}", truncate(&inst.identifier, 28)),
                    Style::default().fg(Color::White),
                ),
                Span::raw(format!("{}:{}  ", truncate(&inst.endpoint, 48), inst.port)),
                Span::styled(inst.engine.clone(), Style::default().fg(DIM)),
            ]))
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(picker.selected));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ACTIVE))
        .title(format!(
            " Select RDS — {}  (↑↓ move · Enter tunnel · Esc cancel) ",
            picker.profile
        ));

    let list = List::new(items)
        .block(block)
        .highlight_style(highlight(true))
        .highlight_symbol("│ ");

    frame.render_stateful_widget(list, rect, &mut state);
}

// ---- helpers ---------------------------------------------------------------

fn panel_block(title: &str, focused: bool) -> Block<'static> {
    let color = if focused { ACTIVE } else { DIM };
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(color))
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(if focused { Color::White } else { DIM }),
        ))
}

fn highlight(focused: bool) -> Style {
    if focused {
        Style::default()
            .bg(Color::Rgb(40, 50, 60))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::DIM)
    }
}

fn status_glyph(status: &Status) -> (&'static str, Color) {
    match status {
        Status::Stopped => ("○", DIM),
        Status::Starting => ("◍", Color::Yellow),
        Status::Running => ("●", Color::Green),
        Status::Failed(_) => ("✗", Color::Red),
        Status::External => ("▣", Color::Blue),
    }
}

fn badge(status: TokenStatus) -> (&'static str, Color, String) {
    match status {
        TokenStatus::Valid(s) => ("●", Color::Green, fmt_dur(s)),
        TokenStatus::Expiring(s) => ("▲", Color::Yellow, fmt_dur(s)),
        TokenStatus::Expired => ("✕", Color::Red, "expired".to_string()),
        TokenStatus::NoToken => ("○", DIM, "login".to_string()),
    }
}

fn fmt_dur(secs: i64) -> String {
    if secs >= 3600 {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}
