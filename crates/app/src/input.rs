use gpui::{
    App, Bounds, Context, CursorStyle, Element, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, IntoElement,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point,
    ShapedLine, SharedString, Style, TextRun, UTF16Selection, UnderlineStyle, Window, div, fill,
    point, prelude::*, px, rgb, rgba, size,
};
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone)]
pub enum TextInputEvent {
    Changed(String),
}

pub struct TextInput {
    focus_handle: FocusHandle,
    content: SharedString,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    selecting: bool,
    multiline: bool,
}

impl TextInput {
    pub fn new(cx: &mut Context<Self>, placeholder: &str, multiline: bool) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            content: "".into(),
            placeholder: placeholder.to_owned().into(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_layout: None,
            last_bounds: None,
            selecting: false,
            multiline,
        }
    }
    pub fn content(&self) -> String {
        self.content.to_string()
    }
    pub fn set_content(&mut self, content: String, cx: &mut Context<Self>) {        if self.content.as_ref() == content {
            return;
        }
        self.content = content.into();
        self.selected_range = self.content.len()..self.content.len();
        cx.emit(TextInputEvent::Changed(self.content.to_string()));
        cx.notify();
    }
    fn cursor(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }
    fn move_to(&mut self, offset: usize, _: &mut Window, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        cx.notify();
    }
    fn select_to(&mut self, offset: usize, _: &mut Window, cx: &mut Context<Self>) {
        if self.selection_reversed {
            self.selected_range.start = offset
        } else {
            self.selected_range.end = offset
        }
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        cx.notify();
    }
    fn previous(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(index, _)| (index < offset).then_some(index))
            .unwrap_or(0)
    }
    fn next(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(index, _)| (index > offset).then_some(index))
            .unwrap_or(self.content.len())
    }
    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8 = 0;
        let mut utf16 = 0;
        for ch in self.content.chars() {
            if utf16 >= offset {
                break;
            }
            utf16 += ch.len_utf16();
            utf8 += ch.len_utf8();
        }
        utf8
    }
    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf8 = 0;
        let mut utf16 = 0;
        for ch in self.content.chars() {
            if utf8 >= offset {
                break;
            }
            utf16 += ch.len_utf16();
            utf8 += ch.len_utf8();
        }
        utf16
    }
    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }
    fn range_from_utf16(&self, range: Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }
    fn index_for_mouse(&self, position: Point<Pixels>) -> usize {
        let (Some(bounds), Some(line)) = (&self.last_bounds, &self.last_layout) else {
            return 0;
        };
        if position.y < bounds.top() {
            0
        } else if position.y > bounds.bottom() {
            self.content.len()
        } else {
            line.closest_index_for_x(position.x - bounds.left())
        }
    }
    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.selecting = true;
        self.move_to(self.index_for_mouse(event.position), window, cx);
    }
    fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.selecting = false;
    }
    fn mouse_move(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.selecting {
            self.select_to(self.index_for_mouse(event.position), window, cx);
        }
    }
    fn key_down(&mut self, event: &gpui::KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let modifiers = event.keystroke.modifiers;
        let offset = match event.keystroke.key.as_str() {
            "left" => self.previous(self.selected_range.start),
            "right" => self.next(self.selected_range.end),
            "backspace" => {
                let end = self.selected_range.end;
                let start = if self.selected_range.is_empty() {
                    self.previous(end)
                } else {
                    self.selected_range.start
                };
                self.content =
                    format!("{}{}", &self.content[..start], &self.content[end..]).into();
                self.selected_range = start..start;
                self.selection_reversed = false;
                cx.emit(TextInputEvent::Changed(self.content.to_string()));
                cx.notify();
                return;
            }
            _ => return,
        };
        if modifiers.shift {
            let anchor = if self.selection_reversed {
                self.selected_range.end
            } else {
                self.selected_range.start
            };
            self.selected_range = anchor.min(offset)..anchor.max(offset);
            self.selection_reversed = offset < anchor;
        } else {
            self.move_to(offset, window, cx);
        }
        cx.notify();
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(range);
        *actual = Some(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }
    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked_range = None;
        cx.notify();
    }
    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range
            .map(|range| self.range_from_utf16(range))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        let text = if self.multiline {
            text.to_string()
        } else {
            text.replace('\n', " ")
        };
        self.content = format!(
            "{}{}{}",
            &self.content[..range.start],
            text,
            &self.content[range.end..]
        )
        .into();
        self.selected_range = range.start + text.len()..range.start + text.len();
        self.selection_reversed = false;
        self.marked_range = None;
        cx.emit(TextInputEvent::Changed(self.content.to_string()));
        cx.notify();
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = if self.multiline {
            text.to_string()
        } else {
            text.replace('\n', " ")
        };
        self.replace_text_in_range(range, &text, window, cx);
        let start = self.selected_range.start.saturating_sub(text.len());
        if !text.is_empty() {
            self.marked_range = Some(start..start + text.len());
        }
        if let Some(selected) = selected {
            let start_utf16 = self.offset_to_utf16(start);
            self.selected_range =
                self.range_from_utf16(start_utf16 + selected.start..start_utf16 + selected.end);
        }
        cx.notify();
    }
    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let line = self.last_layout.as_ref()?;
        if line.text != self.content {
            return None;
        }
        let range = self.range_from_utf16(range);
        Some(Bounds::from_corners(
            point(bounds.left() + line.x_for_index(range.start), bounds.top()),
            point(bounds.left() + line.x_for_index(range.end), bounds.bottom()),
        ))
    }
    fn character_index_for_point(
        &mut self,
        position: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.offset_to_utf16(self.index_for_mouse(position)))
    }
}

struct InputElement {
    input: Entity<TextInput>,
}
struct Prepaint {
    line: Option<ShapedLine>,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
}
impl IntoElement for InputElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl Element for InputElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;
    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = gpui::relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Prepaint {
        let input = self.input.read(cx);
        let content = input.content.clone();
        let range = input.selected_range.clone();
        let cursor = input.cursor();
        let style = window.text_style();
        let (text, color) = if content.is_empty() {
            (input.placeholder.clone(), rgb(0x91a099).into())
        } else {
            (content, style.color)
        };
        let run = TextRun {
            len: text.len(),
            font: style.font(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = if let Some(marked) = &input.marked_range {
            vec![
                TextRun {
                    len: marked.start,
                    ..run.clone()
                },
                TextRun {
                    len: marked.end - marked.start,
                    underline: Some(UnderlineStyle {
                        color: Some(run.color),
                        thickness: px(1.),
                        wavy: true,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: text.len() - marked.end,
                    ..run
                },
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect()
        } else {
            vec![run]
        };
        let line = window.text_system().shape_line(
            text,
            style.font_size.to_pixels(window.rem_size()),
            &runs,
            None,
        );
        let cursor_position = line.x_for_index(cursor);
        let (selection, cursor) = if range.is_empty() {
            (
                None,
                Some(fill(
                    Bounds::new(
                        point(bounds.left() + cursor_position, bounds.top()),
                        size(px(2.), bounds.bottom() - bounds.top()),
                    ),
                    rgb(0x176b5b),
                )),
            )
        } else {
            (
                Some(fill(
                    Bounds::from_corners(
                        point(bounds.left() + line.x_for_index(range.start), bounds.top()),
                        point(bounds.left() + line.x_for_index(range.end), bounds.bottom()),
                    ),
                    rgba(0x176b5b33),
                )),
                None,
            )
        };
        Prepaint {
            line: Some(line),
            cursor,
            selection,
        }
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        state: &mut Prepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        if let Some(selection) = state.selection.take() {
            window.paint_quad(selection);
        }
        let line = state.line.take().expect("input line");
        line.paint(bounds.origin, window.line_height(), window, cx)
            .expect("paint input");
        if focus.is_focused(window)
            && let Some(cursor) = state.cursor.take()
        {
            window.paint_quad(cursor);
        }
        self.input.update(cx, |input, _| {
            input.last_layout = Some(line);
            input.last_bounds = Some(bounds);
        });
    }
}
impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
impl EventEmitter<TextInputEvent> for TextInput {}
impl Render for TextInput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context("TextInput")
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_key_down(cx.listener(Self::key_down))
            .w_full()
            .min_h(px(96.))
            .p_3()
            .rounded_md()
            .bg(rgb(0xf7faf8))
            .border_1()
            .border_color(rgb(0xd6dfdb))
            .child(InputElement { input: cx.entity() })
    }
}
