use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::app::{App, Focus, IssueKey, LogKind, issue_key};

pub fn draw(frame: &mut Frame, app: &mut App) {
    let [main, status] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(frame.area());
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(main);
    // Fit the projects pane to the number of projects, up to a third of the left side
    let count = app.board.as_ref().map_or(1, |b| b.projects.len().max(1)) as u16;
    let projects_height = (count + 2).min((left.height / 3).max(3));
    let [top, bottom] =
        Layout::vertical([Constraint::Length(projects_height), Constraint::Min(1)]).areas(left);

    draw_projects(frame, app, top);
    draw_issues(frame, app, bottom);
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

fn draw_projects(frame: &mut Frame, app: &mut App, area: Rect) {
    let mut lines: Vec<Line> = vec![];
    match &app.board {
        Some(board) => {
            for (i, project) in board.projects.iter().enumerate() {
                let active = project.columns.iter().flat_map(|c| &c.issues).any(|issue| {
                    app.active_issues
                        .contains_key(&issue_key(&issue.repository, issue.number))
                });
                let mut spans = vec![Span::from(format!("[{}/{}]", project.owner, project.title))];
                if active {
                    spans.push(Span::from(" 🟢"));
                }
                let line = Line::from(spans);
                lines.push(if i == app.selected_project {
                    line.bold().black().on_cyan()
                } else {
                    line.cyan()
                });
            }
            if board.projects.is_empty() {
                lines.push(Line::from("No projects").dark_gray());
            }
        }
        None => lines.push(Line::from("loading...").dark_gray()),
    }

    // Keep the selected project visible
    let height = area.height.saturating_sub(2).max(1);
    let selected = app.selected_project as u16;
    let scroll = app
        .projects_scroll
        .min(selected)
        .max((selected + 1).saturating_sub(height));
    app.projects_scroll = scroll;
    app.projects_area = area;

    let paragraph = Paragraph::new(lines)
        .block(pane(" Projects ".into(), app.focus == Focus::Projects))
        .scroll((scroll, 0));
    frame.render_widget(paragraph, area);
}

fn draw_issues(frame: &mut Frame, app: &mut App, area: Rect) {
    let mut lines: Vec<Line> = vec![];
    // (line index, issue) of clickable rows
    let mut links: Vec<(usize, IssueKey)> = vec![];
    if let Some(e) = &app.board_error {
        lines.push(Line::from(e.clone()).red());
    }
    let project = app
        .board
        .as_ref()
        .and_then(|b| b.projects.get(app.selected_project));
    if let Some(project) = project {
        for column in &project.columns {
            lines.push(Line::from(format!("{}>", column.name)).bold().yellow());
            if column.issues.is_empty() {
                lines.push(Line::from(" No items").dark_gray());
            }
            for issue in &column.issues {
                let key = issue_key(&issue.repository, issue.number);
                let mut spans = vec![
                    Span::from(format!(" #{} ", issue.number)).fg(Color::Cyan),
                    Span::from(issue.title.clone()),
                ];
                if app.active_issues.contains_key(&key) {
                    spans.push(Span::from(" 🟢"));
                }
                links.push((lines.len(), key));
                lines.push(Line::from(spans));
            }
        }
    }

    let title = match project {
        Some(p) => format!(" Issues [{}/{}] ", p.owner, p.title),
        None => " Issues ".into(),
    };
    let block = pane(title, app.focus == Focus::Issues);
    let inner = block.inner(area);
    app.issues_area = area;
    app.issue_links = links
        .into_iter()
        .filter_map(|(index, key)| {
            let row = (index as u16).checked_sub(app.issues_scroll)?;
            (row < inner.height).then(|| {
                let width = (lines[index].width() as u16).min(inner.width);
                (Rect::new(inner.x, inner.y + row, width, 1), key)
            })
        })
        .collect();

    let paragraph = Paragraph::new(lines)
        .block(block)
        .scroll((app.issues_scroll, 0));
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
        .block(pane(" Progress ".into(), app.focus == Focus::Progress))
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
    use crate::app::AppEvent;
    use crate::board::{Board, Column, Issue, Project};
    use crate::config::Config;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };

    fn project(title: &str, issues: Vec<(&str, u64, &str)>) -> Project {
        Project {
            id: String::new(),
            owner: "owner".into(),
            title: title.into(),
            columns: vec![
                Column {
                    name: "Todo".into(),
                    issues: vec![],
                },
                Column {
                    name: "In Progress".into(),
                    issues: issues
                        .into_iter()
                        .map(|(repository, number, title)| Issue {
                            repository: repository.into(),
                            number,
                            title: title.into(),
                            updated_at: String::new(),
                        })
                        .collect(),
                },
            ],
        }
    }

    #[tokio::test]
    async fn draw_projects_and_links() {
        let config = Config::parse(r#"{ "listen": "ws://x" }"#).unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(config, tx);
        app.board = Some(Board {
            projects: vec![
                project("p1", vec![("owner/a", 3, "hello")]),
                project("p2", vec![("owner/b", 3, "world")]),
            ],
        });
        app.active_issues.insert(issue_key("owner/b", 3), 1);

        let mut terminal = Terminal::new(TestBackend::new(60, 14)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();

        let row = |terminal: &Terminal<TestBackend>, y: u16| {
            let buffer = terminal.backend().buffer();
            (1..23)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        };
        // projects pane (top left) fits the number of projects
        assert_eq!(row(&terminal, 1), "[owner/p1]");
        assert!(row(&terminal, 2).starts_with("[owner/p2] 🟢"));
        assert_eq!(app.projects_area, Rect::new(0, 0, 24, 4));
        assert_eq!(app.issues_area, Rect::new(0, 4, 24, 9));
        assert_eq!(app.log_area, Rect::new(24, 0, 36, 13));

        // issues pane (bottom left) shows the selected project only
        let rows: Vec<_> = (5..9).map(|y| row(&terminal, y)).collect();
        assert_eq!(
            rows,
            vec!["Todo>", " No items", "In Progress>", " #3 hello"]
        );
        assert_eq!(row(&terminal, 9), "");
        assert_eq!(
            app.issue_links,
            vec![(Rect::new(1, 8, 9, 1), issue_key("owner/a", 3))]
        );

        // links follow scrolling
        app.issues_scroll = 2;
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        assert_eq!(
            app.issue_links,
            vec![(Rect::new(1, 6, 9, 1), issue_key("owner/a", 3))]
        );

        // clicking a project selects it and resets the scroll
        app.handle(AppEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 3,
            row: 2,
            modifiers: KeyModifiers::NONE,
        }));
        assert_eq!(app.selected_project, 1);
        assert_eq!(app.issues_scroll, 0);
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        assert!(row(&terminal, 8).starts_with(" #3 world 🟢"));
        assert_eq!(
            app.issue_links,
            vec![(Rect::new(1, 8, 12, 1), issue_key("owner/b", 3))]
        );

        // selection stays within the projects
        app.handle(AppEvent::Key(KeyEvent::from(KeyCode::Char('j'))));
        assert_eq!(app.selected_project, 1);
        app.handle(AppEvent::Key(KeyEvent::from(KeyCode::Char('k'))));
        app.handle(AppEvent::Key(KeyEvent::from(KeyCode::Char('k'))));
        assert_eq!(app.selected_project, 0);
    }

    #[tokio::test]
    async fn tab_cycles_three_panes() {
        let config = Config::parse(r#"{ "listen": "ws://x" }"#).unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(config, tx);
        let mut focuses = vec![app.focus];
        for _ in 0..3 {
            app.handle(AppEvent::Key(KeyEvent::from(KeyCode::Tab)));
            focuses.push(app.focus);
        }
        assert_eq!(
            focuses,
            vec![
                Focus::Projects,
                Focus::Issues,
                Focus::Progress,
                Focus::Projects
            ]
        );
    }
}
