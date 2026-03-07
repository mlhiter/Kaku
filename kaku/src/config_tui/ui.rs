use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

use super::{App, Mode};
use crate::tui_core::theme::{accent, bg, muted, panel, primary, text_fg};

pub(super) fn ui(frame: &mut ratatui::Frame, app: &mut App) {
    let full = frame.area();
    if full.width < 2 || full.height < 2 {
        return;
    }

    let area = Rect::new(full.x, full.y, full.width - 1, full.height - 1);

    frame.render_widget(Clear, area);
    frame.render_widget(Block::default().style(Style::default().bg(bg())), area);

    let chunks = Layout::vertical([
        Constraint::Length(2), // header
        Constraint::Fill(1),   // content
    ])
    .split(area);

    render_header(frame, chunks[0]);
    render_fields(frame, chunks[1], app);

    if app.mode == Mode::Selecting {
        render_selector(frame, area, app);
    } else if app.mode == Mode::Editing {
        render_editor(frame, area, app);
    }
}

fn render_header(frame: &mut ratatui::Frame, area: Rect) {
    let line = Line::from(vec![
        Span::styled(
            "  Kaku",
            Style::default().fg(primary()).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" · ", Style::default().fg(muted())),
        Span::styled("Settings", Style::default().fg(text_fg())),
    ]);
    frame.render_widget(Paragraph::new(vec![line, Line::from("")]), area);
}

fn render_fields(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let area = area.inner(Margin::new(0, 0));
    let mut items: Vec<ListItem> = Vec::new();
    let mut selected_flat: Option<usize> = None;
    let mut flat = 0usize;

    for (idx, field) in app.fields.iter().enumerate() {
        let is_selected = idx == app.selected;
        if is_selected {
            selected_flat = Some(flat);
        }

        let display_value = app.display_value(field);
        let has_options = field.has_options();

        let key_style = if is_selected {
            Style::default().fg(primary()).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(text_fg())
        };

        let value_style = if is_selected {
            Style::default().fg(primary()).add_modifier(Modifier::BOLD)
        } else if field.value.is_empty() {
            Style::default().fg(muted())
        } else {
            Style::default().fg(accent())
        };

        let marker = if is_selected { "› " } else { "  " };
        let suffix = if has_options && field.options.len() > 2 {
            " ▾"
        } else {
            ""
        };

        let line = Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled(
                marker,
                Style::default()
                    .fg(if is_selected { primary() } else { muted() })
                    .add_modifier(if is_selected {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ),
            Span::styled(format!("{:<20}", field.key), key_style),
            Span::styled(format!("{}{}", display_value, suffix), value_style),
        ]);

        items.push(ListItem::new(line));
        flat += 1;
    }

    items.push(ListItem::new(Line::from("")));
    items.push(ListItem::new(Line::from(vec![
        Span::styled("    ", Style::default()),
        Span::styled("ESC", Style::default().fg(primary())),
        Span::styled(" save and apply changes", Style::default().fg(muted())),
        Span::styled("  ·  ", Style::default().fg(muted())),
        Span::styled("E", Style::default().fg(primary())),
        Span::styled(" open full config", Style::default().fg(muted())),
    ])));

    let mut state = ListState::default();
    state.select(selected_flat);

    let list = List::new(items).highlight_style(Style::default());
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_selector(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let Some((field, select_index)) = app.selecting_view() else {
        return;
    };

    let option_count = field.options.len() as u16;
    let max_popup_width = area.width.saturating_sub(4);
    let min_popup_width = 40u16.min(max_popup_width);
    let longest_option_width = field
        .options
        .iter()
        .map(|opt| opt.chars().count() as u16)
        .max()
        .unwrap_or(0);
    let popup_width = std::cmp::max(
        min_popup_width,
        longest_option_width.saturating_add(10).min(max_popup_width),
    );
    let popup_height = (option_count + 2).min(area.height.saturating_sub(4));
    let popup = Rect::new(
        (area.width.saturating_sub(popup_width)) / 2,
        (area.height.saturating_sub(popup_height)) / 2,
        popup_width,
        popup_height,
    );

    frame.render_widget(Clear, popup);

    let block = Block::default()
        .title(Line::from(vec![
            Span::styled(" Select: ", Style::default().fg(primary())),
            Span::styled(field.key, Style::default().fg(text_fg())),
            Span::raw(" "),
        ]))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(primary()))
        .style(Style::default().bg(panel()));

    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let items: Vec<ListItem> = field
        .options
        .iter()
        .enumerate()
        .map(|(i, opt)| {
            let is_sel = i == select_index;
            let marker = if is_sel { "› " } else { "  " };
            let style = if is_sel {
                Style::default().fg(primary()).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(text_fg())
            };
            ListItem::new(Line::from(vec![
                Span::styled(
                    marker,
                    Style::default()
                        .fg(if is_sel { primary() } else { muted() })
                        .add_modifier(if is_sel {
                            Modifier::BOLD
                        } else {
                            Modifier::empty()
                        }),
                ),
                Span::styled(*opt, style),
            ]))
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(select_index));

    let list = List::new(items).highlight_style(Style::default());
    frame.render_stateful_widget(list, inner, &mut state);
}

fn render_editor(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let Some((field, edit_buf, edit_cursor)) = app.editing_view() else {
        return;
    };

    let popup_width = ((area.width as f32 * 0.7) as u16).min(area.width.saturating_sub(4));
    let popup_height = 5u16.min(area.height.saturating_sub(4));
    let popup = Rect::new(
        (area.width.saturating_sub(popup_width)) / 2,
        (area.height.saturating_sub(popup_height)) / 2,
        popup_width,
        popup_height,
    );

    frame.render_widget(Clear, popup);

    let block = Block::default()
        .title(Line::from(vec![
            Span::styled(" Edit: ", Style::default().fg(primary())),
            Span::styled(field.key, Style::default().fg(text_fg())),
            Span::styled("  ", Style::default()),
            Span::styled("Enter", Style::default().fg(primary())),
            Span::styled(": Save  ", Style::default().fg(muted())),
            Span::styled("Esc", Style::default().fg(primary())),
            Span::styled(": Cancel ", Style::default().fg(muted())),
        ]))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(primary()))
        .style(Style::default().bg(panel()));

    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let content_area = inner.inner(Margin::new(1, 0));

    let line = if edit_buf.is_empty() {
        Line::from(Span::styled(" ", Style::default().bg(primary())))
    } else {
        let char_count = edit_buf.chars().count();
        let byte_pos = edit_buf
            .char_indices()
            .nth(edit_cursor)
            .map(|(i, _)| i)
            .unwrap_or(edit_buf.len());
        let before = &edit_buf[..byte_pos];
        let after = &edit_buf[byte_pos..];

        if edit_cursor >= char_count {
            Line::from(vec![
                Span::styled(before, Style::default().fg(text_fg())),
                Span::styled(" ", Style::default().bg(primary())),
            ])
        } else {
            let mut chars = after.chars();
            let current_char = chars.next().unwrap_or(' ');
            let remaining = chars.as_str();

            Line::from(vec![
                Span::styled(before, Style::default().fg(text_fg())),
                Span::styled(
                    current_char.to_string(),
                    Style::default().bg(primary()).fg(bg()),
                ),
                Span::styled(remaining, Style::default().fg(text_fg())),
            ])
        }
    };

    let input = Paragraph::new(vec![line]).wrap(ratatui::widgets::Wrap { trim: false });
    frame.render_widget(input, content_area);
}
