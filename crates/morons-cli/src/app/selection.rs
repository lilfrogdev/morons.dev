use ratatui::{buffer::Buffer, layout::Position, style::Color, text::Span};
use ratatui_crossterm::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

#[derive(Default)]
pub(super) struct Selection {
    screen: Option<Buffer>,
    anchor: Option<Position>,
    end: Option<Position>,
    dragged: bool,
}

pub(super) enum Gesture {
    Consumed,
    Click,
    Copy(String),
    TooLarge,
    Other,
}

impl Selection {
    pub(super) fn clear(&mut self) {
        self.anchor = None;
        self.end = None;
        self.dragged = false;
    }

    pub(super) fn render(&mut self, buffer: &mut Buffer) {
        if self.screen.as_ref().is_some_and(|old| {
            old.area != buffer.area
                || old
                    .content
                    .iter()
                    .zip(&buffer.content)
                    .any(|(a, b)| a.symbol() != b.symbol())
        }) {
            self.clear();
        }
        self.screen = Some(buffer.clone());
        if let Some((start, end)) = self.range() {
            for y in start.y..=end.y {
                for x in buffer.area.x..buffer.area.right() {
                    let p = Position::new(x, y);
                    if ordered(p) >= ordered(start) && ordered(p) <= ordered(end) {
                        buffer[p].set_bg(Color::Blue).set_fg(Color::White);
                    }
                }
            }
        }
    }

    fn range(&self) -> Option<(Position, Position)> {
        let a = self.anchor?;
        let b = self.end?;
        if a == b && !self.dragged {
            return None;
        }
        Some(if ordered(a) <= ordered(b) {
            (a, b)
        } else {
            (b, a)
        })
    }

    pub(super) fn mouse(&mut self, mouse: MouseEvent) -> Gesture {
        let point = Position::new(mouse.column, mouse.row);
        let inside = self.screen.as_ref().is_some_and(|s| s.area.contains(point));
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.clear();
                if inside {
                    self.anchor = Some(point);
                    self.end = Some(point);
                }
                Gesture::Consumed
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.anchor.is_some() {
                    self.dragged = true;
                    if inside {
                        self.end = Some(point);
                    }
                }
                Gesture::Consumed
            }
            MouseEventKind::Up(MouseButton::Left) => {
                let Some(anchor) = self.anchor else {
                    return Gesture::Consumed;
                };
                if inside {
                    self.end = Some(point);
                }
                if !self.dragged && point == anchor && inside {
                    self.clear();
                    return Gesture::Click;
                }
                let has_range = self.range().is_some();
                let text = self.text();
                self.clear();
                match text {
                    None if has_range => Gesture::TooLarge,
                    Some(text) if !text.trim().is_empty() => Gesture::Copy(text),
                    _ => Gesture::Consumed,
                }
            }
            MouseEventKind::Moved => Gesture::Other,
            _ => {
                self.clear();
                Gesture::Other
            }
        }
    }

    fn text(&self) -> Option<String> {
        let screen = self.screen.as_ref()?;
        let (start, end) = self.range()?;
        let mut text = String::new();
        for y in start.y..=end.y {
            let mut row = String::new();
            let mut x = screen.area.x;
            while x < screen.area.right() {
                let symbol = screen[(x, y)].symbol();
                let width = Span::raw(symbol).width().max(1) as u16;
                if ordered(Position::new(x.saturating_add(width - 1), y)) >= ordered(start)
                    && ordered(Position::new(x, y)) <= ordered(end)
                {
                    row.push_str(symbol);
                }
                x = x.saturating_add(width);
            }
            if y != start.y {
                text.push('\n');
            }
            text.push_str(row.trim_end());
            if text.len() > crate::login_link::MAX_SELECTION_BYTES {
                return None;
            }
        }
        Some(text)
    }
}

fn ordered(p: Position) -> (u16, u16) {
    (p.y, p.x)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{layout::Rect, style::Style};
    use ratatui_crossterm::crossterm::event::KeyModifiers;
    fn event(kind: MouseEventKind, x: u16, y: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        }
    }
    #[test]
    fn forward_reverse_unicode_and_click() {
        let mut screen = Buffer::empty(Rect::new(0, 0, 8, 2));
        screen.set_string(0, 0, "a界e\u{301}", Style::default());
        screen.set_string(0, 1, "next", Style::default());
        for (a, b) in [((1, 0), (3, 1)), ((3, 1), (1, 0))] {
            let mut selection = Selection::default();
            selection.render(&mut screen.clone());
            selection.mouse(event(MouseEventKind::Down(MouseButton::Left), a.0, a.1));
            selection.mouse(event(MouseEventKind::Drag(MouseButton::Left), b.0, b.1));
            let Gesture::Copy(text) =
                selection.mouse(event(MouseEventKind::Up(MouseButton::Left), b.0, b.1))
            else {
                panic!("expected copy");
            };
            assert_eq!(text, "界e\u{301}\nnext");
        }
        let mut selection = Selection::default();
        selection.render(&mut screen);
        selection.mouse(event(MouseEventKind::Down(MouseButton::Left), 0, 0));
        assert!(matches!(
            selection.mouse(event(MouseEventKind::Up(MouseButton::Left), 0, 0)),
            Gesture::Click
        ));
    }
    #[test]
    fn changing_content_cancels_drag() {
        let mut screen = Buffer::empty(Rect::new(0, 0, 8, 2));
        let mut selection = Selection::default();
        selection.render(&mut screen);
        selection.mouse(event(MouseEventKind::Down(MouseButton::Left), 0, 0));
        selection.mouse(event(MouseEventKind::Drag(MouseButton::Left), 3, 0));
        screen.set_string(0, 0, "new", Style::default());
        selection.render(&mut screen);
        assert!(matches!(
            selection.mouse(event(MouseEventKind::Up(MouseButton::Left), 3, 0)),
            Gesture::Consumed
        ));
    }
}
