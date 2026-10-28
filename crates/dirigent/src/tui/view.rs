//! Drawing the TUI from the model.

use dirigera::{ColorMode, Device, DeviceKind, Home, Room, Scene, TemperatureRange};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph};

use super::keymap::{BINDINGS, Hint};
use super::model::{
    self, Dialog, Entry, FLASH, Link, Model, Overlay, Phase, Picker, PickerRow, Row, SWITCH_FLASH,
    Tab, Tone, Wheel, count, presets,
};
use crate::theme::{self, LOW_BATTERY, Rgb};

const DIM: Style = Style::new().add_modifier(Modifier::DIM);
const BOLD: Style = Style::new().add_modifier(Modifier::BOLD);
/// Borders of panes without focus, and status line segments.
const MUTED: Color = Color::Indexed(8);
const GOOD: Color = Color::Indexed(2);
const WARN: Color = Color::Indexed(3);
const BAD: Color = Color::Indexed(1);
const ACCENT: Color = Color::Indexed(4);
const PRESS: Color = Color::Indexed(5);
const BRIGHT: Color = Color::Indexed(15);
/// Background of the selected row.
const SELECTED: Color = Color::Indexed(8);
const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
/// Bars of rising height, for brightness in the list.
const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Smallest window for the narrow layout, and for the wide one.
const TABS_MIN: (u16, u16) = (46, 16);
const WIDE_MIN: (u16, u16) = (100, 24);
/// Width of the list pane in the wide layout, and height of the detail pane.
const LIST_WIDTH: u16 = 46;
const DETAIL_HEIGHT: u16 = 12;
/// Width of the labels in the detail pane, and of the values after a bar.
const LABEL: usize = 12;
const VALUE: usize = 8;

/// What the mouse can point at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Row(Row),
    /// The dot of a device: a click switches it.
    Toggle(Row),
    Tab(Tab),
    /// A bar of the selected light: the wheel turns it.
    Wheel(Wheel),
}

/// A screen area the mouse can point at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hotspot {
    pub area: Rect,
    pub target: Target,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    Wide,
    Tabs,
    TooSmall,
}

fn shape(area: Rect) -> Shape {
    let fits = |(width, height): (u16, u16)| area.width >= width && area.height >= height;
    if fits(WIDE_MIN) {
        Shape::Wide
    } else if fits(TABS_MIN) {
        Shape::Tabs
    } else {
        Shape::TooSmall
    }
}

/// Draws the whole screen and returns the areas the mouse can point at.
pub fn render(model: &Model, frame: &mut Frame) -> Vec<Hotspot> {
    let area = frame.area();
    let shape = shape(area);
    let mut spots = Vec::new();
    if shape == Shape::TooSmall {
        too_small(frame, area);
        return spots;
    }
    let [body, status, hints] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);
    match &model.phase {
        Phase::Ready if shape == Shape::Wide => wide(model, frame, body, &mut spots),
        Phase::Ready => tabs(model, frame, body, &mut spots),
        Phase::Starting => {
            let hub = model.hub_name.as_deref().unwrap_or("the hub");
            let text = vec![Line::from(vec![
                spinner(model),
                Span::raw(format!(" connecting to {hub}")),
            ])];
            dialog(frame, body, " dirigent ", text);
        }
        Phase::Pairing(state) => pairing(model, state, frame, body),
        Phase::Unreachable { message, retry_at } => {
            let left = retry_at.duration_since(model.now).as_secs().max(0);
            let text = vec![
                Line::styled(format!("✗ {message}"), Style::new().fg(BAD)),
                Line::default(),
                Line::styled(format!("trying again in {left} s"), DIM),
            ];
            dialog(frame, body, " dirigent ", text);
        }
    }
    if model.phase == Phase::Ready && matches!(model.link, Link::Offline { .. }) {
        // Panes go dim while the hub is out of reach: what they show may be old.
        frame.buffer_mut().set_style(body, DIM);
    }
    statusline(model, frame, status);
    hint_bar(model, frame, hints, shape);
    if let Some(open) = &model.picker {
        picker(model, open, frame, area);
        spots.clear();
    }
    if model.overlay == Overlay::Help {
        help(frame, area);
        spots.clear();
    }
    spots
}

fn too_small(frame: &mut Frame, area: Rect) {
    let text = vec![
        Line::from("window too small"),
        Line::styled(format!("{} × {} needed", TABS_MIN.0, TABS_MIN.1), DIM),
    ];
    let height = u16::try_from(text.len()).unwrap_or(u16::MAX);
    let middle = area.centered_vertically(Constraint::Length(height));
    frame.render_widget(Paragraph::new(text).centered(), middle);
}

fn pane(title: &str, focused: bool) -> Block<'_> {
    let color = if focused { ACCENT } else { MUTED };
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(color))
        .title(Span::styled(
            format!(" {title} "),
            BOLD.fg(if focused { ACCENT } else { Color::Reset }),
        ))
        .padding(Padding::horizontal(1))
}

fn right_title(text: &str) -> Line<'static> {
    Line::styled(format!(" {text} "), DIM).right_aligned()
}

fn wide(model: &Model, frame: &mut Frame, body: Rect, spots: &mut Vec<Hotspot>) {
    let [list_area, right] =
        Layout::horizontal([Constraint::Length(LIST_WIDTH), Constraint::Fill(1)]).areas(body);
    let [detail_area, activity_area] =
        Layout::vertical([Constraint::Length(DETAIL_HEIGHT), Constraint::Fill(1)]).areas(right);

    let block = pane("devices", true).title_top(right_title(&on_count(model)));
    let inner = block.inner(list_area);
    frame.render_widget(block, list_area);
    list(model, frame, inner, spots);

    let (title, kind) = selected_title(model);
    let block = pane(&title, false).title_top(right_title(&kind));
    let inner = block.inner(detail_area);
    frame.render_widget(block, detail_area);
    frame.render_widget(
        Paragraph::new(detail(model, usize::from(inner.width))),
        inner,
    );
    // The bars of a light sit under its header and a blank line, in the order drawn.
    for (index, wheel) in wheels(model).into_iter().enumerate() {
        let y = inner.y + 2 + u16::try_from(index).unwrap_or(0);
        if y < inner.bottom() {
            spots.push(Hotspot {
                area: Rect::new(inner.x, y, inner.width, 1),
                target: Target::Wheel(wheel),
            });
        }
    }

    let state = if model.link == Link::Live {
        "live"
    } else {
        "offline"
    };
    let block = pane("activity", false).title_top(right_title(state));
    let inner = block.inner(activity_area);
    frame.render_widget(block, activity_area);
    frame.render_widget(
        Paragraph::new(activity(model, usize::from(inner.width))),
        inner,
    );
}

fn tabs(model: &Model, frame: &mut Frame, body: Rect, spots: &mut Vec<Hotspot>) {
    let block = pane("dirigent", true).title_top(right_title(&on_count(model)));
    let inner = block.inner(body);
    frame.render_widget(block, body);
    let [bar, content] =
        Layout::vertical([Constraint::Length(2), Constraint::Fill(1)]).areas(inner);
    let mut x = bar.x;
    let mut spans = Vec::new();
    for (index, tab) in Tab::ALL.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled("  ·  ", DIM));
            x += 5;
        }
        let label = format!("{} {}", index + 1, tab.name());
        let width = u16::try_from(label.chars().count()).unwrap_or(0);
        spots.push(Hotspot {
            area: Rect::new(x, bar.y, width, 1),
            target: Target::Tab(tab),
        });
        x += width;
        spans.push(if tab == model.tab {
            Span::styled(label, BOLD.fg(ACCENT).add_modifier(Modifier::UNDERLINED))
        } else {
            Span::styled(label, DIM)
        });
    }
    frame.render_widget(Line::from(spans), bar);
    match model.tab {
        Tab::Devices => {
            let compact = compact_detail(model, usize::from(content.width));
            let height = u16::try_from(compact.len()).unwrap_or(0);
            let [list_area, detail_area] =
                Layout::vertical([Constraint::Fill(1), Constraint::Length(height)]).areas(content);
            list(model, frame, list_area, spots);
            frame.render_widget(Paragraph::new(compact), detail_area);
        }
        Tab::Activity => {
            frame.render_widget(
                Paragraph::new(activity(model, usize::from(content.width))),
                content,
            );
        }
    }
}

fn on_count(model: &Model) -> String {
    let shown = model.shown_home();
    let Some(home) = &shown else {
        return String::new();
    };
    let switchable: Vec<&Device> = home
        .devices
        .iter()
        .filter(|d| matches!(d.kind(), DeviceKind::Light | DeviceKind::Outlet))
        .collect();
    let on = switchable
        .iter()
        .filter(|d| d.attributes.is_on() == Some(true))
        .count();
    format!("{on} of {} on", switchable.len())
}

/// `left`, then `right` flush with the end of `width` columns.
fn spread(left: Vec<Span<'static>>, right: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let used: usize = left.iter().chain(&right).map(Span::width).sum();
    let mut spans = left;
    if !right.is_empty() {
        spans.push(Span::raw(" ".repeat(width.saturating_sub(used).max(1))));
        spans.extend(right);
    }
    Line::from(spans)
}

/// `text` shortened to `width` columns with an ellipsis, or padded to them.
fn fit(text: &str, width: usize) -> String {
    let count = text.chars().count();
    if count > width {
        let kept: String = text.chars().take(width.saturating_sub(1)).collect();
        format!("{kept}…")
    } else {
        format!("{text}{}", " ".repeat(width - count))
    }
}

fn rgb(Rgb(r, g, b): Rgb) -> Color {
    Color::Rgb(r, g, b)
}

fn room_color(room: &Room) -> Color {
    let name = room.color.as_deref().unwrap_or_default();
    [
        ("green", GOOD),
        ("blue", ACCENT),
        ("yellow", WARN),
        ("red", BAD),
        ("pink", PRESS),
        ("purple", PRESS),
    ]
    .into_iter()
    .find(|(word, _)| name.contains(word))
    .map_or(Color::Reset, |(_, color)| color)
}

/// A device's dot; it blinks while the device identifies itself.
fn identify_dot(model: &Model, device: &Device) -> Span<'static> {
    if model
        .identifying
        .get(&device.id)
        .is_some_and(|until| *until > model.now)
    {
        if !model.animations {
            return Span::styled("◉", Style::new().fg(ACCENT));
        }
        let lit = (model.now.as_millisecond() / 250) % 2 == 0;
        return Span::styled(if lit { "●" } else { " " }, BOLD.fg(ACCENT));
    }
    dot(device)
}

fn dot(device: &Device) -> Span<'static> {
    match device.attributes.is_on() {
        Some(true) => Span::styled("●", Style::new().fg(GOOD)),
        _ => Span::styled("○", DIM),
    }
}

/// A battery gauge: full only when the battery is, and one cell lit while any charge is left.
fn battery(level: u8, cells: usize) -> Vec<Span<'static>> {
    let lit = (usize::from(level) * cells / 100)
        .max(usize::from(level > 0))
        .min(cells);
    let color = if level < LOW_BATTERY { WARN } else { GOOD };
    vec![
        Span::styled("▰".repeat(lit), Style::new().fg(color)),
        Span::styled("▱".repeat(cells - lit), DIM),
    ]
}

fn list(model: &Model, frame: &mut Frame, area: Rect, spots: &mut Vec<Hotspot>) {
    let shown = model.shown_home();
    let Some(home) = &shown else {
        return;
    };
    let lines = model::lines(home);
    let height = usize::from(area.height);
    let selected = lines
        .iter()
        .position(|line| line.row().as_ref() == model.selected.as_ref());
    // Keep the selection in view, at the bottom when it went past it.
    let offset = selected.map_or(0, |at| (at + 1).saturating_sub(height));
    for (index, line) in lines.iter().enumerate().skip(offset).take(height) {
        let y = area.y + u16::try_from(index - offset).unwrap_or(0);
        let rect = Rect::new(area.x, y, area.width, 1);
        let is_selected = Some(index) == selected;
        let text = list_line(model, home, line, is_selected, usize::from(area.width));
        let flashed = match line {
            model::Line::Device(device) => model
                .flashes
                .get(&device.id)
                .map(|at| model.now.duration_since(*at)),
            _ => None,
        };
        // A change from elsewhere lights the row up, then lets it fade.
        let style = match flashed {
            Some(since) if since < FLASH / 3 => Style::new().fg(Color::Black).bg(WARN),
            Some(since) if since < FLASH => {
                Style::new()
                    .fg(WARN)
                    .bg(if is_selected { SELECTED } else { Color::Reset })
            }
            _ if is_selected => Style::new().bg(SELECTED),
            _ => Style::new(),
        };
        frame.render_widget(Paragraph::new(text).style(style), rect);
        if let Some(row) = line.row() {
            spots.push(Hotspot {
                area: rect,
                target: Target::Row(row.clone()),
            });
            if let model::Line::Device(_) = line {
                spots.push(Hotspot {
                    area: Rect::new(area.x + 3, y, 3, 1),
                    target: Target::Toggle(row),
                });
            }
        }
    }
}

fn list_line(
    model: &Model,
    home: &Home,
    line: &model::Line<'_>,
    selected: bool,
    width: usize,
) -> Line<'static> {
    let mark = if selected {
        Span::styled("▸ ", BOLD.fg(ACCENT))
    } else {
        Span::raw("  ")
    };
    match *line {
        model::Line::Heading(text) => Line::styled(format!("  {text}"), DIM),
        model::Line::Room(room) => {
            let devices: Vec<&Device> = home
                .devices_in(&room.id)
                .filter(|d| matches!(d.kind(), DeviceKind::Light | DeviceKind::Outlet))
                .collect();
            let on = devices
                .iter()
                .filter(|d| d.attributes.is_on() == Some(true))
                .count();
            let mut right: Vec<Span<'static>> = devices.iter().map(|d| dot(d)).collect();
            right.push(Span::styled(format!("  {on} of {} on", devices.len()), DIM));
            spread(
                vec![
                    mark,
                    Span::styled(room.name.clone(), BOLD.fg(room_color(room))),
                ],
                right,
                width,
            )
        }
        model::Line::Device(device) => {
            let name = fit(device.name(), 18);
            let on = device.attributes.is_on() == Some(true);
            let left = vec![
                Span::raw("  "),
                mark,
                identify_dot(model, device),
                Span::raw(" "),
                Span::styled(
                    name.trim_end().to_owned(),
                    if on { Style::new() } else { DIM },
                ),
            ];
            spread(left, device_state(model, device), width)
        }
        model::Line::Scene(scene) => {
            let left = vec![
                Span::raw("  "),
                mark,
                Span::styled("▶ ", Style::new().fg(PRESS)),
                Span::raw(scene.name().to_owned()),
            ];
            let left_for = model
                .scene_runs
                .get(&scene.id)
                .map(|until| until.duration_since(model.now).as_secs().max(0));
            let right = match left_for {
                Some(seconds) => vec![Span::styled(
                    format!("ran · undo {seconds} s"),
                    Style::new().fg(WARN),
                )],
                None => vec![Span::styled(count(scene.actions.len(), "action"), DIM)],
            };
            spread(left, right, width)
        }
        model::Line::Remote(remote) => {
            let left = vec![
                Span::raw("  "),
                mark,
                Span::raw(fit(remote.name(), 18).trim_end().to_owned()),
            ];
            let mut right = Vec::new();
            if let Some(level) = remote.attributes.battery_percentage() {
                right.extend(battery(level, 5));
                let style = if level < LOW_BATTERY {
                    Style::new().fg(WARN)
                } else {
                    Style::new()
                };
                right.push(Span::styled(format!(" {level:>3} %"), style));
                right.push(Span::styled(
                    if level < LOW_BATTERY { " ⚠" } else { "  " },
                    Style::new().fg(WARN),
                ));
            }
            spread(left, right, width)
        }
    }
}

/// The right side of a device's line: on or off, and a light's color and brightness.
fn device_state(model: &Model, device: &Device) -> Vec<Span<'static>> {
    if device.is_reachable == Some(false) {
        return vec![Span::styled("unreachable", Style::new().fg(WARN))];
    }
    let a = &device.attributes;
    match a.is_on() {
        Some(true) if device.controls().brightness => {
            let mut spans = Vec::new();
            if model.colors
                && let Some(color) = theme::light(device)
            {
                spans.push(Span::styled("■ ", Style::new().fg(rgb(color))));
            }
            if let Some(level) = a.light_level() {
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "a percentage, clamped to 1-100 first"
                )]
                let moving = bar_level(model, device, level).round().clamp(1.0, 100.0) as usize;
                let bar = BARS[(moving - 1) * (BARS.len() - 1) / 99];
                spans.push(Span::styled(bar.to_string(), Style::new().fg(ACCENT)));
                spans.push(Span::raw(format!(" {level:>3} %")));
            }
            spans
        }
        Some(true) => vec![Span::styled("on", Style::new().fg(GOOD))],
        Some(false) => vec![Span::styled("off", DIM)],
        None => vec![],
    }
}

/// Where a brightness bar is drawn: running to `level` after a change, else at it.
fn bar_level(model: &Model, device: &Device, level: u8) -> f64 {
    model
        .motions
        .get(&device.id)
        .filter(|motion| motion.running(model.now))
        .map_or(f64::from(level), |motion| motion.value(model.now))
}

/// The bars of the selected light the wheel can turn, top to bottom.
fn wheels(model: &Model) -> Vec<Wheel> {
    let Some(Row::Device(id)) = &model.selected else {
        return vec![];
    };
    let Some(device) = model.device(id).filter(|d| d.kind() == DeviceKind::Light) else {
        return vec![];
    };
    let controls = device.controls();
    let mut wheels = Vec::new();
    if controls.brightness {
        wheels.push(Wheel::Brightness);
    }
    if controls.white.is_some() {
        wheels.push(Wheel::White);
    }
    if controls.color {
        wheels.extend([Wheel::Hue, Wheel::Saturation]);
    }
    wheels
}

fn selected_title(model: &Model) -> (String, String) {
    let Some(home) = &model.home else {
        return ("detail".into(), String::new());
    };
    match &model.selected {
        Some(Row::Device(id)) => home.device(id).map_or_else(
            || ("detail".into(), String::new()),
            |device| {
                let kind = match device.kind() {
                    DeviceKind::Light => "light",
                    DeviceKind::Outlet => "outlet",
                    DeviceKind::Controller => "remote",
                    DeviceKind::Gateway => "hub",
                    DeviceKind::Other => "device",
                };
                (device.name().to_owned(), kind.to_owned())
            },
        ),
        Some(Row::Room(id)) => (
            home.room(id).map_or("room", |r| r.name.as_str()).to_owned(),
            "room".into(),
        ),
        Some(Row::Scene(id)) => (
            home.scene(id).map_or("scene", Scene::name).to_owned(),
            "scene".into(),
        ),
        None => ("detail".into(), String::new()),
    }
}

fn label(text: &str) -> Span<'static> {
    Span::styled(fit(text, LABEL), DIM)
}

fn row(name: &str, mut value: Vec<Span<'static>>) -> Line<'static> {
    value.insert(0, label(name));
    Line::from(value)
}

fn pill(model: &Model, device: &Device, on: bool) -> Span<'static> {
    // Switched from here a moment ago: the state lights up.
    let flashing = model
        .switched
        .get(&device.id)
        .is_some_and(|at| model.now.duration_since(*at) < SWITCH_FLASH);
    match (on, flashing) {
        (true, true) => Span::styled(" ON ", BOLD.fg(Color::Black).bg(BRIGHT)),
        (true, false) => Span::styled(" ON ", BOLD.fg(Color::Black).bg(GOOD)),
        (false, true) => Span::styled(" OFF ", BOLD.fg(Color::Black).bg(BRIGHT)),
        (false, false) => Span::styled(" OFF ", BOLD.bg(MUTED)),
    }
}

/// Width of a bar in a detail pane `width` columns wide: what the label and value leave.
fn bar_width(width: usize) -> usize {
    width.saturating_sub(LABEL + VALUE).clamp(8, 64)
}

fn bar(share: f64, width: usize, color: Color, lit: bool) -> Vec<Span<'static>> {
    let filled = share_of(share, width);
    let style = if lit { Style::new().fg(color) } else { DIM };
    vec![
        Span::styled("━".repeat(filled), style),
        Span::styled("─".repeat(width - filled), DIM),
    ]
}

/// `share` of `width` cells, rounded.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "a share of a few dozen cells"
)]
fn share_of(share: f64, width: usize) -> usize {
    ((share.clamp(0.0, 1.0) * width as f64).round() as usize).min(width)
}

/// A line in the colors from `color_at(0)` to `color_at(1)`, with a dot at `mark`.
fn gradient(
    width: usize,
    mark: f64,
    colors: bool,
    color_at: impl Fn(f64) -> Rgb,
) -> Vec<Span<'static>> {
    let at = share_of(mark, width.saturating_sub(1));
    (0..width)
        .map(|cell| {
            if cell == at {
                return Span::styled("●", BOLD.fg(BRIGHT));
            }
            if !colors {
                return Span::styled("─", DIM);
            }
            #[allow(clippy::cast_precision_loss, reason = "a few dozen cells")]
            let share = cell as f64 / (width.max(2) - 1) as f64;
            Span::styled("━", Style::new().fg(rgb(color_at(share))))
        })
        .collect()
}

fn detail(model: &Model, width: usize) -> Vec<Line<'static>> {
    let shown = model.shown_home();
    let Some(home) = &shown else {
        return vec![];
    };
    match &model.selected {
        Some(Row::Device(id)) => {
            home.device(id)
                .map_or_else(Vec::new, |device| match device.kind() {
                    DeviceKind::Light => light_detail(model, device, width),
                    DeviceKind::Controller => remote_detail(model, home, device),
                    _ => outlet_detail(model, home, device),
                })
        }
        Some(Row::Room(id)) => home
            .room(id)
            .map_or_else(Vec::new, |room| room_detail(model, home, room)),
        Some(Row::Scene(id)) => home
            .scene(id)
            .map_or_else(Vec::new, |scene| scene_detail(model, home, scene)),
        None => vec![Line::styled("nothing selected", DIM)],
    }
}

fn header(device: &Device, lead: Option<Span<'static>>) -> Line<'static> {
    let mut spans = Vec::new();
    if let Some(lead) = lead {
        spans.push(lead);
        spans.push(Span::raw("   "));
    }
    let mut parts = Vec::new();
    if let Some(room) = &device.room {
        parts.push(room.name.clone());
    }
    if let Some(model) = device.attributes.model() {
        parts.push(model.to_owned());
    }
    if let Some((first, rest)) = parts.split_first() {
        spans.push(Span::raw(first.clone()));
        for part in rest {
            spans.push(Span::styled(" · ", DIM));
            spans.push(Span::styled(part.clone(), DIM));
        }
    }
    Line::from(spans)
}

fn seen(model: &Model, device: &Device) -> Option<Line<'static>> {
    (device.is_reachable == Some(false)).then(|| {
        let ago = device
            .seen_at()
            .map(|at| format!(" · seen {}", theme::ago(at, model.now)))
            .unwrap_or_default();
        Line::styled(format!("unreachable{ago}"), Style::new().fg(WARN))
    })
}

fn light_detail(model: &Model, device: &Device, width: usize) -> Vec<Line<'static>> {
    let a = &device.attributes;
    let controls = device.controls();
    let on = a.is_on() == Some(true);
    let bars = bar_width(width);
    let mode = a.color_mode();
    let mut lines = vec![
        header(device, Some(pill(model, device, on))),
        Line::default(),
    ];
    if controls.brightness {
        let level = a.light_level().unwrap_or(0);
        let mut value = bar(bar_level(model, device, level) / 100.0, bars, ACCENT, on);
        value.push(Span::raw(format!("{:>VALUE$}", format!("{level} %"))));
        lines.push(row("brightness", value));
    }
    if let Some(range) = controls.white {
        let kelvin = a.color_temperature().unwrap_or(range.warmest);
        let mark = f64::from(kelvin.saturating_sub(range.warmest))
            / f64::from((range.coolest - range.warmest).max(1));
        let mut value = gradient(bars, mark, model.colors, |share| white_at(range, share));
        let style = if mode == Some(ColorMode::Temperature) {
            Style::new()
        } else {
            DIM
        };
        value.push(Span::styled(
            format!("{:>VALUE$}", format!("{kelvin} K")),
            style,
        ));
        lines.push(row("white", value));
    }
    if controls.color {
        let hue = a.color_hue().unwrap_or(0.0);
        let saturation = a.color_saturation().unwrap_or(0.0);
        let shown = if mode == Some(ColorMode::Color) {
            Style::new()
        } else {
            DIM
        };
        let mut value = gradient(bars, hue / 359.0, model.colors, |share| {
            theme::hsv(share * 359.0, 0.9)
        });
        value.push(Span::styled(
            format!("{:>VALUE$}", format!("{hue:.0}°")),
            shown,
        ));
        lines.push(row("color", value));
        let mut value = bar(saturation, bars, PRESS, on);
        value.push(Span::styled(
            format!("{:>VALUE$}", format!("{:.0} %", saturation * 100.0)),
            shown,
        ));
        lines.push(row("saturation", value));
    }
    lines.push(Line::default());
    lines.push(seen(model, device).unwrap_or_else(|| {
        let mut parts = Vec::new();
        match mode {
            Some(ColorMode::Color) => parts.push("mode color".to_owned()),
            Some(ColorMode::Temperature) => parts.push("mode white".to_owned()),
            _ => {}
        }
        if let Some(range) = controls.white {
            parts.push(format!("range {range}"));
        }
        if let Some(firmware) = a.firmware_version() {
            parts.push(format!("fw {firmware}"));
        }
        Line::styled(parts.join("  ·  "), DIM)
    }));
    lines
}

fn white_at(range: TemperatureRange, share: f64) -> Rgb {
    let span = f64::from(range.coolest - range.warmest);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "within the light's kelvin range"
    )]
    let kelvin = (f64::from(range.warmest) + share * span).round() as u32;
    theme::kelvin(kelvin)
}

fn links(home: &Home, device: &Device) -> Vec<String> {
    let ids: Vec<dirigera::DeviceId> = if device.kind() == DeviceKind::Controller {
        home.devices
            .iter()
            .filter(|other| other.remote_links().contains(&device.id))
            .map(|other| other.id.clone())
            .collect()
    } else {
        device.remote_links().to_vec()
    };
    ids.iter()
        .filter_map(|id| home.device(id))
        .map(|d| d.name().to_owned())
        .collect()
}

fn firmware(device: &Device) -> Option<Line<'static>> {
    let a = &device.attributes;
    let version = a.firmware_version()?;
    let state = match a.ota_status() {
        Some("updateAvailable") => "  update available, the hub installs it",
        Some("upToDate") => "  up to date",
        _ => "",
    };
    Some(row(
        "firmware",
        vec![Span::raw(version.to_owned()), Span::styled(state, DIM)],
    ))
}

fn outlet_detail(model: &Model, home: &Home, device: &Device) -> Vec<Line<'static>> {
    let on = device.attributes.is_on() == Some(true);
    let mut lines = vec![
        header(device, Some(pill(model, device, on))),
        Line::default(),
    ];
    lines.push(row("switch", vec![Span::raw("on and off")]));
    let remotes = links(home, device);
    if !remotes.is_empty() {
        lines.push(row("remote", vec![Span::raw(remotes.join(", "))]));
    }
    lines.extend(firmware(device));
    lines.extend(seen(model, device));
    lines
}

fn remote_detail(model: &Model, home: &Home, device: &Device) -> Vec<Line<'static>> {
    let mut lines = vec![header(device, None), Line::default()];
    if let Some(level) = device.attributes.battery_percentage() {
        let mut value = battery(level, 10);
        let low = level < LOW_BATTERY;
        value.push(Span::styled(
            format!("  {level} %"),
            if low { BOLD.fg(WARN) } else { Style::new() },
        ));
        if low {
            value.push(Span::styled("  replace the battery", Style::new().fg(WARN)));
        }
        lines.push(row("battery", value));
    }
    let switches = links(home, device);
    if !switches.is_empty() {
        lines.push(row("switches", vec![Span::raw(switches.join(", "))]));
    }
    let pressed = model.pressed.get(&device.id).map_or_else(
        || Span::styled("none since dirigent started", DIM),
        |at| Span::raw(theme::ago(*at, model.now)),
    );
    lines.push(row("last press", vec![pressed]));
    lines.extend(firmware(device));
    lines.push(Line::default());
    lines.push(Line::styled(
        "read only: the hub reports remotes, it cannot press them",
        DIM,
    ));
    lines
}

fn room_detail(model: &Model, home: &Home, room: &Room) -> Vec<Line<'static>> {
    let devices: Vec<&Device> = home.devices_in(&room.id).collect();
    let switchable: Vec<&&Device> = devices
        .iter()
        .filter(|d| matches!(d.kind(), DeviceKind::Light | DeviceKind::Outlet))
        .collect();
    let on = switchable
        .iter()
        .filter(|d| d.attributes.is_on() == Some(true))
        .count();
    let dimmable = switchable
        .iter()
        .filter(|d| d.controls().brightness)
        .count();
    let mut summary = vec![
        Span::raw(count(switchable.len(), "device")),
        Span::styled(" · ", DIM),
        Span::styled(format!("{on} on"), Style::new().fg(GOOD)),
    ];
    if dimmable > 0 {
        summary.push(Span::styled(" · ", DIM));
        summary.push(Span::styled(
            format!("{} can dim", count(dimmable, "light")),
            DIM,
        ));
    }
    let mut lines = vec![Line::from(summary), Line::default()];
    for device in devices {
        let mut spans = vec![Span::raw("  ")];
        if device.kind() == DeviceKind::Controller {
            spans.push(Span::raw("  "));
        } else {
            spans.push(dot(device));
            spans.push(Span::raw(" "));
        }
        spans.push(Span::raw(fit(device.name(), 16)));
        spans.push(Span::styled(fit(kind_word(device), 8), DIM));
        if device.kind() == DeviceKind::Controller {
            if let Some(level) = device.attributes.battery_percentage() {
                spans.push(Span::styled(format!("battery {level} %"), DIM));
            }
        } else {
            spans.extend(device_state(model, device));
        }
        lines.push(Line::from(spans));
    }
    lines
}

fn kind_word(device: &Device) -> &'static str {
    match device.kind() {
        DeviceKind::Light => "light",
        DeviceKind::Outlet => "outlet",
        DeviceKind::Controller => "remote",
        DeviceKind::Gateway => "hub",
        DeviceKind::Other => "device",
    }
}

fn scene_detail(model: &Model, home: &Home, scene: &Scene) -> Vec<Line<'static>> {
    let kind = match scene.kind.as_deref() {
        Some("userScene") | None => "scene",
        Some("wakeUpScene") => "wake-up scene",
        Some(other) => other,
    };
    let mut lines = vec![
        Line::from(vec![
            Span::raw(kind.to_owned()),
            Span::styled(started(scene), DIM),
        ]),
        Line::default(),
    ];
    for (index, action) in scene.actions.iter().enumerate() {
        let device = action
            .device_id
            .as_ref()
            .and_then(|id| home.device(id))
            .map_or("a device", Device::name);
        let what = match action
            .attributes
            .as_ref()
            .and_then(dirigera::Attributes::is_on)
        {
            Some(true) => "on",
            Some(false) => "off",
            None => "changed",
        };
        let name = if index == 0 { "actions" } else { "" };
        lines.push(row(name, vec![Span::raw(format!("{device} → {what}"))]));
    }
    if let Some(at) = scene.triggered_at() {
        lines.push(row("last run", vec![Span::raw(theme::ago(at, model.now))]));
    }
    if let Some(window) = scene.undo_window() {
        lines.push(row(
            "undo",
            vec![Span::styled(
                format!("possible for {} s after a run", window.as_secs()),
                DIM,
            )],
        ));
    }
    lines
}

/// How a scene starts, from its triggers: " · from the app", " · at set times".
fn started(scene: &Scene) -> String {
    let mut ways: Vec<&str> = scene
        .extra
        .get("triggers")
        .and_then(|triggers| triggers.as_array())
        .into_iter()
        .flatten()
        .filter_map(|trigger| trigger["type"].as_str())
        .map(|kind| match kind {
            "app" => "from the app",
            "time" => "at set times",
            "sunriseSunset" => "at sunrise or sunset",
            "controller" => "by a remote",
            "duration" => "after a while",
            other => other,
        })
        .collect();
    ways.dedup();
    if ways.is_empty() {
        String::new()
    } else {
        format!(" · {}", ways.join(", "))
    }
}

/// Two lines under the list in the narrow layout: what the selected light shows.
fn compact_detail(model: &Model, width: usize) -> Vec<Line<'static>> {
    let Some(Row::Device(id)) = &model.selected else {
        return vec![];
    };
    let shown = model.shown_home();
    let Some(device) = shown
        .as_ref()
        .and_then(|home| home.device(id))
        .filter(|d| d.kind() == DeviceKind::Light)
    else {
        return vec![];
    };
    let a = &device.attributes;
    let on = a.is_on() == Some(true);
    let mut lines = vec![Line::styled("─".repeat(width), DIM)];
    if let Some(level) = a.light_level() {
        let mut value = bar(
            f64::from(level) / 100.0,
            bar_width(width).min(20),
            ACCENT,
            on,
        );
        value.push(Span::raw(format!("  {level} %")));
        lines.push(row("brightness", value));
    }
    let mut spans = Vec::new();
    match a.color_mode() {
        Some(ColorMode::Color) => {
            if let (Some(hue), Some(saturation)) = (a.color_hue(), a.color_saturation()) {
                if model.colors
                    && let Some(color) = theme::light(device)
                {
                    spans.push(Span::styled("■ ", Style::new().fg(rgb(color))));
                }
                spans.push(Span::raw(format!("{hue:.0}° {:.0} %", saturation * 100.0)));
                lines.push(row("color", spans));
            }
        }
        _ => {
            if let Some(kelvin) = a.color_temperature() {
                lines.push(row("white", vec![Span::raw(format!("{kelvin} K"))]));
            }
        }
    }
    lines
}

fn activity(model: &Model, width: usize) -> Vec<Line<'static>> {
    model
        .activity
        .iter()
        .map(|entry| activity_line(model, entry, width))
        .collect()
}

fn activity_line(model: &Model, entry: &Entry, width: usize) -> Line<'static> {
    let time = entry
        .at
        .to_zoned(model.tz.clone())
        .strftime("%H:%M:%S")
        .to_string();
    let (icon, color) = match entry.tone {
        Tone::Good => ("✓", GOOD),
        Tone::Info => ("●", ACCENT),
        Tone::Warn => ("⚠", WARN),
        Tone::Bad => ("✗", BAD),
        Tone::Press => ("◆", PRESS),
        Tone::Pending => (spinner_frame(model), ACCENT),
    };
    let right = entry
        .source
        .clone()
        .map(|source| vec![Span::styled(source, DIM)])
        .unwrap_or_default();
    // Time, icon and name take 27 columns; the text gets what the source leaves.
    let source: usize = right.iter().map(Span::width).sum();
    let room = width.saturating_sub(27 + source + usize::from(source > 0));
    let left = vec![
        Span::styled(time, DIM),
        Span::raw("  "),
        Span::styled(icon, Style::new().fg(color)),
        Span::raw(" "),
        Span::raw(format!("{} ", fit(&entry.who, 14))),
        Span::raw(fit(&entry.what, room).trim_end().to_owned()),
    ];
    spread(left, right, width)
}

fn spinner_frame(model: &Model) -> &'static str {
    const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let frame = usize::try_from(model.now.as_millisecond() / 80).unwrap_or(0) % SPINNER.len();
    FRAMES[frame]
}

fn spinner(model: &Model) -> Span<'static> {
    Span::styled(spinner_frame(model), Style::new().fg(ACCENT))
}

/// A box in the middle of `area` with `text`.
fn dialog(frame: &mut Frame, area: Rect, title: &str, text: Vec<Line<'static>>) {
    let width = text.iter().map(Line::width).max().unwrap_or(0) + 4;
    let width = u16::try_from(width)
        .unwrap_or(u16::MAX)
        .min(area.width.saturating_sub(2));
    let height = u16::try_from(text.len() + 2)
        .unwrap_or(u16::MAX)
        .min(area.height);
    let popup = area.centered(Constraint::Length(width), Constraint::Length(height));
    frame.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(ACCENT))
        .title(Span::styled(title.to_owned(), BOLD.fg(ACCENT)))
        .padding(Padding::horizontal(1));
    frame.render_widget(Paragraph::new(text).block(block), popup);
}

fn pairing(model: &Model, state: &Dialog, frame: &mut Frame, body: Rect) {
    let mut text = Vec::new();
    match state {
        Dialog::Searching { reason } => {
            if let Some(reason) = reason {
                text.push(Line::styled(reason.clone(), Style::new().fg(WARN)));
                text.push(Line::default());
            }
            text.push(Line::from(vec![
                spinner(model),
                Span::raw(" looking for the hub on the network"),
            ]));
        }
        Dialog::Pressing {
            hub,
            address,
            until,
        } => {
            text.push(Line::from(vec![
                Span::styled("✓", Style::new().fg(GOOD)),
                Span::raw(" found "),
                Span::styled(hub.clone(), BOLD),
                Span::styled(format!(" at {address}"), DIM),
            ]));
            let left = until.duration_since(model.now).as_secs().max(0);
            text.push(Line::from(vec![
                spinner(model),
                Span::raw(" press the action button on the bottom of the hub"),
                Span::styled(format!("   {left} s"), Style::new().fg(WARN)),
            ]));
        }
        Dialog::Failed(message) => {
            text.push(Line::styled(format!("✗ {message}"), Style::new().fg(BAD)));
        }
    }
    text.push(Line::default());
    text.push(Line::styled(
        "the token stays on this computer; `dirigent unpair` removes it from the hub",
        DIM,
    ));
    dialog(frame, body, " pair with a hub ", text);
}

fn statusline(model: &Model, frame: &mut Frame, area: Rect) {
    let (label, color) = match (&model.phase, model.link) {
        (Phase::Pairing(_), _) => ("PAIRING", PRESS),
        (Phase::Unreachable { .. }, _) | (Phase::Ready, Link::Offline { .. }) => ("OFFLINE", BAD),
        (Phase::Ready, Link::Live) => ("LIVE", GOOD),
        _ => ("CONNECTING", WARN),
    };
    let name = model
        .hub_name
        .clone()
        .unwrap_or_else(|| "dirigent".to_owned());
    // Each segment has the rank it is dropped in when the window is narrow: highest first.
    let mut segments: Vec<(u8, Vec<Span<'static>>)> = Vec::new();
    if let (Phase::Ready, Link::Offline { retry_at }) = (&model.phase, model.link) {
        let left = retry_at.duration_since(model.now).as_secs().max(0);
        segments.push((
            0,
            vec![Span::styled(
                format!("○ reconnecting in {left} s"),
                Style::new().fg(BAD),
            )],
        ));
    }
    if let Some(home) = &model.home {
        for remote in home
            .devices
            .iter()
            .filter(|d| d.kind() == DeviceKind::Controller)
        {
            if let Some(level) = remote
                .attributes
                .battery_percentage()
                .filter(|&l| l < LOW_BATTERY)
            {
                let text = format!("⚠ {} battery {level} %", remote.name());
                segments.push((1, vec![Span::styled(text, Style::new().fg(WARN))]));
            }
        }
    }
    if let Some(address) = model.address {
        segments.push((2, vec![Span::raw(address.ip().to_string())]));
    }
    if let Some(home) = &model.home {
        segments.push((3, vec![Span::raw(on_count(model))]));
        if let Some(firmware) = home
            .hub
            .as_ref()
            .and_then(|hub| hub.attributes.firmware_version())
        {
            segments.push((4, vec![Span::styled(format!("fw {firmware}"), DIM)]));
        }
    }
    let line = |segments: &[(u8, Vec<Span<'static>>)]| {
        let mut spans = vec![
            Span::styled(format!(" {label} "), BOLD.fg(Color::Black).bg(color)),
            Span::styled(format!(" {name} "), BOLD.bg(MUTED)),
        ];
        for (index, (_, segment)) in segments.iter().enumerate() {
            spans.push(if index == 0 {
                " ".into()
            } else {
                Span::styled(" │ ", DIM)
            });
            spans.extend(segment.iter().cloned());
        }
        Line::from(spans)
    };
    let width = usize::from(area.width);
    while line(&segments).width() > width {
        let Some(least) = (0..segments.len()).max_by_key(|&i| segments[i].0) else {
            break;
        };
        segments.remove(least);
    }
    frame.render_widget(line(&segments), area);
}

/// The hints the selection allows, and what space does to it.
fn allowed(model: &Model) -> (Vec<Hint>, Option<&'static str>) {
    let Some(home) = &model.home else {
        return (vec![], None);
    };
    match &model.selected {
        Some(Row::Device(id)) => {
            let Some(device) = home.device(id) else {
                return (vec![], None);
            };
            let controls = device.controls();
            let mut hints = Vec::new();
            for (allows, hint) in [
                (controls.power, Hint::Activate),
                (controls.brightness, Hint::Dim),
                (controls.white.is_some(), Hint::White),
                (controls.color || controls.white.is_some(), Hint::Color),
                (
                    matches!(device.kind(), DeviceKind::Light | DeviceKind::Outlet),
                    Hint::Identify,
                ),
            ] {
                if allows {
                    hints.push(hint);
                }
            }
            (hints, Some("on/off"))
        }
        Some(Row::Room(id)) => {
            let mut hints = vec![Hint::Activate];
            if home.devices_in(id).any(|d| d.controls().brightness) {
                hints.push(Hint::Dim);
            }
            (hints, Some("room on/off"))
        }
        Some(Row::Scene(_)) => (vec![Hint::Activate], Some("run")),
        None => (vec![], None),
    }
}

fn hint_bar(model: &Model, frame: &mut Frame, area: Rect, shape: Shape) {
    let (allows, activate) = allowed(model);
    // (rank, keys, label); a lower rank stays longer when the window is narrow.
    let hints: Vec<(u8, &str, &str)> = match (&model.overlay, &model.phase) {
        (Overlay::Help, _) => vec![(0, "?", "close"), (0, "Esc", "close")],
        _ if model.picker.is_some() => vec![
            (0, "↑↓", "row"),
            (0, "←→", "change"),
            (2, "⇧", "fine"),
            (0, "⏎", "keep"),
            (0, "Esc", "back"),
        ],
        (_, Phase::Pairing(Dialog::Failed(_))) => vec![(0, "⏎", "try again"), (0, "q", "quit")],
        (_, Phase::Pairing(_)) => vec![(0, "Esc", "cancel"), (0, "q", "quit")],
        (_, Phase::Unreachable { .. }) => vec![(0, "⏎", "try now"), (0, "q", "quit")],
        _ => BINDINGS
            .iter()
            .filter(|b| match b.hint {
                Hint::Always => true,
                Hint::Tabs => shape == Shape::Tabs,
                Hint::Undo => !model.undo.is_empty(),
                Hint::Never => false,
                hint => allows.contains(&hint),
            })
            .map(|b| {
                let label = match b.hint {
                    Hint::Activate => activate.unwrap_or(b.label),
                    _ => b.label,
                };
                (b.rank, b.short, label)
            })
            .collect(),
    };
    let version = match &model.notice {
        Some(notice) if notice.until > model.now => {
            let color = match notice.tone {
                Tone::Good => GOOD,
                Tone::Bad => BAD,
                Tone::Warn => WARN,
                _ => Color::Reset,
            };
            Span::styled(format!("{} ", notice.text), Style::new().fg(color))
        }
        _ => Span::styled(concat!("dirigent ", env!("CARGO_PKG_VERSION"), " "), DIM),
    };
    let line = |hints: &[(u8, &str, &str)]| {
        let mut left = vec![Span::raw(" ")];
        for (index, (_, key, label)) in hints.iter().enumerate() {
            if index > 0 {
                left.push(Span::raw("  "));
            }
            left.push(Span::styled((*key).to_owned(), BOLD));
            left.push(Span::raw(format!(" {label}")));
        }
        spread(left, vec![version.clone()], usize::from(area.width))
    };
    let mut hints = hints;
    while line(&hints).width() > usize::from(area.width) {
        let Some(least) = (0..hints.len()).max_by_key(|&i| hints[i].0) else {
            break;
        };
        hints.remove(least);
    }
    frame.render_widget(line(&hints), area);
}

fn picker(model: &Model, open: &Picker, frame: &mut Frame, area: Rect) {
    let Some(device) = model
        .device(&open.device)
        .map(|d| model.shown(d).into_owned())
    else {
        return;
    };
    let controls = device.controls();
    let mut can = Vec::new();
    if controls.color {
        can.push("any hue and saturation".to_owned());
    }
    if let Some(range) = controls.white {
        can.push(format!("white {range}"));
    }
    let footer = format!("{} · the light follows right away", can.join(", or "));
    // As wide as the note under it, so the bars end with it.
    let width = footer
        .chars()
        .count()
        .clamp(56, usize::from(area.width.saturating_sub(8)).max(40));
    let mut text = preset_lines(open, &device, width);
    text.push(Line::default());
    text.extend(picker_rows(model, open, &device, width));
    text.push(Line::default());
    text.push(Line::styled(footer, DIM));
    dialog(frame, area, &format!(" color · {} ", device.name()), text);
}

/// The marker and label of a picker row; the row `←→` changes stands out.
fn picker_head(open: &Picker, row: PickerRow, name: &str) -> Vec<Span<'static>> {
    if row == open.row {
        vec![
            Span::styled("▸ ", BOLD.fg(ACCENT)),
            Span::styled(fit(name, LABEL), BOLD.fg(ACCENT)),
        ]
    } else {
        vec![Span::raw("  "), Span::styled(fit(name, LABEL), DIM)]
    }
}

/// The presets with their swatches, wrapped to `width`; the one `←→` is on stands out.
fn preset_lines(open: &Picker, device: &Device, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut line = picker_head(open, PickerRow::Presets, "presets");
    let mut used = 2 + LABEL;
    for (index, (name, change)) in presets(device).into_iter().enumerate() {
        let swatch = match change {
            dirigera::Change::White(kelvin) => theme::kelvin(kelvin),
            dirigera::Change::Color { hue, saturation } => theme::hsv(hue, saturation),
            _ => theme::Rgb(255, 255, 255),
        };
        let chip = 2 + name.chars().count() + 2;
        if used + chip > width {
            let indent = vec![Span::raw(" ".repeat(2 + LABEL))];
            lines.push(Line::from(std::mem::replace(&mut line, indent)));
            used = 2 + LABEL;
        }
        let picked = open.row == PickerRow::Presets && index == open.preset;
        line.push(Span::styled("■ ", Style::new().fg(rgb(swatch))));
        line.push(if picked {
            Span::styled(name, BOLD.fg(Color::Black).bg(BRIGHT))
        } else {
            Span::raw(name)
        });
        line.push(Span::raw("  "));
        used += chip;
    }
    lines.push(Line::from(line));
    lines
}

/// Hue, saturation and white as bars under the presets, for what the light can show.
fn picker_rows(model: &Model, open: &Picker, device: &Device, width: usize) -> Vec<Line<'static>> {
    let a = &device.attributes;
    let mode = a.color_mode();
    let bars = width.saturating_sub(2 + LABEL + VALUE).max(8);
    let shown = |on: bool| if on { Style::new() } else { DIM };
    let hue = a.color_hue().unwrap_or(0.0);
    let saturation = a.color_saturation().unwrap_or(0.0);
    let mut lines = Vec::new();
    for row in Picker::rows(device) {
        let (name, mut bar, value, lit) = match row {
            PickerRow::Presets => continue,
            PickerRow::Hue => (
                "hue",
                gradient(bars, hue / 359.0, model.colors, |share| {
                    theme::hsv(share * 359.0, 0.9)
                }),
                format!("{hue:.0}°"),
                mode == Some(ColorMode::Color),
            ),
            PickerRow::Saturation => (
                "saturation",
                gradient(bars, saturation, model.colors, |share| {
                    theme::hsv(hue, share)
                }),
                format!("{:.0} %", saturation * 100.0),
                mode == Some(ColorMode::Color),
            ),
            PickerRow::White => {
                let Some(range) = device.controls().white else {
                    continue;
                };
                let kelvin = a.color_temperature().unwrap_or(range.warmest);
                let mark = f64::from(kelvin.saturating_sub(range.warmest))
                    / f64::from((range.coolest - range.warmest).max(1));
                (
                    "white",
                    gradient(bars, mark, model.colors, |share| white_at(range, share)),
                    format!("{kelvin} K"),
                    mode == Some(ColorMode::Temperature),
                )
            }
        };
        let mut spans = picker_head(open, row, name);
        spans.append(&mut bar);
        spans.push(Span::styled(format!("{value:>VALUE$}"), shown(lit)));
        lines.push(Line::from(spans));
    }
    lines
}

fn help(frame: &mut Frame, area: Rect) {
    let mut text: Vec<Line<'static>> = BINDINGS
        .iter()
        .filter(|binding| !binding.keys.is_empty())
        .map(|binding| {
            Line::from(vec![
                Span::styled(fit(binding.keys, 10), BOLD),
                Span::raw(binding.help),
            ])
        })
        .collect();
    text.push(Line::default());
    text.push(Line::styled("mouse: a click selects a row or a tab", DIM));
    dialog(frame, area, " keys ", text);
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::KeyCode;
    use serde_json::json;

    use super::*;
    use crate::tui::model::tests::{LAMP, REMOTE, STRIP, connected, event, key, live, now, start};
    use crate::tui::model::{Failure, FailureKind, Msg, Pairing};

    fn screen(model: &Model, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                render(model, frame);
            })
            .unwrap();
        terminal.backend().to_string()
    }

    fn select(model: &mut Model, id: &str) {
        model.update(Msg::Select(Row::Device(dirigera::DeviceId(id.to_owned()))));
    }

    /// Live, with changes from a remote and from elsewhere in the activity.
    fn lived_in() -> Model {
        let mut model = live();
        model.update(event(&json!({
            "type": "remotePressEvent",
            "time": "2026-01-01T12:00:05.000Z",
            "data": { "id": REMOTE, "clickPattern": "singlePress" },
        })));
        model.update(event(&json!({
            "type": "deviceStateChanged",
            "time": "2026-01-01T12:00:05.300Z",
            "data": { "id": LAMP, "attributes": { "isOn": true } },
        })));
        model.update(event(&json!({
            "type": "deviceStateChanged",
            "time": "2026-01-01T12:01:10.000Z",
            "data": { "id": STRIP, "attributes": { "lightLevel": 40 } },
        })));
        model.update(Msg::Tick(now() + jiff::SignedDuration::from_secs(90)));
        model
    }

    #[test]
    fn wide_layout_with_a_light() {
        insta::assert_snapshot!(screen(&lived_in(), 110, 26));
    }

    #[test]
    fn details_of_an_outlet_a_room_a_scene_and_a_remote() {
        let mut model = lived_in();
        select(&mut model, LAMP);
        insta::assert_snapshot!("outlet", screen(&model, 110, 26));
        model.update(key(KeyCode::Up));
        insta::assert_snapshot!("room", screen(&model, 110, 26));
        model.update(key(KeyCode::Down));
        model.update(key(KeyCode::Down));
        insta::assert_snapshot!("scene", screen(&model, 110, 26));
        select(&mut model, REMOTE);
        insta::assert_snapshot!("remote", screen(&model, 110, 26));
    }

    #[test]
    fn narrow_layout_tabs() {
        let mut model = lived_in();
        insta::assert_snapshot!("tab_devices", screen(&model, 64, 22));
        model.update(key(KeyCode::Char('2')));
        insta::assert_snapshot!("tab_activity", screen(&model, 64, 22));
    }

    #[test]
    fn too_small_a_window() {
        insta::assert_snapshot!(screen(&lived_in(), 40, 12));
    }

    #[test]
    fn help_overlay() {
        let mut model = lived_in();
        model.update(key(KeyCode::Char('?')));
        insta::assert_snapshot!(screen(&model, 110, 26));
    }

    #[test]
    fn lost_connection() {
        let mut model = lived_in();
        model.update(Msg::Lost("the hub closed the event stream".into()));
        insta::assert_snapshot!(screen(&model, 110, 26));
    }

    #[test]
    fn pairing_dialog() {
        let (mut model, _) = Model::start(start());
        model.update(Msg::Connected(Err(Failure {
            kind: FailureKind::PairAgain,
            message: "the hub no longer accepts dirigent".into(),
        })));
        insta::assert_snapshot!("pairing_searching", screen(&model, 110, 26));
        model.update(Msg::Pairing(Pairing::Pressing {
            hub: "gw2-0123456789ab".into(),
            address: "192.168.1.20:8443".parse().unwrap(),
        }));
        model.update(Msg::Tick(now() + jiff::SignedDuration::from_secs(8)));
        insta::assert_snapshot!("pairing_pressing", screen(&model, 110, 26));
        model.update(Msg::Pairing(Pairing::Failed(
            "the button was not pressed within 120 s".into(),
        )));
        insta::assert_snapshot!("pairing_failed", screen(&model, 110, 26));
        model.update(key(KeyCode::Enter));
        model.update(Msg::Pairing(Pairing::Done(Box::new(connected()))));
        assert_eq!(model.phase, Phase::Ready);
    }

    #[test]
    fn a_change_on_its_way_and_confirmed() {
        let mut model = lived_in();
        model.update(key(KeyCode::Char(' ')));
        insta::assert_snapshot!("change_on_its_way", screen(&model, 110, 26));
        let later = model.now + jiff::SignedDuration::from_millis(130);
        let effects = model.update(Msg::Tick(later));
        let Some(model::Effect::Send { batch, .. }) = effects.first() else {
            panic!("{effects:?}");
        };
        model.update(Msg::Sent {
            batch: *batch,
            result: Ok(()),
        });
        model.update(Msg::Tick(later + jiff::SignedDuration::from_millis(250)));
        model.update(event(&json!({
            "type": "deviceStateChanged",
            "data": { "id": STRIP, "attributes": { "isOn": false } },
        })));
        insta::assert_snapshot!("change_confirmed", screen(&model, 110, 26));
    }

    #[test]
    fn color_picker() {
        let mut model = lived_in();
        model.update(key(KeyCode::Char('c')));
        model.update(key(KeyCode::Down));
        insta::assert_snapshot!(screen(&model, 110, 26));
    }

    #[test]
    fn a_notice_says_what_cannot_be_done() {
        let mut model = lived_in();
        select(&mut model, LAMP);
        model.update(key(KeyCode::Right));
        insta::assert_snapshot!(screen(&model, 110, 26));
    }

    #[test]
    fn a_scene_counts_down_its_undo_window() {
        let mut model = lived_in();
        let scene = dirigera::SceneId("a1b2c3d4-0000-4000-8000-0000000000b1".into());
        model.update(Msg::Select(Row::Scene(scene.clone())));
        model.update(key(KeyCode::Enter));
        model.update(Msg::SceneDone {
            scene,
            undo: false,
            result: Ok(()),
        });
        model.update(Msg::Tick(model.now + jiff::SignedDuration::from_secs(4)));
        insta::assert_snapshot!(screen(&model, 110, 26));
    }

    #[test]
    fn clicks_select_rows_and_tabs() {
        let model = lived_in();
        let mut terminal = Terminal::new(TestBackend::new(64, 22)).unwrap();
        let mut spots = Vec::new();
        terminal
            .draw(|frame| spots = render(&model, frame))
            .unwrap();
        assert!(spots.iter().any(|s| s.target == Target::Tab(Tab::Activity)));
        let lamp = spots
            .iter()
            .find(|s| s.target == Target::Row(Row::Device(dirigera::DeviceId(LAMP.to_owned()))))
            .unwrap();
        assert_eq!(lamp.area.width, 60);
    }
}
