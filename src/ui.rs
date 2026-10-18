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

fn draw_board(frame: &mut Frame, app: &mut App, area: Rect) {
    let title = match &app.board {
        Some(b) => format!(" {}/{} ", b.owner, b.title),
        None => format!(" {} ", app.repository),
    };
    let mut lines: Vec<Line> = vec![];
    // (line index, issue number) of clickable rows
    let mut links: Vec<(usize, u64)> = vec![];
    if let Some(e) = &app.board_error {
        lines.push(Line::from(e.clone()).red());
    }
    match &app.board {
        Some(board) => {
            for (i, column) in board.columns.iter().enumerate() {
                if i > 0 {
                    lines.push(Line::default());
                }
                lines.push(Line::from(format!("[{}]", column.name)).bold().yellow());
                if column.issues.is_empty() {
                    lines.push(Line::from("No items").dark_gray());
                }
                for issue in &column.issues {
                    let mut spans = vec![
                        Span::from(format!("#{} ", issue.number)).fg(Color::Cyan),
                        Span::from(issue.title.clone()),
                    ];
                    if app.active_issues.contains_key(&issue.number) {
                        spans.push(Span::from(" 🟢"));
                    }
                    links.push((lines.len(), issue.number));
                    lines.push(Line::from(spans));
                }
            }
        }
        None if app.board_error.is_none() => lines.push(Line::from("loading...").dark_gray()),
        None => {}
    }

    let block = pane(title, app.focus == Focus::Board);
    let inner = block.inner(area);
    app.board_area = area;
    app.issue_links = links
        .into_iter()
        .filter_map(|(index, number)| {
            let row = (index as u16).checked_sub(app.board_scroll)?;
            (row < inner.height).then(|| {
                let width = (lines[index].width() as u16).min(inner.width);
                (Rect::new(inner.x, inner.y + row, width, 1), number)
            })
        })
        .collect();

    let paragraph = Paragraph::new(lines)
        .block(block)
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

    app.log_area = area;
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
        Span::from("q:quit r:refresh Tab:focus j/k:scroll G:follow click:open issue").dark_gray(),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::{Board, Column, Issue};
    use crate::config::Config;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[tokio::test]
    async fn draw_board_sections_and_links() {
        let config = Config::parse(r#"{ "listen": "ws://x" }"#).unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new("owner/repo".into(), config, tx);
        app.board = Some(Board {
            owner: "owner".into(),
            title: "proj".into(),
            columns: vec![
                Column {
                    name: "Todo".into(),
                    issues: vec![],
                },
                Column {
                    name: "In Progress".into(),
                    issues: vec![Issue {
                        number: 3,
                        title: "hello".into(),
                        updated_at: String::new(),
                    }],
                },
            ],
        });

        let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();

        let buffer = terminal.backend().buffer();
        let row = |y: u16| {
            (1..23)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        };
        let rows: Vec<_> = (1..6).map(row).collect();
        assert_eq!(
            rows,
            vec!["[Todo]", "No items", "", "[In Progress]", "#3 hello"]
        );
        assert_eq!(app.issue_links, vec![(Rect::new(1, 5, 8, 1), 3)]);

        // links follow scrolling
        app.board_scroll = 2;
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        assert_eq!(app.issue_links, vec![(Rect::new(1, 3, 8, 1), 3)]);
    }
}
