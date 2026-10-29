use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Tabs, Wrap};

use crate::block::Blocker;
use crate::layout::StyleKind;
use crate::tabs::{App, Overlay, help_lines};

pub fn draw(frame: &mut Frame, app: &App, blocker: &Blocker) {
    let area = frame.area();
    let [tabs, url, page, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(area);

    let titles: Vec<Line> = (0..app.tab_count())
        .map(|index| Line::from(app.tab_label(index)))
        .collect();
    frame.render_widget(
        Tabs::new(titles)
            .select(app.current())
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
        tabs,
    );
    frame.render_widget(Paragraph::new(app.url_line()), url);

    if let Some(overlay) = app.overlay {
        let mut lines = match overlay {
            Overlay::Help => help_lines()
                .iter()
                .map(|line| Line::from(*line))
                .collect::<Vec<_>>(),
            Overlay::Lists => blocker
                .lists()
                .iter()
                .map(|list| {
                    let state = list
                        .error
                        .clone()
                        .or(list.updated.clone())
                        .unwrap_or_else(|| "not downloaded".into());
                    Line::from(format!("{}  {state}", list.title))
                })
                .collect(),
        };
        let rows = page.height as usize;
        let max = lines.len().saturating_sub(rows.max(1));
        let start = (app.overlay_scroll as usize).min(max);
        lines.drain(..start);
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), page);
    } else {
        let mut lines = Vec::new();
        for line in app.viewport_lines() {
            let mut spans = Vec::new();
            if line.cursor {
                spans.push(Span::styled(
                    "> ",
                    Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
                ));
            }
            for piece in line.pieces {
                if let Some(hint) = piece.hint {
                    spans.push(Span::styled(
                        format!("{hint} "),
                        Style::default().add_modifier(Modifier::BOLD),
                    ));
                }
                let mut style = match piece.style {
                    StyleKind::Heading => Style::default().add_modifier(Modifier::BOLD),
                    StyleKind::Link => Style::default().add_modifier(Modifier::UNDERLINED),
                    StyleKind::Code => Style::default().add_modifier(Modifier::DIM),
                    StyleKind::Text => Style::default(),
                };
                if line.cursor || line.current_match {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                spans.push(Span::styled(piece.text, style));
            }
            if spans.is_empty() {
                spans.push(Span::raw(" "));
            }
            lines.push(Line::from(spans));
        }
        frame.render_widget(Paragraph::new(lines), page);
    }

    frame.render_widget(
        Paragraph::new(app.status_line(blocker.is_updating())),
        status,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn blank_ui_draws_the_chrome() {
        let blocker = Blocker::from_rules("");
        let app = App::new(80, std::env::temp_dir().join("holonet-draw-absent"));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &app, &blocker)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("1 new"), "{text}");
        assert!(text.contains("o open"), "{text}");
    }

    #[test]
    fn focused_link_draws_a_cursor() {
        let blocker = Blocker::from_rules("");
        let mut app = App::new(80, std::env::temp_dir().join("holonet-draw-absent"));
        app.finish(
            0,
            0,
            Ok(crate::load::Fetched {
                final_url: "https://example.com/".into(),
                html: "<p><a href=\"/next\">next</a></p>".into(),
            }),
            &blocker,
        );
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &app, &blocker)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("> "), "{text}");
        assert!(text.contains("next"), "{text}");
        assert!(app.status_line(false).contains("T new tab"));
    }
}
