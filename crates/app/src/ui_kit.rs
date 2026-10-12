//! The app's look: colours, spacing and the few building blocks every screen is made of
//! (cards, rows, buttons, badges, switches). Screens use these instead of styling widgets
//! themselves, so everything looks the same and long text is shortened instead of wrapping
//! letter by letter.

use egui::{Align, Color32, CornerRadius, Layout, Margin, Response, RichText, Stroke, Ui, Vec2};

pub(crate) const BG: Color32 = Color32::from_rgb(16, 18, 23);
pub(crate) const SIDEBAR: Color32 = Color32::from_rgb(21, 24, 30);
pub(crate) const SURFACE: Color32 = Color32::from_rgb(26, 29, 36);
pub(crate) const SURFACE_HI: Color32 = Color32::from_rgb(34, 38, 47);
pub(crate) const BORDER: Color32 = Color32::from_rgb(44, 49, 60);
pub(crate) const TEXT: Color32 = Color32::from_rgb(232, 234, 239);
pub(crate) const MUTED: Color32 = Color32::from_rgb(142, 148, 162);
pub(crate) const FAINT: Color32 = Color32::from_rgb(100, 106, 120);
pub(crate) const ACCENT: Color32 = Color32::from_rgb(91, 132, 255);
pub(crate) const OK: Color32 = Color32::from_rgb(47, 191, 113);
pub(crate) const WARN: Color32 = Color32::from_rgb(245, 165, 36);
pub(crate) const ERR: Color32 = Color32::from_rgb(229, 72, 77);

/// Corner radii and paddings used everywhere.
pub(crate) const R_CARD: u8 = 14;
pub(crate) const R_ROW: u8 = 10;
pub(crate) const R_BTN: u8 = 8;
pub(crate) const PAD_CARD: i8 = 18;
pub(crate) const ROW_H: f32 = 34.0;

/// A colour mixed over the surface (opaque, unlike `gamma_multiply`).
pub(crate) fn tint(color: Color32, amount: f32) -> Color32 {
    let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * amount).round() as u8;
    Color32::from_rgb(mix(SURFACE.r(), color.r()), mix(SURFACE.g(), color.g()), mix(SURFACE.b(), color.b()))
}

pub(crate) fn setup_style(ctx: &egui::Context) {
    use egui::{FontId, TextStyle};
    ctx.set_theme(egui::Theme::Dark);
    ctx.style_mut(|s| {
        s.text_styles = [
            (TextStyle::Heading, FontId::proportional(22.0)),
            (TextStyle::Body, FontId::proportional(14.5)),
            (TextStyle::Button, FontId::proportional(14.0)),
            (TextStyle::Small, FontId::proportional(12.0)),
            (TextStyle::Monospace, FontId::monospace(14.0)),
        ]
        .into();
        s.spacing.item_spacing = egui::vec2(8.0, 8.0);
        s.spacing.button_padding = egui::vec2(12.0, 6.0);
        s.spacing.interact_size.y = 30.0;
        s.spacing.menu_margin = Margin::same(8);
        let v = &mut s.visuals;
        v.override_text_color = None;
        v.panel_fill = BG;
        v.window_fill = SURFACE;
        v.faint_bg_color = SURFACE;
        v.extreme_bg_color = Color32::from_rgb(13, 15, 19);
        v.code_bg_color = SURFACE_HI;
        v.window_corner_radius = CornerRadius::same(R_CARD);
        v.menu_corner_radius = CornerRadius::same(R_ROW);
        v.window_stroke = Stroke::new(1.0_f32, BORDER);
        v.window_shadow = egui::epaint::Shadow { offset: [0, 8], blur: 28, spread: 0, color: Color32::from_black_alpha(110) };
        v.popup_shadow = egui::epaint::Shadow { offset: [0, 6], blur: 18, spread: 0, color: Color32::from_black_alpha(90) };
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, BORDER);
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, TEXT);
        v.widgets.inactive.weak_bg_fill = SURFACE_HI;
        v.widgets.inactive.bg_fill = SURFACE_HI;
        v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, TEXT);
        // Radio buttons and checkboxes need an outline to be seen on the dark cards.
        v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(74, 81, 98));
        v.widgets.hovered.weak_bg_fill = Color32::from_rgb(45, 50, 62);
        v.widgets.hovered.bg_fill = Color32::from_rgb(45, 50, 62);
        v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(64, 70, 86));
        v.widgets.active.weak_bg_fill = Color32::from_rgb(52, 58, 72);
        v.selection.bg_fill = ACCENT.gamma_multiply(0.55);
        v.selection.stroke = Stroke::new(1.0_f32, TEXT);
        v.hyperlink_color = ACCENT;
        v.text_cursor.stroke = Stroke::new(2.0_f32, ACCENT);
        for w in [&mut v.widgets.noninteractive, &mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
            w.corner_radius = CornerRadius::same(R_BTN);
        }
    });
}

// ---- text ----

/// Page title and a one-line explanation under it.
pub(crate) fn page_header(ui: &mut Ui, title: &str, subtitle: &str) {
    ui.label(RichText::new(title).size(22.0).color(TEXT).strong());
    if !subtitle.is_empty() {
        ui.add(egui::Label::new(RichText::new(subtitle).color(MUTED)).wrap());
    }
    ui.add_space(10.0);
}

/// Small grey caps label above a group ("YOUR ID").
pub(crate) fn section_label(ui: &mut Ui, text: &str) {
    ui.add_space(10.0);
    ui.label(RichText::new(text).size(11.5).color(MUTED).strong());
}

/// Secondary text that wraps.
pub(crate) fn note(ui: &mut Ui, text: impl Into<String>) -> Response {
    ui.add(egui::Label::new(RichText::new(text.into()).size(12.5).color(MUTED)).wrap())
}

/// One line that is shortened with "…" when there is no room.
pub(crate) fn line(ui: &mut Ui, text: RichText) -> Response {
    ui.add(egui::Label::new(text).truncate())
}

// ---- containers ----

/// A rounded panel as wide as the space it is in.
pub(crate) fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0_f32, BORDER))
        .corner_radius(R_CARD)
        .inner_margin(PAD_CARD)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// A card with a title (and an optional explanation) on top.
pub(crate) fn titled_card<R>(ui: &mut Ui, title: &str, subtitle: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    card(ui, |ui| {
        ui.label(RichText::new(title).size(16.0).color(TEXT).strong());
        if !subtitle.is_empty() {
            note(ui, subtitle);
        }
        ui.add_space(6.0);
        add(ui)
    })
}

/// A list row: content on the left (shortened if needed), controls on the right.
pub(crate) fn row(ui: &mut Ui, left: impl FnOnce(&mut Ui), right: impl FnOnce(&mut Ui)) {
    egui::Frame::new().fill(SURFACE_HI).corner_radius(R_ROW).inner_margin(Margin::symmetric(12, 8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        split(ui, ROW_H, left, right);
    });
}

/// Controls on the right, the rest of the width for the left side; as tall as the content.
pub(crate) fn split(ui: &mut Ui, min_height: f32, left: impl FnOnce(&mut Ui), right: impl FnOnce(&mut Ui)) {
    ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), min_height), Layout::right_to_left(Align::Center), |ui| {
        right(ui);
        ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), min_height), Layout::left_to_right(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            left(ui)
        });
    });
}

/// A section label with something small on the right ("RECENT … Clear").
pub(crate) fn section_header(ui: &mut Ui, text: &str, right: impl FnOnce(&mut Ui)) {
    ui.add_space(10.0);
    split(ui, 22.0, |ui| {
        ui.label(RichText::new(text).size(11.5).color(MUTED).strong());
    }, right);
}

/// Two-line text for a row: a name and a grey detail line.
pub(crate) fn row_text(ui: &mut Ui, title: &str, detail: &str) {
    ui.vertical(|ui| {
        line(ui, RichText::new(title).color(TEXT).strong());
        if !detail.is_empty() {
            line(ui, RichText::new(detail).size(12.0).color(MUTED));
        }
    });
}

/// A settings line: title and explanation on the left, the control on the right.
pub(crate) fn setting_row(ui: &mut Ui, title: &str, description: &str, control: impl FnOnce(&mut Ui)) {
    split(
        ui,
        ROW_H,
        |ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.add(egui::Label::new(RichText::new(title).color(TEXT)).wrap());
                if !description.is_empty() {
                    note(ui, description);
                }
            });
        },
        |ui| {
            ui.add_space(8.0);
            control(ui);
        },
    );
}

/// Thin line between groups inside a card.
pub(crate) fn divider(ui: &mut Ui) {
    ui.add_space(4.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 0.0, BORDER);
    ui.add_space(4.0);
}

/// "Nothing here yet" text.
pub(crate) fn empty(ui: &mut Ui, text: &str) {
    egui::Frame::new().fill(SURFACE_HI.gamma_multiply(0.6)).corner_radius(R_ROW).inner_margin(Margin::symmetric(12, 10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.add(egui::Label::new(RichText::new(text).color(FAINT)).wrap());
    });
}

/// Highlighted box for something that needs the user.
pub(crate) fn callout(ui: &mut Ui, color: Color32, add: impl FnOnce(&mut Ui)) {
    egui::Frame::new()
        .fill(tint(color, 0.13))
        .stroke(Stroke::new(1.0_f32, tint(color, 0.55)))
        .corner_radius(R_ROW)
        .inner_margin(12)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
}

// ---- buttons ----

fn button(text: &str, fill: Color32, color: Color32, stroke: Stroke) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_string()).color(color))
        .fill(fill)
        .stroke(stroke)
        .corner_radius(R_BTN)
        .min_size(Vec2::new(0.0, 30.0))
}

/// The main action of a screen or dialog.
pub(crate) fn primary(text: &str) -> egui::Button<'static> {
    button(text, ACCENT, Color32::WHITE, Stroke::NONE)
}

pub(crate) fn secondary(text: &str) -> egui::Button<'static> {
    button(text, SURFACE_HI, TEXT, Stroke::new(1.0_f32, BORDER))
}

/// Something that removes or refuses.
pub(crate) fn danger(text: &str) -> egui::Button<'static> {
    button(text, tint(ERR, 0.22), Color32::from_rgb(255, 196, 198), Stroke::new(1.0_f32, tint(ERR, 0.5)))
}

/// Strong red: ends a session.
pub(crate) fn danger_solid(text: &str) -> egui::Button<'static> {
    button(text, ERR, Color32::WHITE, Stroke::NONE)
}

/// Text-like button for minor actions.
pub(crate) fn quiet(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_string()).color(MUTED)).frame(false).min_size(Vec2::new(0.0, 26.0))
}

/// Small borderless icon button with a tooltip.
pub(crate) fn icon_button(ui: &mut Ui, icon: &str, tip: &str) -> Response {
    ui.add(egui::Button::new(RichText::new(icon).size(15.0).color(MUTED)).frame(false).min_size(Vec2::new(28.0, 28.0)))
        .on_hover_text(tip)
}

/// Full-width button (Connect).
pub(crate) fn wide(ui: &mut Ui, button: egui::Button<'_>, height: f32) -> Response {
    ui.add_sized([ui.available_width(), height], button)
}

// ---- small things ----

/// Coloured dot with text (painted: the bullet glyph is missing from the default font).
pub(crate) fn dot(ui: &mut Ui, color: Color32, text: impl Into<String>) -> Response {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 18.0), egui::Sense::hover());
        ui.painter().circle_filled(rect.center(), 4.5, color);
        ui.add(egui::Label::new(RichText::new(text.into()).color(TEXT)).wrap())
    })
    .inner
}

/// Small rounded label ("trusted", "active").
pub(crate) fn badge(ui: &mut Ui, text: &str, color: Color32) -> Response {
    let galley = ui.painter().layout_no_wrap(text.to_string(), egui::FontId::proportional(11.5), color);
    let size = egui::vec2(galley.size().x + 16.0, 20.0);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter().rect_filled(rect, 10.0, tint(color, 0.2));
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, color);
    resp
}

/// Rounded status pill with a dot (header status), as wide as its text.
pub(crate) fn status_pill(ui: &mut Ui, color: Color32, text: &str) -> Response {
    let galley = ui.painter().layout_no_wrap(text.to_string(), egui::FontId::proportional(13.0), color);
    let size = egui::vec2(galley.size().x + 32.0, 26.0);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let painter = ui.painter();
    painter.rect_filled(rect, 13.0, tint(color, 0.16));
    painter.circle_filled(egui::pos2(rect.left() + 13.0, rect.center().y), 4.0, color);
    painter.galley(egui::pos2(rect.left() + 23.0, rect.center().y - galley.size().y / 2.0), galley, color);
    resp
}

/// On/off switch.
pub(crate) fn switch(ui: &mut Ui, on: &mut bool) -> Response {
    let size = egui::vec2(40.0, 22.0);
    let (rect, mut resp) = ui.allocate_exact_size(size, egui::Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool_responsive(resp.id, *on);
    let bg = if *on { ACCENT } else { Color32::from_rgb(58, 64, 78) };
    let painter = ui.painter();
    painter.rect_filled(rect, rect.height() / 2.0, bg);
    let r = rect.height() / 2.0 - 3.0;
    let x = egui::lerp((rect.left() + r + 3.0)..=(rect.right() - r - 3.0), t);
    painter.circle_filled(egui::pos2(x, rect.center().y), r, Color32::WHITE);
    resp
}

/// Connected buttons, one of them selected.
pub(crate) fn segmented<T: PartialEq + Copy>(ui: &mut Ui, value: &mut T, options: &[(T, &str, &str)]) -> bool {
    let mut changed = false;
    egui::Frame::new().fill(Color32::from_rgb(13, 15, 19)).corner_radius(R_BTN + 2).inner_margin(3).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for (v, label, tip) in options {
                let on = *value == *v;
                let text = RichText::new(*label).size(13.5).color(if on { Color32::WHITE } else { MUTED });
                let btn = egui::Button::new(text)
                    .fill(if on { ACCENT } else { Color32::TRANSPARENT })
                    .stroke(Stroke::NONE)
                    .corner_radius(R_BTN)
                    .min_size(Vec2::new(0.0, 28.0));
                let resp = ui.add(btn);
                let resp = if tip.is_empty() { resp } else { resp.on_hover_text(*tip) };
                if resp.clicked() && !on {
                    *value = *v;
                    changed = true;
                }
            }
        });
    });
    changed
}

/// Single-line text field in the app's style.
pub(crate) fn input<'a>(text: &'a mut String, hint: &str) -> egui::TextEdit<'a> {
    egui::TextEdit::singleline(text)
        .hint_text(RichText::new(hint.to_string()).color(FAINT))
        .margin(Margin::symmetric(10, 7))
        .background_color(Color32::from_rgb(13, 15, 19))
}

/// A thin bar that empties as time runs out (dialogs that answer themselves).
pub(crate) fn countdown(ui: &mut Ui, fraction: f32, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 3.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 1.5, BORDER);
    let mut done = rect;
    done.set_width(rect.width() * fraction.clamp(0.0, 1.0));
    ui.painter().rect_filled(done, 1.5, color);
}

/// Round icon with a tinted background (dialogs).
pub(crate) fn icon_disc(ui: &mut Ui, icon: &str, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(44.0, 44.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 22.0, tint(color, 0.22));
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, icon, egui::FontId::proportional(20.0), color);
}

/// A link that opens with the system's default program (egui's own opener does nothing on
/// some Linux desktops).
pub(crate) fn link(ui: &mut Ui, text: impl Into<egui::WidgetText>, url: &str) -> Response {
    let resp = ui.link(text).on_hover_text(url);
    if resp.clicked() {
        crate::open_url(url);
    }
    resp
}
