use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::app::{App, Focus, LogKind};

pub fn draw(frame: &mut Frame, app: &mut App) {
    let [main, status] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(frame.area());
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(main);

    draw_board(frame, app, left);
    draw_log(frame, app, right);
    draw_status(frame, app, status);
}

fn pane(title: String, focused: bool) -> Block<'static> {
    let style = if focused {
        Style::new().fg(Color::Cyan)
    } else {
        Style::new().fg(Color::DarkGray)
    };
    Block::bordered().title(title).border_style(style)
}

fn draw_board(frame: &mut Frame, app: &App, area: Rect) {
    let title = match &app.board {
        Some(b) => format!(" {}/{} ", b.owner, b.title),
        None => format!(" {} ", app.repository),
    };
    let mut lines: Vec<Line> = vec![];
    if let Some(e) = &app.board_error {
        lines.push(Line::from(e.clone()).red());
    }
    match &app.board {
        Some(board) => {
            for column in &board.columns {
                lines.push(Line::from(format!("[{}]", column.name)).bold().yellow());
                for issue in &column.issues {
                    let mut spans = vec![
                        Span::from(format!("#{} ", issue.number)).fg(Color::Cyan),
                        Span::from(issue.title.clone()),
                    ];
                    if app.active_issues.contains(&issue.number) {
                        spans.push(Span::from(" 🟢"));
                    }
                    lines.push(Line::from(spans));
                }
            }
        }
        None if app.board_error.is_none() => lines.push(Line::from("loading...").dark_gray()),
        None => {}
    }

    let paragraph = Paragraph::new(lines)
        .block(pane(title, app.focus == Focus::Board))
        .scroll((app.board_scroll, 0));
    frame.render_widget(paragraph, area);
}

fn draw_log(frame: &mut Frame, app: &mut App, area: Rect) {
    let lines: Vec<Line> = app
        .log
        .iter()
        .map(|l| {
            let style = match l.kind {
                LogKind::Event => Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                LogKind::Skipped => Style::new().fg(Color::DarkGray),
                LogKind::Command => Style::new().fg(Color::Green),
                LogKind::Stdout => Style::new(),
                LogKind::Stderr => Style::new().fg(Color::Red),
                LogKind::Exit => Style::new().fg(Color::DarkGray),
            };
            Line::styled(l.text.clone(), style)
        })
        .collect();

    let height = area.height.saturating_sub(2);
    let bottom = (lines.len() as u16).saturating_sub(height);
    app.log_bottom_hint = bottom;
    let scroll = app.log_scroll.map_or(bottom, |s| s.min(bottom));

    let paragraph = Paragraph::new(lines)
        .block(pane(" Events ".into(), app.focus == Focus::Log))
        .scroll((scroll, 0));
    frame.render_widget(paragraph, area);
}

fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    let loading = if app.loading { " (refreshing)" } else { "" };
    let line = Line::from(vec![
        Span::from(format!(" {}{loading} ", app.status)).dark_gray(),
        Span::from("q:quit r:refresh Tab:focus j/k:scroll G:follow").dark_gray(),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}
