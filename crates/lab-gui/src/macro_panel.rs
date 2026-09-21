// SPDX-License-Identifier: GPL-3.0-only
// SPDX-FileCopyrightText: 2026 Corey T. White

//! The macro view: a canvas mirroring the hardware (8 knobs, 4 faders,
//! 8 pads). Bidirectional: hardware moves animate it (via app state),
//! dragging knobs and faders publishes control changes.

use iced::mouse;
use iced::widget::canvas::{self, Frame, Geometry, Path, Stroke, Text};
use iced::{Color, Point, Radians, Rectangle, Renderer, Theme};

use lab_core::mapping::Control;

use crate::Message;
use crate::theme;

/// A control's display state, projected from app state each frame.
#[derive(Debug, Clone)]
pub struct ControlView {
    pub control: Control,
    pub label: String,
    pub normalized: f64,
    /// Dimmed when the mapped parameter looks unassigned in this patch.
    pub active: bool,
}

/// Per-pad visual: pressed state and, in Loops mode, a slot color.
#[derive(Debug, Clone, Copy)]
pub struct PadView {
    pub down: bool,
    pub slot_color: Option<Color>,
}

pub struct MacroPanel<'a> {
    pub controls: &'a [ControlView],
    pub pads: [PadView; 8],
}

#[derive(Debug, Default)]
pub struct DragState {
    drag: Option<Drag>,
}

#[derive(Debug)]
struct Drag {
    control: Control,
    value: f64,
    last_y: f32,
}

/// Vertical drag distance for a full 0..=1 sweep, in pixels.
const DRAG_RANGE: f32 = 150.0;

/// Knob radius scaled to the knob grid cell, clamped to stay usable.
fn knob_radius(bounds: Rectangle) -> f32 {
    let cell_w = bounds.width * 0.66 / 4.0;
    let cell_h = bounds.height * 0.62 / 2.0;
    (cell_w.min(cell_h) * 0.32).clamp(18.0, 48.0)
}

impl<'a> MacroPanel<'a> {
    fn value_of(&self, control: Control) -> f64 {
        self.controls
            .iter()
            .find(|c| c.control == control)
            .map(|c| c.normalized)
            .unwrap_or(0.0)
    }

    fn hit_test(&self, bounds: Rectangle, position: Point) -> Option<Control> {
        let radius = knob_radius(bounds);
        for i in 0..8u8 {
            let center = knob_center(i, bounds);
            let dx = position.x - center.x;
            let dy = position.y - center.y;
            if (dx * dx + dy * dy).sqrt() <= radius + 6.0 {
                return Some(Control::Encoder(i));
            }
        }
        for i in 0..4u8 {
            if fader_rect(i, bounds).contains(position) {
                return Some(Control::Fader(i));
            }
        }
        None
    }
}

impl canvas::Program<Message> for MacroPanel<'_> {
    type State = DragState;

    fn update(
        &self,
        state: &mut DragState,
        event: &canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        match event {
            canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                // position_in is already relative to the widget's bounds.
                let position = cursor.position_in(bounds)?;
                let local = Rectangle {
                    x: 0.0,
                    y: 0.0,
                    ..bounds
                };
                if let Some(control) = self.hit_test(local, position) {
                    state.drag = Some(Drag {
                        control,
                        value: self.value_of(control),
                        last_y: position.y,
                    });
                    return Some(canvas::Action::request_redraw().and_capture());
                }
                for i in 0..8u8 {
                    if pad_rect(i, local).contains(position) {
                        return Some(canvas::Action::publish(Message::PadClicked(i)).and_capture());
                    }
                }
                None?
            }
            canvas::Event::Mouse(mouse::Event::CursorMoved { position }) => {
                let drag = state.drag.as_mut()?;
                let y = position.y - bounds.y;
                let delta = (drag.last_y - y) / DRAG_RANGE;
                drag.last_y = y;
                drag.value = (drag.value + delta as f64).clamp(0.0, 1.0);
                Some(
                    canvas::Action::publish(Message::ControlDragged(drag.control, drag.value))
                        .and_capture(),
                )
            }
            canvas::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                state.drag.take()?;
                Some(canvas::Action::request_redraw().and_capture())
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        _state: &DragState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let local = Rectangle {
            x: 0.0,
            y: 0.0,
            ..bounds
        };
        frame.fill_rectangle(Point::ORIGIN, bounds.size(), theme::PANEL);

        for i in 0..8u8 {
            self.draw_knob(&mut frame, i, local);
        }
        for i in 0..4u8 {
            self.draw_fader(&mut frame, i, local);
        }
        for i in 0..8u8 {
            let rect = pad_rect(i, local);
            let pad = self.pads[i as usize];
            let fill = if pad.down {
                theme::PAD_ON
            } else if let Some(color) = pad.slot_color {
                color
            } else {
                theme::PAD_OFF
            };
            frame.fill_rectangle(Point::new(rect.x, rect.y), rect.size(), fill);
            frame.fill_text(Text {
                content: format!("{}", i + 1),
                position: Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0),
                color: if pad.down || pad.slot_color.is_some() {
                    Color::BLACK
                } else {
                    theme::TEXT_DIM
                },
                size: 12.0.into(),
                align_x: iced::widget::text::Alignment::Center,
                align_y: iced::alignment::Vertical::Center,
                ..Text::default()
            });
        }

        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &DragState,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if state.drag.is_some() {
            return mouse::Interaction::ResizingVertically;
        }
        let local = Rectangle {
            x: 0.0,
            y: 0.0,
            ..bounds
        };
        match cursor
            .position_in(bounds)
            .and_then(|p| self.hit_test(local, p))
        {
            Some(_) => mouse::Interaction::Pointer,
            None => mouse::Interaction::default(),
        }
    }
}

impl MacroPanel<'_> {
    fn draw_knob(&self, frame: &mut Frame, index: u8, bounds: Rectangle) {
        let radius = knob_radius(bounds);
        let center = knob_center(index, bounds);
        let value = self.value_of(Control::Encoder(index)) as f32;
        let view = self
            .controls
            .iter()
            .find(|c| c.control == Control::Encoder(index));
        let label = view
            .map(|c| c.label.clone())
            .unwrap_or_else(|| format!("Enc {}", index + 1));
        let active = view.is_some_and(|c| c.active);
        let dim = |color: Color| {
            if active {
                color
            } else {
                Color { a: 0.35, ..color }
            }
        };

        // 270-degree sweep from lower-left to lower-right.
        let start = Radians(std::f32::consts::PI * 0.75);
        let sweep = std::f32::consts::PI * 1.5;

        let track = Path::new(|b| {
            b.arc(canvas::path::Arc {
                center,
                radius,
                start_angle: start,
                end_angle: Radians(start.0 + sweep),
            })
        });
        frame.stroke(
            &track,
            Stroke::default()
                .with_color(dim(theme::TRACK))
                .with_width(5.0),
        );
        if value > 0.001 {
            let fill = Path::new(|b| {
                b.arc(canvas::path::Arc {
                    center,
                    radius,
                    start_angle: start,
                    end_angle: Radians(start.0 + sweep * value),
                })
            });
            frame.stroke(
                &fill,
                Stroke::default()
                    .with_color(dim(theme::ACCENT))
                    .with_width(5.0),
            );
        }
        // Pointer line.
        let angle = start.0 + sweep * value;
        let pointer = Path::line(
            Point::new(
                center.x + angle.cos() * (radius - 12.0),
                center.y + angle.sin() * (radius - 12.0),
            ),
            Point::new(
                center.x + angle.cos() * (radius - 3.0),
                center.y + angle.sin() * (radius - 3.0),
            ),
        );
        frame.stroke(
            &pointer,
            Stroke::default()
                .with_color(dim(theme::TEXT))
                .with_width(3.0),
        );

        // Current value in the middle of the knob.
        frame.fill_text(Text {
            content: format!("{:.0}%", value * 100.0),
            position: center,
            color: dim(theme::TEXT),
            size: 12.0.into(),
            align_x: iced::widget::text::Alignment::Center,
            align_y: iced::alignment::Vertical::Center,
            ..Text::default()
        });
        frame.fill_text(Text {
            content: label,
            position: Point::new(center.x, center.y + radius + 12.0),
            color: theme::TEXT_DIM,
            size: 11.0.into(),
            align_x: iced::widget::text::Alignment::Center,
            align_y: iced::alignment::Vertical::Center,
            ..Text::default()
        });
    }

    fn draw_fader(&self, frame: &mut Frame, index: u8, bounds: Rectangle) {
        let rect = fader_rect(index, bounds);
        let value = self.value_of(Control::Fader(index)) as f32;
        let label = self
            .controls
            .iter()
            .find(|c| c.control == Control::Fader(index))
            .map(|c| c.label.clone())
            .unwrap_or_else(|| format!("F{}", index + 1));

        let track_x = rect.x + rect.width / 2.0;
        let track = Path::line(
            Point::new(track_x, rect.y),
            Point::new(track_x, rect.y + rect.height),
        );
        frame.stroke(
            &track,
            Stroke::default().with_color(theme::TRACK).with_width(4.0),
        );

        let handle_y = rect.y + rect.height * (1.0 - value);
        let filled = Path::line(
            Point::new(track_x, handle_y),
            Point::new(track_x, rect.y + rect.height),
        );
        frame.stroke(
            &filled,
            Stroke::default()
                .with_color(theme::ACCENT_DIM)
                .with_width(4.0),
        );
        frame.fill_rectangle(
            Point::new(track_x - 10.0, handle_y - 5.0),
            iced::Size::new(20.0, 10.0),
            theme::ACCENT,
        );

        // Fader columns are narrow; shorten the label rather than let
        // neighbouring labels collide.
        let column_width = bounds.width * 0.30 / 4.0;
        let label = if column_width < 70.0 && label.len() > 3 {
            label.chars().take(3).collect::<String>()
        } else {
            label
        };
        frame.fill_text(Text {
            content: label,
            position: Point::new(track_x, rect.y + rect.height + 12.0),
            color: theme::TEXT_DIM,
            size: 11.0.into(),
            align_x: iced::widget::text::Alignment::Center,
            align_y: iced::alignment::Vertical::Center,
            ..Text::default()
        });
    }
}

/// Knobs: 2 rows x 4 columns in the left ~2/3, top ~60% of the panel.
fn knob_center(index: u8, bounds: Rectangle) -> Point {
    let cols = 4.0;
    let area_w = bounds.width * 0.66;
    let area_h = bounds.height * 0.62;
    let col = (index % 4) as f32;
    let row = (index / 4) as f32;
    Point::new(
        bounds.x + area_w * (col + 0.5) / cols,
        bounds.y + area_h * (row + 0.5) / 2.0,
    )
}

/// Faders: 4 columns in the right ~1/3, top ~60%.
fn fader_rect(index: u8, bounds: Rectangle) -> Rectangle {
    let area_x = bounds.x + bounds.width * 0.68;
    let area_w = bounds.width * 0.30;
    let area_h = bounds.height * 0.55;
    let col_w = area_w / 4.0;
    Rectangle {
        x: area_x + col_w * index as f32 + col_w * 0.25,
        y: bounds.y + 14.0,
        width: col_w * 0.5,
        height: area_h - 14.0,
    }
}

/// Pads: one row of 8 along the bottom.
fn pad_rect(index: u8, bounds: Rectangle) -> Rectangle {
    let area_y = bounds.y + bounds.height * 0.72;
    let area_h = bounds.height * 0.24;
    let col_w = bounds.width / 8.0;
    Rectangle {
        x: bounds.x + col_w * index as f32 + col_w * 0.08,
        y: area_y,
        width: col_w * 0.84,
        height: area_h,
    }
}
