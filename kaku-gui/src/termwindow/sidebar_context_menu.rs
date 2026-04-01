use crate::termwindow::box_model::*;
use crate::termwindow::modal::Modal;
use crate::termwindow::{SidebarAction, TermWindow, UIItem};
use crate::utilsprites::RenderMetrics;
use anyhow::{ensure, Context};
use config::{Dimension, DimensionContext};
use std::cell::{Ref, RefCell};
use termwiz::cell::unicode_column_width;
use wezterm_term::color::ColorPalette;
use wezterm_term::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use window::color::LinearRgba;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SidebarContextMenuTone {
    Default,
    Danger,
}

#[derive(Clone, Debug)]
pub struct SidebarContextMenuItem {
    pub label: String,
    pub action: SidebarAction,
    pub tone: SidebarContextMenuTone,
}

impl SidebarContextMenuItem {
    pub fn new(label: impl Into<String>, action: SidebarAction) -> Self {
        Self {
            label: label.into(),
            action,
            tone: SidebarContextMenuTone::Default,
        }
    }

    pub fn danger(label: impl Into<String>, action: SidebarAction) -> Self {
        Self {
            label: label.into(),
            action,
            tone: SidebarContextMenuTone::Danger,
        }
    }
}

pub struct SidebarContextMenuModal {
    element: RefCell<Option<Vec<ComputedElement>>>,
    anchor: UIItem,
    items: Vec<SidebarContextMenuItem>,
    selected_row: RefCell<usize>,
}

impl SidebarContextMenuModal {
    pub fn new(
        term_window: &mut TermWindow,
        anchor: UIItem,
        items: Vec<SidebarContextMenuItem>,
    ) -> anyhow::Result<Self> {
        ensure!(
            !items.is_empty(),
            "sidebar context menu requires at least one item"
        );
        let modal = Self {
            element: RefCell::new(None),
            anchor,
            items,
            selected_row: RefCell::new(0),
        };
        modal.reconfigure(term_window);
        Ok(modal)
    }

    fn row_height(metrics: &RenderMetrics) -> f32 {
        (metrics.cell_size.height as f32 * 1.35).max(24.0)
    }

    fn panel_width(&self, metrics: &RenderMetrics, window_width: f32) -> f32 {
        let max_columns = self
            .items
            .iter()
            .map(|item| unicode_column_width(item.label.as_str(), None))
            .max()
            .unwrap_or(8);
        let desired = (max_columns as f32 + 8.0) * metrics.cell_size.width as f32;
        let min_width = self.anchor.width.max(220) as f32;
        desired.clamp(min_width, (window_width - 16.0).max(min_width))
    }

    fn row_from_abs_point(&self, abs_x: f32, abs_y: f32) -> Option<usize> {
        let element = self.element.borrow();
        let root = element.as_ref()?.first()?;
        if abs_x < root.bounds.min_x()
            || abs_x > root.bounds.max_x()
            || abs_y < root.bounds.min_y()
            || abs_y > root.bounds.max_y()
        {
            return None;
        }
        let kids = match &root.content {
            ComputedElementContent::Children(kids) => kids,
            _ => return None,
        };

        kids.iter().position(|kid| {
            abs_x >= kid.bounds.min_x()
                && abs_x <= kid.bounds.max_x()
                && abs_y >= kid.bounds.min_y()
                && abs_y <= kid.bounds.max_y()
        })
    }

    fn set_selection(&self, row: usize) -> bool {
        if row >= self.items.len() {
            return false;
        }
        let mut selected = self.selected_row.borrow_mut();
        if *selected == row {
            return false;
        }
        *selected = row;
        true
    }

    fn move_selection(&self, delta: isize) -> bool {
        if self.items.is_empty() {
            return false;
        }
        let selected = *self.selected_row.borrow() as isize;
        let next = (selected + delta).clamp(0, (self.items.len() - 1) as isize) as usize;
        self.set_selection(next)
    }

    fn activate_selected(&self, term_window: &mut TermWindow) {
        let selected = *self.selected_row.borrow();
        let action = self.items.get(selected).map(|item| item.action.clone());
        term_window.cancel_modal();
        if let Some(action) = action {
            if let Err(err) = term_window.perform_workspace_sidebar_action(action) {
                log::warn!("workspace sidebar context-menu action failed: {:#}", err);
                term_window.show_toast("Sidebar action failed".to_string());
            }
        }
    }

    fn map_event_point_to_abs(event: MouseEvent, term_window: &mut TermWindow) -> (f32, f32) {
        let top_bar_height = if term_window.show_tab_bar && !term_window.config.tab_bar_at_bottom {
            term_window.tab_bar_pixel_height().unwrap_or(0.0)
        } else {
            0.0
        };
        let (padding_left, padding_top) = term_window.padding_left_top();
        let border = term_window.get_os_border();
        let content_x = padding_left + border.left.get() as f32;
        let content_y = top_bar_height + padding_top + border.top.get() as f32;
        let cell_width = term_window.render_metrics.cell_size.width as f32;
        let cell_height = term_window.render_metrics.cell_size.height as f32;
        let abs_x = content_x + event.x as f32 * cell_width + event.x_pixel_offset as f32;
        let abs_y = content_y + event.y as f32 * cell_height + event.y_pixel_offset as f32;
        (abs_x, abs_y)
    }

    fn row_text_color(item: &SidebarContextMenuItem, palette: &ColorPalette) -> LinearRgba {
        match item.tone {
            SidebarContextMenuTone::Default => palette.foreground.to_linear(),
            SidebarContextMenuTone::Danger => LinearRgba(0.95, 0.32, 0.32, 1.0),
        }
    }

    fn compute(&self, term_window: &mut TermWindow) -> anyhow::Result<Vec<ComputedElement>> {
        let font = term_window
            .fonts
            .title_font()
            .context("resolve sidebar context menu font")?;
        let metrics = RenderMetrics::with_font_metrics(&font.metrics());
        let dimensions = term_window.dimensions;
        let row_height = Self::row_height(&metrics);
        let panel_height = row_height * self.items.len() as f32 + 8.0;
        let window_width = dimensions.pixel_width as f32;
        let window_height = dimensions.pixel_height as f32;
        let panel_width = self.panel_width(&metrics, window_width);

        let x = self.anchor.x as f32 + 6.0;
        let x = x.clamp(8.0, (window_width - panel_width - 8.0).max(8.0));
        let below_y = self.anchor.y as f32 + 2.0;
        let y = if below_y + panel_height <= window_height - 8.0 {
            below_y
        } else {
            (self.anchor.y as f32 - panel_height - 2.0).max(8.0)
        };

        let palette = term_window.palette().clone();
        let panel_bg = palette.background.to_linear().mul_alpha(0.97);
        let border = palette.foreground.to_linear().mul_alpha(0.24);
        let selected_bg = palette.foreground.to_linear().mul_alpha(0.16);

        let mut rows = Vec::with_capacity(self.items.len());
        let selected_row = *self.selected_row.borrow();
        for (idx, item) in self.items.iter().enumerate() {
            let row_bg = if idx == selected_row {
                selected_bg
            } else {
                LinearRgba::TRANSPARENT
            };
            rows.push(
                Element::new(
                    &font,
                    ElementContent::Children(vec![Element::new(
                        &font,
                        ElementContent::Text(item.label.clone()),
                    )
                    .colors(ElementColors {
                        border: BorderColor::default(),
                        bg: LinearRgba::TRANSPARENT.into(),
                        text: Self::row_text_color(item, &palette).into(),
                    })]),
                )
                .colors(ElementColors {
                    border: BorderColor::default(),
                    bg: row_bg.into(),
                    text: palette.foreground.to_linear().into(),
                })
                .padding(BoxDimension {
                    left: Dimension::Cells(0.8),
                    right: Dimension::Cells(0.8),
                    top: Dimension::Cells(0.3),
                    bottom: Dimension::Cells(0.3),
                })
                .min_height(Some(Dimension::Pixels(row_height)))
                .min_width(Some(Dimension::Percent(1.0)))
                .display(DisplayType::Block),
            );
        }

        let element = Element::new(&font, ElementContent::Children(rows))
            .colors(ElementColors {
                border: BorderColor::new(border),
                bg: panel_bg.into(),
                text: palette.foreground.to_linear().into(),
            })
            .padding(BoxDimension {
                left: Dimension::Pixels(2.0),
                right: Dimension::Pixels(2.0),
                top: Dimension::Pixels(2.0),
                bottom: Dimension::Pixels(2.0),
            })
            .border(BoxDimension::new(Dimension::Pixels(1.0)))
            .min_width(Some(Dimension::Pixels(panel_width)))
            .display(DisplayType::Block);

        let computed = term_window.compute_element(
            &LayoutContext {
                height: DimensionContext {
                    dpi: dimensions.dpi as f32,
                    pixel_max: dimensions.pixel_height as f32,
                    pixel_cell: metrics.cell_size.height as f32,
                },
                width: DimensionContext {
                    dpi: dimensions.dpi as f32,
                    pixel_max: dimensions.pixel_width as f32,
                    pixel_cell: metrics.cell_size.width as f32,
                },
                bounds: euclid::rect(x, y, panel_width, panel_height),
                metrics: &metrics,
                gl_state: term_window.render_state.as_ref().unwrap(),
                zindex: 120,
            },
            &element,
        )?;

        Ok(vec![computed])
    }
}

impl Modal for SidebarContextMenuModal {
    fn mouse_event(&self, event: MouseEvent, term_window: &mut TermWindow) -> anyhow::Result<()> {
        let (abs_x, abs_y) = Self::map_event_point_to_abs(event, term_window);

        match (event.kind, event.button) {
            (MouseEventKind::Move, MouseButton::None | MouseButton::Left) => {
                if let Some(row) = self.row_from_abs_point(abs_x, abs_y) {
                    if self.set_selection(row) {
                        term_window.invalidate_modal();
                    }
                }
            }
            (MouseEventKind::Press, MouseButton::Left) => {
                if let Some(row) = self.row_from_abs_point(abs_x, abs_y) {
                    let _ = self.set_selection(row);
                    self.activate_selected(term_window);
                } else {
                    term_window.cancel_modal();
                }
            }
            (MouseEventKind::Press, MouseButton::Right) => {
                term_window.cancel_modal();
            }
            (MouseEventKind::Press, MouseButton::WheelUp(lines)) => {
                if self.move_selection(-(lines.max(1).min(4) as isize)) {
                    term_window.invalidate_modal();
                }
            }
            (MouseEventKind::Press, MouseButton::WheelDown(lines)) => {
                if self.move_selection(lines.max(1).min(4) as isize) {
                    term_window.invalidate_modal();
                }
            }
            (MouseEventKind::Press, _) => {
                term_window.cancel_modal();
            }
            _ => {}
        }

        Ok(())
    }

    fn key_down(
        &self,
        key: KeyCode,
        mods: KeyModifiers,
        term_window: &mut TermWindow,
    ) -> anyhow::Result<bool> {
        let handled = match (key, mods) {
            (KeyCode::Escape, KeyModifiers::NONE) | (KeyCode::Char('g'), KeyModifiers::CTRL) => {
                term_window.cancel_modal();
                true
            }
            (KeyCode::Enter, KeyModifiers::NONE) => {
                self.activate_selected(term_window);
                true
            }
            (KeyCode::UpArrow, KeyModifiers::NONE) | (KeyCode::Char('k'), KeyModifiers::NONE) => {
                self.move_selection(-1)
            }
            (KeyCode::DownArrow, KeyModifiers::NONE) | (KeyCode::Char('j'), KeyModifiers::NONE) => {
                self.move_selection(1)
            }
            (KeyCode::Char(c), KeyModifiers::NONE) if c.is_ascii_digit() => {
                let row = match c.to_digit(10) {
                    Some(0) | None => return Ok(false),
                    Some(v) => v as usize - 1,
                };
                if row < self.items.len() {
                    let _ = self.set_selection(row);
                    self.activate_selected(term_window);
                    true
                } else {
                    false
                }
            }
            _ => false,
        };

        if handled {
            term_window.invalidate_modal();
        }

        Ok(handled)
    }

    fn focus_changed(&self, focused: bool, term_window: &mut TermWindow) {
        if !focused {
            term_window.cancel_modal();
        }
    }

    fn computed_element(
        &self,
        term_window: &mut TermWindow,
    ) -> anyhow::Result<Ref<'_, [ComputedElement]>> {
        if self.element.borrow().is_none() {
            let element = self.compute(term_window)?;
            self.element.borrow_mut().replace(element);
        }

        Ok(Ref::map(self.element.borrow(), |value| {
            value.as_ref().unwrap().as_slice()
        }))
    }

    fn reconfigure(&self, _term_window: &mut TermWindow) {
        self.element.borrow_mut().take();
    }
}
