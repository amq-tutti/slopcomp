//! Look and feel: palette, global egui style, and the few custom widgets that make the app feel
//! less stock (step tabs, cards, primary buttons, gradient progress bars, the loading indicator).

use eframe::egui::{
    self, epaint::Mesh, style::WidgetVisuals, Color32, CornerRadius, FontFamily, FontId, Margin, Pos2, Rect, Response, RichText,
    Sense, Shadow, Stroke, TextStyle, Ui, Vec2,
};

// ---------------------------------------------------------------- palette

/// Top bar padding above and below its row; the current tab's underline sits on the bar's bottom edge.
pub const BAR_PAD_Y: f32 = 10.0;

/// Window background.
pub const BG: Color32 = Color32::from_rgb(0x12, 0x13, 0x17);
/// Top bar, slightly darker than the page.
pub const BAR: Color32 = Color32::from_rgb(0x0e, 0x0f, 0x13);
/// Cards.
pub const SURFACE: Color32 = Color32::from_rgb(0x1a, 0x1c, 0x22);
/// Buttons and controls at rest.
pub const SURFACE_2: Color32 = Color32::from_rgb(0x25, 0x27, 0x30);
/// Hovered controls.
pub const SURFACE_3: Color32 = Color32::from_rgb(0x30, 0x33, 0x3e);
/// Inputs and progress tracks: an inset, darker than cards.
pub const INSET: Color32 = Color32::from_rgb(0x0f, 0x10, 0x14);
pub const BORDER: Color32 = Color32::from_rgb(0x2b, 0x2e, 0x38);
pub const TEXT: Color32 = Color32::from_rgb(0xd8, 0xda, 0xe2);
pub const TEXT_BRIGHT: Color32 = Color32::from_rgb(0xf4, 0xf4, 0xf8);
pub const TEXT_WEAK: Color32 = Color32::from_rgb(0x8a, 0x8e, 0x9e);
/// Accent: violet, with a pink partner for gradients.
pub const ACCENT: Color32 = Color32::from_rgb(0x8e, 0x7b, 0xff);
pub const ACCENT_2: Color32 = Color32::from_rgb(0xe8, 0x6f, 0xb4);
pub const ACCENT_SOFT: Color32 = Color32::from_rgb(0x36, 0x30, 0x6e);
pub const OK: Color32 = Color32::from_rgb(0x4c, 0xc3, 0x8a);
pub const WARN: Color32 = Color32::from_rgb(0xe9, 0xb4, 0x4c);
pub const ERR: Color32 = Color32::from_rgb(0xef, 0x6b, 0x6b);

// ---------------------------------------------------------------- global style

pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.style_mut_of(egui::Theme::Dark, |s| {
        s.text_styles = [
            (TextStyle::Heading, FontId::new(19.0, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Small, FontId::new(11.5, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(12.5, FontFamily::Monospace)),
        ]
        .into();

        let sp = &mut s.spacing;
        sp.item_spacing = Vec2::new(8.0, 7.0);
        sp.button_padding = Vec2::new(11.0, 5.0);
        sp.interact_size = Vec2::new(40.0, 26.0);
        sp.indent = 16.0;
        sp.menu_margin = Margin::same(5); // tooltips and dropdowns: the same snug padding on every side
        sp.window_margin = Margin::same(10);
        sp.combo_width = 120.0;

        let v = &mut s.visuals;
        v.dark_mode = true;
        v.panel_fill = BG;
        v.window_fill = SURFACE_2;
        v.window_stroke = Stroke::new(1.0, BORDER);
        v.window_corner_radius = CornerRadius::same(10);
        v.menu_corner_radius = CornerRadius::same(10);
        let shadow = Shadow { offset: [0, 10], blur: 28, spread: 0, color: Color32::from_black_alpha(140) };
        v.window_shadow = shadow;
        v.popup_shadow = shadow;
        v.extreme_bg_color = INSET;
        v.faint_bg_color = Color32::from_rgb(0x17, 0x19, 0x1e);
        v.code_bg_color = INSET;
        v.hyperlink_color = ACCENT;
        v.warn_fg_color = WARN;
        v.error_fg_color = ERR;
        v.selection.bg_fill = ACCENT_SOFT;
        v.selection.stroke = Stroke::new(1.0, Color32::from_rgb(0xe6, 0xe1, 0xff));
        v.indent_has_left_vline = false;
        v.interact_cursor = Some(egui::CursorIcon::PointingHand);
        // Sliders: the travelled part of the rail in the accent colour.
        v.slider_trailing_fill = true;

        let w = |fill: Color32, stroke: Stroke, fg: Color32, r: u8| WidgetVisuals {
            bg_fill: fill,
            weak_bg_fill: fill,
            bg_stroke: stroke,
            corner_radius: CornerRadius::same(r),
            fg_stroke: Stroke::new(1.0, fg),
            expansion: 0.0,
        };
        v.widgets.noninteractive = w(SURFACE, Stroke::new(1.0, BORDER), TEXT, 8);
        v.widgets.inactive = w(SURFACE_2, Stroke::new(1.0, Color32::from_rgb(0x33, 0x36, 0x42)), TEXT, 7);
        v.widgets.hovered = w(SURFACE_3, Stroke::new(1.0, ACCENT.gamma_multiply(0.55)), TEXT_BRIGHT, 7);
        v.widgets.active = w(ACCENT_SOFT, Stroke::new(1.0, ACCENT), TEXT_BRIGHT, 7);
        v.widgets.open = w(SURFACE_3, Stroke::new(1.0, BORDER), TEXT_BRIGHT, 7);
    });
}

// ---------------------------------------------------------------- building blocks

/// A rounded card for a section of a page.
pub fn card() -> egui::Frame {
    egui::Frame::new().fill(SURFACE).stroke(Stroke::new(1.0, BORDER)).corner_radius(10).inner_margin(Margin::same(14))
}

/// A smaller card, for items inside a page (a source, a history entry).
pub fn tile() -> egui::Frame {
    egui::Frame::new().fill(SURFACE).stroke(Stroke::new(1.0, BORDER)).corner_radius(8).inner_margin(Margin::symmetric(12, 10))
}

/// Small, spaced, upper-case label above a section.
pub fn section(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text.to_uppercase()).size(11.5).color(TEXT_WEAK).extra_letter_spacing(1.2));
    ui.add_space(2.0);
}

/// Filled accent button for the main action on a page. Brightens on hover, darkens while
/// pressed, and shows the pointing-hand cursor. Use with `ui.add` / `ui.add_enabled`.
pub fn primary(text: impl Into<String>) -> Primary {
    Primary(text.into())
}

pub struct Primary(String);

impl egui::Widget for Primary {
    fn ui(self, ui: &mut Ui) -> Response {
        let galley = ui.painter().layout_no_wrap(self.0, FontId::proportional(14.5), Color32::WHITE);
        let size = Vec2::new(galley.size().x + 2.0 * ui.spacing().button_padding.x + 4.0, 32.0);
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
        let enabled = ui.is_enabled();
        let resp = if enabled { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp };
        if ui.is_rect_visible(rect) {
            let hover = ui.ctx().animate_bool_with_time(resp.id.with("hover"), enabled && resp.hovered(), 0.1);
            let base = ACCENT.gamma_multiply(0.85);
            let mut fill = mix(base, mix(ACCENT, Color32::WHITE, 0.12), hover);
            if resp.is_pointer_button_down_on() {
                fill = mix(fill, Color32::BLACK, 0.18);
            }
            let p = ui.painter();
            p.rect_filled(rect, 8.0, fill);
            p.rect_stroke(rect, 8.0, Stroke::new(1.0, mix(ACCENT, Color32::WHITE, 0.2 * hover)), egui::StrokeKind::Inside);
            p.galley(rect.center() - galley.size() / 2.0, galley, Color32::WHITE);
        }
        resp
    }
}

/// Linear colour mix.
fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_unmultiplied(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()), l(a.a(), b.a()))
}

/// A horizontal capsule filled with a left-to-right gradient.
fn gradient_capsule(painter: &egui::Painter, rect: Rect, left: Color32, right: Color32) {
    if rect.width() <= 0.5 {
        return;
    }
    let r = (rect.height() / 2.0).min(rect.width() / 2.0);
    let color_at = |x: f32| mix(left, right, (x - rect.left()) / rect.width());
    let mut mesh = Mesh::default();
    let c = rect.center();
    mesh.colored_vertex(c, color_at(c.x));
    let steps = 10;
    let (lc, rc) = (Pos2::new(rect.left() + r, c.y), Pos2::new(rect.right() - r, c.y));
    let mut pts = Vec::new();
    // Right cap from top to bottom, then left cap from bottom to top (clockwise).
    for i in 0..=steps {
        let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * i as f32 / steps as f32;
        pts.push(rc + Vec2::new(a.cos(), a.sin()) * r);
    }
    for i in 0..=steps {
        let a = std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * i as f32 / steps as f32;
        pts.push(lc + Vec2::new(a.cos(), a.sin()) * r);
    }
    for p in &pts {
        mesh.colored_vertex(*p, color_at(p.x));
    }
    let n = pts.len() as u32;
    for i in 0..n {
        mesh.add_triangle(0, 1 + i, 1 + (i + 1) % n);
    }
    painter.add(mesh);
}

// ---------------------------------------------------------------- motion

/// How long things take to appear and disappear.
pub const FADE_SECS: f32 = 0.22;

/// 0..1 that eases towards `on` over `secs` (ease-out, so a change starts at once and settles
/// softly). It starts at 0 the first time an id is seen, so new things fade in as well as out.
pub fn ease(ctx: &egui::Context, id: egui::Id, on: bool, secs: f32) -> f32 {
    let dt = ctx.input(|i| i.stable_dt).min(0.1);
    let target = if on { 1.0 } else { 0.0 };
    let lin = ctx.data_mut(|d| {
        let v = d.get_temp_mut_or_insert_with(id, || 0.0f32);
        let step = dt / secs.max(0.01);
        *v = if *v < target { (*v + step).min(target) } else { (*v - step).max(target) };
        *v
    });
    if lin != target {
        ctx.request_repaint();
    }
    1.0 - (1.0 - lin).powi(3)
}

/// Shows `add` while `visible`, fading it in and out and growing / shrinking its height so
/// what is below slides instead of jumping. `add` is still called while fading out, so it must be
/// able to draw its last state. Returns whether anything was drawn.
pub fn fade(ui: &mut Ui, id_salt: impl std::hash::Hash, visible: bool, add: impl FnOnce(&mut Ui)) -> bool {
    let id = ui.id().with(("fade", id_salt));
    let t = ease(ui.ctx(), id, visible, FADE_SECS);
    if t <= 0.0 {
        return false;
    }
    let h_id = id.with("h");
    let full_h: Option<f32> = ui.ctx().data(|d| d.get_temp(h_id));
    let width = ui.available_width();
    let shown_h = full_h.map(|h| h * t);
    let top = ui.cursor().min;
    let max = Rect::from_min_size(top, Vec2::new(width, shown_h.unwrap_or(f32::INFINITY).max(0.0)));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(top, Vec2::new(width, 1e6))).layout(*ui.layout()));
    child.set_clip_rect(max.intersect(ui.clip_rect()));
    child.set_opacity(t);
    add(&mut child);
    let h = child.min_rect().height();
    ui.ctx().data_mut(|d| d.insert_temp(h_id, h));
    let shown = if t >= 1.0 { h } else { shown_h.unwrap_or(0.0).min(h) };
    // Width scales too, so a card wrapping this grows and shrinks instead of snapping.
    ui.allocate_exact_size(Vec2::new(child.min_rect().width().min(width) * t, shown), Sense::hover());
    true
}

/// Slim progress bar with a label on the left and the percentage on the right. The fill glides
/// to new values and turns from the accent gradient to green when it reaches 100%.
pub fn progress(ui: &mut Ui, id_salt: impl std::hash::Hash, fraction: f32, label: &str, width: f32) -> Response {
    let fraction = fraction.clamp(0.0, 1.0);
    let id = ui.id().with(("progress", id_salt));
    let shown = ui.ctx().animate_value_with_time(id, fraction, 0.35);
    let done = ease(ui.ctx(), id.with("done"), fraction >= 1.0 && shown >= 0.999, 0.4);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 28.0), Sense::hover());
    let painter = ui.painter_at(rect.expand(2.0));
    let font = FontId::proportional(12.5);
    painter.text(rect.left_top(), egui::Align2::LEFT_TOP, label, font.clone(), TEXT);
    let pct = if fraction >= 1.0 && done > 0.5 { "done".to_string() } else { format!("{:.0}%", shown * 100.0) };
    painter.text(rect.right_top(), egui::Align2::RIGHT_TOP, pct, font, mix(TEXT_WEAK, OK, done));
    let track = Rect::from_min_size(Pos2::new(rect.left(), rect.bottom() - 6.0), Vec2::new(width, 6.0));
    painter.rect_filled(track, 3.0, INSET);
    painter.rect_stroke(track, 3.0, Stroke::new(1.0, BORDER), egui::StrokeKind::Inside);
    if shown > 0.0 {
        let fill = Rect::from_min_size(track.min, Vec2::new((width * shown).max(6.0), 6.0));
        let green = OK.gamma_multiply(0.8);
        gradient_capsule(&painter, fill, mix(ACCENT, green, done), mix(ACCENT_2, green, done));
        // Moving sheen so a slow step still looks alive; it fades out with the colour change.
        if done < 1.0 {
            let t = ui.input(|i| i.time) as f32;
            let x = fill.left() + (t * 0.6).fract() * (fill.width() + 40.0) - 20.0;
            let sheen = Rect::from_center_size(Pos2::new(x, fill.center().y), Vec2::new(18.0, 6.0)).intersect(fill);
            if sheen.width() > 0.0 {
                painter.rect_filled(sheen, 3.0, Color32::from_white_alpha((40.0 * (1.0 - done)) as u8));
            }
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
        }
    }
    resp
}

/// Loading indicator: the two logo squares sliding apart and back together. Call it every frame
/// with `active`; when work stops the squares settle together, fade, and the space closes up.
/// `bg` is the colour behind it (page or card): the thin rounded gap around the pink square is drawn in it.
pub fn busy(ui: &mut Ui, id_salt: impl std::hash::Hash, active: bool, bg: Color32) -> Option<Response> {
    let t = ease(ui.ctx(), ui.id().with(("busy", id_salt)), active, FADE_SECS);
    if t <= 0.0 {
        return None;
    }
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(26.0 * t, 16.0), Sense::hover());
    if ui.is_rect_visible(rect) {
        let time = ui.input(|i| i.time) as f32;
        let size = 11.0;
        let radius = 3.0;
        // Distance between the squares' left edges: breathes between 4 and 9 while busy and
        // eases back to the logo's resting overlap as it fades.
        let breathe = (time * 3.2).sin() * 0.5 + 0.5;
        let sep = 4.0 + 5.0 * breathe * t;
        // Snap both squares to whole screen pixels: at fractional positions the anti-aliased edges
        // change from frame to frame and the gap seems to change width as they move.
        let ppp = ui.ctx().pixels_per_point();
        let snap = |v: f32| (v * ppp).round() / ppp;
        let c = rect.center();
        let back = Rect::from_min_size(Pos2::new(snap(c.x - sep / 2.0 - size / 2.0), snap(c.y - size / 2.0)), Vec2::splat(size));
        let front = Rect::from_min_size(Pos2::new(snap(c.x + sep / 2.0 - size / 2.0), back.top()), Vec2::splat(size));
        // The pink square sits on the violet one with a thin gap that follows its rounded corners
        // (a ring in the background colour). Both at full accent brightness: a darker pink made its
        // square look smaller than the violet one, though they are the same size.
        let p = ui.painter();
        p.rect_filled(back, radius, ACCENT.gamma_multiply(t));
        p.rect_filled(front.expand(1.25), radius + 1.25, bg.gamma_multiply(t));
        p.rect_filled(front, radius, ACCENT_2.gamma_multiply(t));
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(16));
    }
    Some(resp)
}

/// The app mark: two overlapping rounded frames, like two sources side by side.
pub fn logo(ui: &mut Ui) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(26.0, 20.0), Sense::hover());
    let p = ui.painter();
    let a = Rect::from_center_size(Pos2::new(rect.left() + 7.5, rect.center().y), Vec2::splat(15.0));
    let b = a.translate(Vec2::new(9.0, 0.0));
    p.rect_filled(a, 4.0, ACCENT);
    p.rect_filled(b, 4.0, ACCENT_2.gamma_multiply(0.85));
    p.rect_stroke(b, 4.0, Stroke::new(1.5, BAR), egui::StrokeKind::Outside);
    resp
}

/// One step in the top bar: an optional number in a circle, then the label.
pub fn step_tab(ui: &mut Ui, number: Option<u8>, label: &str, selected: bool, enabled: bool) -> Response {
    let font = FontId::proportional(14.0);
    let galley = ui.painter().layout_no_wrap(label.to_string(), font, TEXT);
    let circle = if number.is_some() { 20.0 } else { 0.0 };
    let gap = if number.is_some() { 8.0 } else { 0.0 };
    let pad = 10.0;
    let size = Vec2::new(pad * 2.0 + circle + gap + galley.size().x, 32.0);
    let (rect, resp) = ui.allocate_exact_size(size, if enabled { Sense::click() } else { Sense::hover() });
    let resp = if enabled { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp };
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let hovered = enabled && resp.hovered();
    let dim = if enabled { 1.0 } else { 0.4 };
    let p = ui.painter();
    if selected {
        p.rect_filled(rect, 8.0, SURFACE_2);
        p.rect_stroke(rect, 8.0, Stroke::new(1.0, BORDER), egui::StrokeKind::Inside);
    } else if hovered {
        p.rect_filled(rect, 8.0, SURFACE);
    }
    let mut x = rect.left() + pad;
    if let Some(n) = number {
        let c = Pos2::new(x + circle / 2.0, rect.center().y);
        if selected {
            p.circle_filled(c, circle / 2.0, ACCENT);
        } else {
            p.circle_stroke(c, circle / 2.0 - 0.5, Stroke::new(1.0, TEXT_WEAK.gamma_multiply(dim)));
        }
        let col = if selected { Color32::WHITE } else { TEXT_WEAK.gamma_multiply(dim) };
        p.text(c, egui::Align2::CENTER_CENTER, n.to_string(), FontId::proportional(11.5), col);
        x += circle + gap;
    }
    let col = if selected || hovered { TEXT_BRIGHT } else { TEXT_WEAK.gamma_multiply(dim) };
    p.galley_with_override_text_color(Pos2::new(x, rect.center().y - galley.size().y / 2.0), galley, col);
    if selected {
        // Accent underline on the bar's bottom border, like a tab indicator.
        let y = rect.bottom() + BAR_PAD_Y - 1.0;
        p.line_segment([Pos2::new(rect.left() + 10.0, y), Pos2::new(rect.right() - 10.0, y)], Stroke::new(2.0, ACCENT));
    }
    resp
}

/// Thin connector between step tabs.
pub fn step_link(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(14.0, 32.0), Sense::hover());
    ui.painter().line_segment([rect.left_center(), rect.right_center()], Stroke::new(1.0, BORDER));
}

/// Small coloured status pill ("done", "stopped").
pub fn pill(ui: &mut Ui, text: &str, color: Color32) -> Response {
    let galley = ui.painter().layout_no_wrap(text.to_string(), FontId::proportional(11.5), color);
    let size = galley.size() + Vec2::new(14.0, 6.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, rect.height() / 2.0, color.gamma_multiply(0.16));
    p.galley(rect.center() - galley.size() / 2.0, galley, color);
    resp
}

/// A file or folder path on one line: cut with "…" when it doesn't fit, the full path on hover.
pub fn path_label(ui: &mut Ui, path: &str, color: Color32) -> Response {
    ui.add(egui::Label::new(RichText::new(path).color(color)).truncate()).on_hover_text(path)
}

/// A full-width strip of controls at the top of a page.
pub fn toolbar() -> egui::Frame {
    egui::Frame::new().fill(SURFACE).stroke(Stroke::new(1.0, BORDER)).corner_radius(10).inner_margin(Margin::symmetric(10, 8))
}

/// Short vertical divider between groups of controls in a row.
pub fn vsep(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(9.0, 20.0), Sense::hover());
    ui.painter().line_segment([rect.center_top(), rect.center_bottom()], Stroke::new(1.0, BORDER));
}

/// Segmented control: one inset strip with an option per segment, the chosen one raised.
/// `options` are (label, hover text, shortcut keys). Returns the index clicked, if any.
/// `id_salt` must be unique per control: segments are identified by it, not by where they sit,
/// so two controls in look-alike rows can't share widget ids.
pub fn segmented(ui: &mut Ui, id_salt: impl std::hash::Hash, options: &[(&str, &str, &[&str])], selected: usize) -> Option<usize> {
    let base = egui::Id::new(("segmented", id_salt));
    let font = FontId::proportional(14.0);
    let pad = Vec2::new(12.0, 0.0);
    let h = 28.0;
    let galleys: Vec<_> = options.iter().map(|(l, ..)| ui.painter().layout_no_wrap(l.to_string(), font.clone(), TEXT)).collect();
    let width: f32 = galleys.iter().map(|g| g.size().x + pad.x * 2.0).sum::<f32>() + 4.0;
    let (outer, _) = ui.allocate_exact_size(Vec2::new(width, h), Sense::hover());
    let p = ui.painter().clone();
    p.rect_filled(outer, 8.0, INSET);
    p.rect_stroke(outer, 8.0, Stroke::new(1.0, BORDER), egui::StrokeKind::Inside);
    let mut x = outer.left() + 2.0;
    let mut clicked = None;
    for (i, (g, (_, hover, keys))) in galleys.into_iter().zip(options).enumerate() {
        let w = g.size().x + pad.x * 2.0;
        let seg = Rect::from_min_size(Pos2::new(x, outer.top() + 2.0), Vec2::new(w, h - 4.0));
        let resp = key_hint(ui.interact(seg, base.with(i), Sense::click()), hover, keys).on_hover_cursor(egui::CursorIcon::PointingHand);
        let col = if i == selected {
            p.rect_filled(seg, 6.0, SURFACE_3);
            p.rect_stroke(seg, 6.0, Stroke::new(1.0, ACCENT.gamma_multiply(0.6)), egui::StrokeKind::Inside);
            TEXT_BRIGHT
        } else if resp.hovered() {
            p.rect_filled(seg, 6.0, SURFACE);
            TEXT
        } else {
            TEXT_WEAK
        };
        p.galley_with_override_text_color(seg.center() - g.size() / 2.0, g, col);
        if resp.clicked() && i != selected {
            clicked = Some(i);
        }
        x += w;
    }
    clicked
}

#[derive(Clone, Copy, PartialEq)]
pub enum Icon {
    Up,
    Down,
    Remove,
}

/// Small square icon button with no background until hovered. Remove turns red on hover.
pub fn icon_button(ui: &mut Ui, icon: Icon, enabled: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(26.0), if enabled { Sense::click() } else { Sense::hover() });
    let resp = if enabled { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp };
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let hovered = enabled && resp.hovered();
    let danger = icon == Icon::Remove;
    let p = ui.painter();
    if hovered {
        let fill = if danger { ERR.gamma_multiply(0.18) } else { SURFACE_3 };
        p.rect_filled(rect, 6.0, fill);
    }
    let col = match (enabled, hovered, danger) {
        (false, ..) => TEXT_WEAK.gamma_multiply(0.35),
        (true, true, true) => ERR,
        (true, true, false) => TEXT_BRIGHT,
        _ => TEXT_WEAK,
    };
    let stroke = Stroke::new(1.6, col);
    let c = rect.center();
    match icon {
        Icon::Up | Icon::Down => {
            let s = if icon == Icon::Up { -1.0 } else { 1.0 };
            p.line_segment([c + Vec2::new(-4.5, -2.25 * s), c + Vec2::new(0.0, 2.25 * s)], stroke);
            p.line_segment([c + Vec2::new(0.0, 2.25 * s), c + Vec2::new(4.5, -2.25 * s)], stroke);
        }
        Icon::Remove => {
            p.line_segment([c + Vec2::new(-4.0, -4.0), c + Vec2::new(4.0, 4.0)], stroke);
            p.line_segment([c + Vec2::new(-4.0, 4.0), c + Vec2::new(4.0, -4.0)], stroke);
        }
    }
    resp
}

// ---------------------------------------------------------------- form controls and disclosure

/// Checkbox: a rounded square with a visible border; filled with the accent and ticked when on.
/// The fill eases in and out. Clicking the label toggles it too.
pub fn checkbox(ui: &mut Ui, on: &mut bool, text: &str) -> Response {
    let box_size = 16.0;
    let gap = if text.is_empty() { 0.0 } else { 8.0 };
    let galley = (!text.is_empty()).then(|| ui.painter().layout_no_wrap(text.to_string(), FontId::proportional(14.0), TEXT));
    let text_w = galley.as_ref().map_or(0.0, |g| g.size().x);
    let h = ui.spacing().interact_size.y.max(box_size);
    let (rect, mut resp) = ui.allocate_exact_size(Vec2::new(box_size + gap + text_w, h), Sense::click());
    if ui.is_enabled() {
        resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    }
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool_with_time(resp.id, *on, 0.12);
        let hovered = resp.hovered() && ui.is_enabled();
        let b = Rect::from_center_size(Pos2::new(rect.left() + box_size / 2.0, rect.center().y), Vec2::splat(box_size));
        let p = ui.painter();
        let rest_border = Color32::from_rgb(0x4a, 0x4f, 0x5e);
        p.rect_filled(b, 4.0, mix(INSET, ACCENT, t));
        let border = if hovered { ACCENT } else { mix(rest_border, ACCENT, t) };
        p.rect_stroke(b, 4.0, Stroke::new(1.0, border), egui::StrokeKind::Inside);
        if t > 0.0 {
            // Tick drawn from its short stroke to its long one, so it "writes" itself as it appears.
            let a = b.center() + Vec2::new(-3.8, 0.2);
            let m = b.center() + Vec2::new(-1.2, 2.8);
            let z = b.center() + Vec2::new(4.0, -2.8);
            let col = Color32::WHITE.gamma_multiply(t);
            let stroke = Stroke::new(1.8, col);
            let first = (t * 2.0).min(1.0);
            p.line_segment([a, a + (m - a) * first], stroke);
            if t > 0.5 {
                let second = (t - 0.5) * 2.0;
                p.line_segment([m, m + (z - m) * second], stroke);
            }
        }
        if let Some(g) = galley {
            let col = if hovered { TEXT_BRIGHT } else { TEXT };
            p.galley_with_override_text_color(Pos2::new(b.right() + gap, rect.center().y - g.size().y / 2.0), g, col);
        }
    }
    resp
}

/// Chevron for disclosure headers: points right when closed and turns down as it opens.
pub fn chevron(ui: &mut Ui, openness: f32, response: &Response) {
    let c = response.rect.center();
    let col = if response.hovered() { TEXT_BRIGHT } else { TEXT_WEAK };
    let rot = egui::emath::Rot2::from_angle(openness * std::f32::consts::FRAC_PI_2);
    let pts = [Vec2::new(-1.8, -4.0), Vec2::new(2.2, 0.0), Vec2::new(-1.8, 4.0)].map(|v| c + rot * v);
    let stroke = Stroke::new(1.6, col);
    ui.painter().line_segment([pts[0], pts[1]], stroke);
    ui.painter().line_segment([pts[1], pts[2]], stroke);
}

/// A collapsible section: chevron plus a custom header row (all of it clickable), then the body,
/// which slides open. Returns the body's value when it is shown.
pub fn disclosure<R>(
    ui: &mut Ui,
    id: egui::Id,
    default_open: bool,
    header: impl FnOnce(&mut Ui),
    body: impl FnOnce(&mut Ui) -> R,
) -> Option<R> {
    let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, default_open);
    let row = ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let button = state.show_toggle_button(ui, chevron);
        ui.spacing_mut().item_spacing.x = 8.0;
        header(ui);
        button
    });
    // Clicking anywhere on the header row's text toggles it, not only the chevron.
    let hit = ui.interact(row.response.rect, id.with("header"), Sense::click());
    if hit.clicked() && !row.inner.clicked() {
        state.toggle(ui);
    }
    if hit.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    state.show_body_indented(&row.response, ui, body).map(|r| r.inner)
}

/// An inset panel for machine output (logs): darker than the card, monospace.
pub fn code_panel() -> egui::Frame {
    egui::Frame::new().fill(INSET).stroke(Stroke::new(1.0, BORDER)).corner_radius(6).inner_margin(Margin::symmetric(10, 8))
}

// ---------------------------------------------------------------- keyboard hints

/// Small keyboard glyph: a rounded outline with two rows of keys and a space bar.
fn keyboard_icon(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(17.0, 12.0), Sense::hover());
    let p = ui.painter();
    p.rect_stroke(rect, 2.5, Stroke::new(1.1, TEXT_WEAK), egui::StrokeKind::Inside);
    for row in 0..2 {
        for k in 0..4 {
            let c = Pos2::new(rect.left() + 3.6 + k as f32 * 3.3, rect.top() + 3.4 + row as f32 * 2.6);
            p.rect_filled(Rect::from_center_size(c, Vec2::splat(1.4)), 0.3, TEXT_WEAK);
        }
    }
    let y = rect.bottom() - 2.8;
    p.line_segment([Pos2::new(rect.left() + 5.0, y), Pos2::new(rect.right() - 5.0, y)], Stroke::new(1.3, TEXT_WEAK));
}

/// One key drawn as a keycap.
pub fn keycap(ui: &mut Ui, key: &str) {
    let galley = ui.painter().layout_no_wrap(key.to_string(), FontId::proportional(12.0), TEXT_BRIGHT);
    let size = Vec2::new((galley.size().x + 10.0).max(18.0), 18.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 4.0, SURFACE_3);
    p.rect_stroke(rect, 4.0, Stroke::new(1.0, Color32::from_rgb(0x45, 0x49, 0x57)), egui::StrokeKind::Inside);
    p.galley(rect.center() - galley.size() / 2.0, galley, TEXT_BRIGHT);
}

/// Tooltip with the shortcut first (keyboard icon and keycaps) and the description after it on
/// the same line; long descriptions wrap onto further lines.
pub fn key_hint(resp: Response, text: &str, keys: &[&str]) -> Response {
    if text.is_empty() && keys.is_empty() {
        return resp;
    }
    let text = text.to_string();
    let keys: Vec<String> = keys.iter().map(|k| k.to_string()).collect();
    resp.on_hover_ui(move |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if !keys.is_empty() {
                keyboard_icon(ui);
                ui.add_space(2.0);
                for k in &keys {
                    keycap(ui, k);
                }
                ui.add_space(6.0);
            }
            if !text.is_empty() {
                ui.label(&text);
            }
        });
    })
}

/// One screen's shortcuts in the "?" list.
pub struct ShortcutGroup {
    pub title: &'static str,
    /// Belongs to the screen being shown: listed first; the screen's main group is marked.
    pub current: bool,
    /// A view mode within a screen ("Viewer · Slider"): drawn a little tighter under its screen.
    pub sub: bool,
    /// Keys and what they do. "–" and lower-case words ("scroll", "drag") are drawn as text, not keycaps.
    pub rows: Vec<(Vec<&'static str>, &'static str)>,
}

/// Round "?" button that opens a list of every keyboard shortcut, grouped by screen. The "?" key
/// toggles it as well; Esc or a click elsewhere closes it.
pub fn shortcuts_button(ui: &mut Ui, groups: &[ShortcutGroup]) {
    let open_id = egui::Id::new("shortcuts_open");
    let mut open: bool = ui.ctx().data(|d| d.get_temp(open_id)).unwrap_or(false);
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(28.0), Sense::click());
    let resp = key_hint(resp.on_hover_cursor(egui::CursorIcon::PointingHand), "Keyboard shortcuts", &["?"]);
    let hovered = resp.hovered();
    {
        let p = ui.painter();
        let c = rect.center();
        if open || hovered {
            p.circle_filled(c, 13.0, SURFACE_3);
        }
        p.circle_stroke(c, 12.5, Stroke::new(1.0, if open { ACCENT } else { Color32::from_rgb(0x45, 0x49, 0x57) }));
        let col = if open || hovered { TEXT_BRIGHT } else { TEXT };
        p.text(c, egui::Align2::CENTER_CENTER, "?", FontId::proportional(14.0), col);
    }
    let typed_q = !ui.ctx().wants_keyboard_input()
        && ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Text(t) if t == "?")));
    if resp.clicked() || typed_q {
        open = !open;
    }
    if open && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        open = false;
    }
    if open {
        let area = egui::Area::new(egui::Id::new("shortcuts_list"))
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::RIGHT_TOP)
            .fixed_pos(rect.right_bottom() + Vec2::new(0.0, 8.0))
            .constrain(true)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).inner_margin(Margin::same(14)).show(ui, |ui| {
                    ui.set_max_width(420.0);
                    ui.label(RichText::new("Keyboard shortcuts").size(15.0).color(TEXT_BRIGHT));
                    for g in groups {
                        ui.add_space(if g.sub { 6.0 } else { 12.0 });
                        ui.horizontal(|ui| {
                            section(ui, g.title);
                            if g.current && !g.sub {
                                pill(ui, "this screen", ACCENT);
                            }
                        });
                        egui::Grid::new(("shortcuts", g.title)).num_columns(2).spacing([14.0, 6.0]).show(ui, |ui| {
                            for (keys, what) in &g.rows {
                                ui.horizontal(|ui| {
                                    // Same key-column width in every group, so descriptions line up.
                                    ui.set_min_width(118.0);
                                    ui.spacing_mut().item_spacing.x = 4.0;
                                    for k in keys {
                                        let plain = *k == "–" || k.chars().next().is_some_and(|c| c.is_lowercase());
                                        if plain {
                                            ui.label(RichText::new(*k).color(TEXT_WEAK));
                                        } else {
                                            keycap(ui, k);
                                        }
                                    }
                                });
                                ui.label(RichText::new(*what).color(TEXT));
                                ui.end_row();
                            }
                        });
                    }
                });
            });
        // Close on a click outside the list and the button.
        let clicked_outside = ui.input(|i| i.pointer.any_click())
            && ui.input(|i| i.pointer.interact_pos()).is_some_and(|p| !area.response.rect.contains(p) && !rect.contains(p));
        if clicked_outside {
            open = false;
        }
    }
    ui.ctx().data_mut(|d| d.insert_temp(open_id, open));
}

/// Call once at the end of each frame: shows the pointing-hand cursor over any enabled, click-only
/// widget that hasn't set a cursor of its own: combo boxes and their items, menu entries,
/// selectable labels, toggles. Text fields and drag values keep theirs; draggable areas (sliders,
/// the image stage) are left alone.
pub fn pointer_cursor_for_clickables(ctx: &egui::Context) {
    let Some(pos) = ctx.pointer_hover_pos() else { return };
    let Some(w) = ctx.viewport(|v| v.hits.click) else { return };
    // A popup's background counts as clickable in egui (its "move" widget); the text and keycaps
    // drawn on it aren't, so it shouldn't get the hand.
    let area_background = w.id == w.layer_id.id.with("move");
    let clickable = w.enabled && w.sense.senses_click() && !w.sense.senses_drag() && !area_background;
    if clickable && w.interact_rect.contains(pos) && ctx.output(|o| o.cursor_icon) == egui::CursorIcon::Default {
        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
    }
}

/// A row in a list or menu: left-aligned text, rounded fill on hover, soft accent fill when selected.
pub fn list_row(ui: &mut Ui, selected: bool, text: &str, width: f32) -> Response {
    let galley = ui.painter().layout_no_wrap(text.to_string(), FontId::proportional(14.0), TEXT);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 28.0), Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    if ui.is_rect_visible(rect) {
        let hovered = resp.hovered();
        let p = ui.painter();
        if selected {
            p.rect_filled(rect, 6.0, ACCENT_SOFT);
        } else if hovered {
            p.rect_filled(rect, 6.0, SURFACE_3);
        }
        let col = if selected || hovered { TEXT_BRIGHT } else { TEXT };
        let clip = rect.shrink2(Vec2::new(10.0, 0.0));
        p.with_clip_rect(clip).galley_with_override_text_color(Pos2::new(clip.left(), rect.center().y - galley.size().y / 2.0), galley, col);
    }
    resp
}

/// Digit keys 1–9 pressed this frame, as (index 0–8, Shift held). Matches the physical key as well
/// as the logical one, so Shift + a top-row digit counts even where it types "!" or "@".
pub fn digit_presses(i: &egui::InputState) -> Vec<(usize, bool)> {
    use egui::Key::*;
    let digit = |k: egui::Key| [Num1, Num2, Num3, Num4, Num5, Num6, Num7, Num8, Num9].iter().position(|d| *d == k);
    i.events
        .iter()
        .filter_map(|e| match e {
            egui::Event::Key { key, physical_key, pressed: true, repeat: false, modifiers } => {
                physical_key.and_then(digit).or_else(|| digit(*key)).map(|idx| (idx, modifiers.shift))
            }
            _ => None,
        })
        .collect()
}
