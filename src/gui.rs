//! The Edgehop window: header with status, tabs, and card-style sections.
use crate::config::{Config, Role};
use crate::layout::{Rect, ScreenPos};
use crate::net;
use crate::platform::{self, HIDE_TO_TRAY, WINDOW_TITLE};
use crate::state::{lk, AppState, ConnectedInfo};
use crate::tray;
use eframe::egui::{self, Color32, CornerRadius, FontId, Margin, RichText, Sense, Stroke, TextStyle};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Look and feel
// ---------------------------------------------------------------------------

const ACCENT: Color32 = Color32::from_rgb(99, 102, 241); // indigo
const OK: Color32 = Color32::from_rgb(34, 197, 94);
const WARN: Color32 = Color32::from_rgb(245, 158, 11);
const BAD: Color32 = Color32::from_rgb(239, 68, 68);
const IDLE: Color32 = Color32::from_rgb(148, 155, 170);

struct Palette {
    card: Color32,
    border: Color32,
    canvas: Color32,
    monitor: Color32,
    monitor_edge: Color32,
    muted: Color32,
    text: Color32,
}

fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            card: Color32::from_rgb(24, 27, 34),
            border: Color32::from_rgb(40, 45, 56),
            canvas: Color32::from_rgb(15, 17, 22),
            monitor: Color32::from_rgb(52, 58, 72),
            monitor_edge: Color32::from_rgb(78, 86, 104),
            muted: Color32::from_rgb(150, 157, 172),
            text: Color32::from_rgb(232, 234, 240),
        }
    } else {
        Palette {
            card: Color32::WHITE,
            border: Color32::from_rgb(226, 229, 236),
            canvas: Color32::from_rgb(241, 243, 247),
            monitor: Color32::from_rgb(214, 219, 229),
            monitor_edge: Color32::from_rgb(178, 186, 201),
            muted: Color32::from_rgb(104, 112, 128),
            text: Color32::from_rgb(28, 32, 42),
        }
    }
}

fn apply_theme(ctx: &egui::Context) {
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 8.0);
        s.spacing.button_padding = egui::vec2(12.0, 5.0);
        s.spacing.interact_size.y = 26.0;
        s.spacing.slider_width = 220.0;
        s.text_styles.insert(TextStyle::Heading, FontId::proportional(20.0));
        s.text_styles.insert(TextStyle::Body, FontId::proportional(14.0));
        s.text_styles.insert(TextStyle::Button, FontId::proportional(14.0));
        s.text_styles.insert(TextStyle::Small, FontId::proportional(11.5));
        s.text_styles.insert(TextStyle::Monospace, FontId::monospace(12.5));
    });
    for (theme, dark) in [(egui::Theme::Dark, true), (egui::Theme::Light, false)] {
        let p = palette(dark);
        let mut v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
        v.panel_fill = if dark { Color32::from_rgb(17, 19, 24) } else { Color32::from_rgb(246, 247, 250) };
        v.window_fill = p.card;
        v.extreme_bg_color = p.canvas;
        v.faint_bg_color = p.canvas;
        v.override_text_color = Some(p.text);
        v.hyperlink_color = ACCENT;
        v.selection.bg_fill = ACCENT.gamma_multiply(if dark { 0.55 } else { 0.85 });
        v.selection.stroke = Stroke::new(1.0_f32, Color32::WHITE);
        v.slider_trailing_fill = true;
        v.window_corner_radius = CornerRadius::same(14);
        v.menu_corner_radius = CornerRadius::same(10);
        v.window_stroke = Stroke::new(1.0_f32, p.border);
        for w in [
            &mut v.widgets.noninteractive,
            &mut v.widgets.inactive,
            &mut v.widgets.hovered,
            &mut v.widgets.active,
            &mut v.widgets.open,
        ] {
            w.corner_radius = CornerRadius::same(7);
        }
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, p.border);
        v.widgets.inactive.weak_bg_fill = if dark { Color32::from_rgb(36, 40, 50) } else { Color32::from_rgb(236, 238, 243) };
        v.widgets.inactive.bg_fill = v.widgets.inactive.weak_bg_fill;
        v.widgets.hovered.weak_bg_fill = if dark { Color32::from_rgb(46, 51, 64) } else { Color32::from_rgb(226, 229, 237) };
        v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, ACCENT.gamma_multiply(0.7));
        v.widgets.active.weak_bg_fill = ACCENT.gamma_multiply(0.35);
        ctx.set_visuals_of(theme, v);
    }
}

/// A rounded card section with an optional title and subtitle.
fn card<R>(ui: &mut egui::Ui, title: &str, subtitle: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let p = palette(ui.visuals().dark_mode);
    egui::Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0_f32, p.border))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            if !title.is_empty() {
                ui.label(RichText::new(title).size(15.5).strong());
            }
            if !subtitle.is_empty() {
                ui.label(RichText::new(subtitle).color(p.muted));
            }
            if !title.is_empty() || !subtitle.is_empty() {
                ui.add_space(4.0);
            }
            add(ui)
        })
        .inner
}

/// A painted status dot (font glyphs for "●" render inconsistently).
fn dot(ui: &mut egui::Ui, color: Color32, radius: f32) {
    let (r, _) = ui.allocate_exact_size(egui::vec2(radius * 2.0, radius * 2.0), Sense::hover());
    ui.painter().circle_filled(r.center(), radius, color);
}

/// "● Online"-style rounded badge.
fn pill(ui: &mut egui::Ui, color: Color32, text: &str) {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.16))
        .corner_radius(CornerRadius::same(255))
        .inner_margin(Margin::symmetric(10, 3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                dot(ui, color, 4.0);
                ui.label(RichText::new(text).color(color).size(12.5).strong());
            });
        });
}

fn primary_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(text).color(Color32::WHITE).strong())
            .fill(ACCENT)
            .min_size(egui::vec2(96.0, 30.0)),
    )
}

fn danger_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(egui::Button::new(RichText::new(text).color(BAD)).stroke(Stroke::new(1.0_f32, BAD.gamma_multiply(0.5))))
}

/// A setting row: label + help on the left, control on the right.
fn row(ui: &mut egui::Ui, label: &str, help: &str, control: impl FnOnce(&mut egui::Ui)) {
    let muted = palette(ui.visuals().dark_mode).muted;
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.set_width((ui.available_width() * 0.5).max(220.0));
            ui.label(label);
            if !help.is_empty() {
                ui.label(RichText::new(help).small().color(muted));
            }
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), control);
    });
}

/// Pairing code as big digit boxes: "482 913".
fn code_boxes(ui: &mut egui::Ui, code: &str) {
    let p = palette(ui.visuals().dark_mode);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        for (i, ch) in code.chars().filter(|c| c.is_ascii_digit()).enumerate() {
            if i == 3 {
                ui.add_space(10.0);
            }
            egui::Frame::new()
                .fill(p.canvas)
                .stroke(Stroke::new(1.0_f32, p.border))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.label(RichText::new(ch.to_string()).size(30.0).strong().monospace().color(ACCENT));
                });
        }
    });
}

// ---------------------------------------------------------------------------

pub fn run(app: Arc<AppState>, minimized: bool) -> eframe::Result<()> {
    let icon = egui::IconData { rgba: crate::icon::rgba(64), width: 64, height: 64 };
    let mut vp = egui::ViewportBuilder::default()
        .with_title(WINDOW_TITLE)
        .with_inner_size([900.0, 680.0])
        .with_min_inner_size([680.0, 520.0])
        .with_icon(Arc::new(icon));
    if minimized && HIDE_TO_TRAY {
        vp = vp.with_visible(false);
    }
    let options = eframe::NativeOptions { viewport: vp, ..Default::default() };
    eframe::run_native("edgehop", options, Box::new(move |cc| Ok(Box::new(Gui::new(cc, app, minimized)))))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Screens,
    Computers,
    Connection,
    Settings,
    Activity,
}

struct Gui {
    app: Arc<AppState>,
    tray: Option<tray::Tray>,
    tray_tried: bool,
    logo: Option<egui::TextureHandle>,
    tab: Tab,
    name_edit: String,
    addr_edit: String,
    message: Option<String>,
    start_minimized: bool,
    dragging: Option<ScreenPos>,
    confirm_forget: Option<String>,
}

impl Gui {
    fn new(cc: &eframe::CreationContext<'_>, app: Arc<AppState>, minimized: bool) -> Gui {
        *lk(&app.ctx) = Some(cc.egui_ctx.clone());
        apply_theme(&cc.egui_ctx);
        let cfg = app.cfg();
        let logo = cc.egui_ctx.load_texture(
            "logo",
            egui::ColorImage::from_rgba_unmultiplied([64, 64], &crate::icon::rgba(64)),
            egui::TextureOptions::LINEAR,
        );
        Gui {
            app,
            tray: None,
            tray_tried: false,
            logo: Some(logo),
            tab: if cfg.role == Role::Server { Tab::Screens } else { Tab::Connection },
            name_edit: cfg.name.clone(),
            addr_edit: cfg.server_address.clone(),
            message: None,
            start_minimized: minimized && !HIDE_TO_TRAY,
            dragging: None,
            confirm_forget: None,
        }
    }

    fn restart(&self) {
        self.app.start_engine();
    }

    fn tabs_for(role: Role) -> &'static [(Tab, &'static str)] {
        match role {
            Role::Server => &[
                (Tab::Screens, "Screens"),
                (Tab::Computers, "Computers"),
                (Tab::Settings, "Settings"),
                (Tab::Activity, "Activity"),
            ],
            Role::Client => &[(Tab::Connection, "Connection"), (Tab::Settings, "Settings"), (Tab::Activity, "Activity")],
        }
    }
}

impl eframe::App for Gui {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.tray_tried {
            self.tray_tried = true;
            self.tray = tray::create(self.app.clone());
        }
        if self.start_minimized {
            self.start_minimized = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }
        if ctx.input(|i| i.viewport().close_requested()) && self.tray.is_some() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            platform::hide_window(ctx);
        }

        let cfg = self.app.cfg();
        if !Self::tabs_for(cfg.role).iter().any(|(t, _)| *t == self.tab) {
            self.tab = Self::tabs_for(cfg.role)[0].0;
        }
        self.header(ctx, &cfg);
        self.pairing_dialog(ctx);

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(ctx.style().visuals.panel_fill).inner_margin(Margin::symmetric(20, 16)))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    egui::Frame::new().inner_margin(Margin { left: 0, right: 14, top: 4, bottom: 8 }).show(ui, |ui| {
                        match self.tab {
                            Tab::Screens => self.screens_tab(ui, &cfg),
                            Tab::Computers => self.computers_tab(ui, &cfg),
                            Tab::Connection => self.connection_tab(ui, &cfg),
                            Tab::Settings => self.settings_tab(ui, &cfg),
                            Tab::Activity => self.activity_tab(ui),
                        }
                    });
                });
            });

        ctx.request_repaint_after(Duration::from_millis(500));
    }
}

impl Gui {
    // ---------------- header ----------------

    fn header(&mut self, ctx: &egui::Context, cfg: &Config) {
        let dark = ctx.style().visuals.dark_mode;
        let p = palette(dark);
        egui::TopBottomPanel::top("header")
            .frame(
                egui::Frame::new()
                    .fill(p.card)
                    .stroke(Stroke::new(1.0_f32, p.border))
                    .inner_margin(Margin { left: 20, right: 20, top: 14, bottom: 0 }),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if let Some(logo) = &self.logo {
                        ui.image((logo.id(), egui::vec2(30.0, 30.0)));
                    }
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.label(RichText::new("Edgehop").size(20.0).strong());
                        ui.label(RichText::new("Keyboard & mouse sharing").small().color(p.muted));
                    });
                    ui.add_space(12.0);

                    let running = self.app.is_running();
                    let ok = *lk(&self.app.connected_ok);
                    let (color, label) = match (running, ok, cfg.role) {
                        (false, _, _) => (IDLE, "Stopped".to_string()),
                        (true, true, Role::Server) => {
                            let n = lk(&self.app.connected).len();
                            if n == 0 {
                                (WARN, "Waiting for computers".to_string())
                            } else {
                                (OK, format!("{n} connected"))
                            }
                        }
                        (true, true, Role::Client) => (OK, "Connected".to_string()),
                        (true, false, _) => (WARN, "Connecting".to_string()),
                    };
                    pill(ui, color, &label);
                    if running && cfg.role == Role::Server {
                        let active = lk(&self.app.active_screen).clone();
                        if !active.is_empty() {
                            ui.label(RichText::new(format!("Mouse on {active}")).color(p.muted));
                        }
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if running {
                            if ui.button("Stop").on_hover_text("Stop sharing").clicked() {
                                self.app.stop_engine();
                            }
                        } else if primary_button(ui, "Start").clicked() {
                            self.restart();
                        }
                    });
                });

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    for (tab, name) in Self::tabs_for(cfg.role) {
                        let selected = self.tab == *tab;
                        let text = RichText::new(*name).size(14.0);
                        let text = if selected { text.strong().color(ACCENT) } else { text.color(p.muted) };
                        let r = ui.add(egui::Button::new(text).frame(false).min_size(egui::vec2(0.0, 30.0)));
                        if selected {
                            let line = egui::Rect::from_min_max(
                                egui::pos2(r.rect.left(), r.rect.bottom() - 2.0),
                                egui::pos2(r.rect.right(), r.rect.bottom() + 1.0),
                            );
                            ui.painter().rect_filled(line, 2.0, ACCENT);
                        }
                        if r.clicked() {
                            self.tab = *tab;
                        }
                        ui.add_space(10.0);
                    }
                });
                ui.add_space(2.0);
            });
    }

    // ---------------- pairing ----------------

    fn pairing_dialog(&mut self, ctx: &egui::Context) {
        let first = lk(&self.app.pending)
            .first()
            .map(|p| (p.id, p.name.clone(), p.addr.clone(), p.code.clone(), p.key.clone()));
        let Some((id, name, addr, code, key)) = first else { return };
        let mut decision = None;
        egui::Modal::new(egui::Id::new("pairing")).show(ctx, |ui| {
            let muted = palette(ui.visuals().dark_mode).muted;
            ui.set_width(380.0);
            ui.label(RichText::new(format!("Pair with “{name}”?")).size(18.0).strong());
            ui.label(RichText::new(format!("{addr} wants to use this keyboard and mouse.")).color(muted));
            ui.add_space(10.0);
            ui.label("Approve only if the same code is showing on that computer:");
            ui.add_space(4.0);
            ui.vertical_centered(|ui| code_boxes(ui, &code));
            ui.add_space(6.0);
            ui.label(RichText::new(format!("Key fingerprint {}", net::fingerprint(&key))).small().color(muted));
            ui.add_space(12.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if primary_button(ui, "Approve").clicked() {
                    decision = Some(true);
                }
                if ui.button("Reject").clicked() {
                    decision = Some(false);
                }
            });
        });
        if let Some(ok) = decision {
            let mut pending = lk(&self.app.pending);
            if let Some(pos) = pending.iter().position(|p| p.id == id) {
                let p = pending.remove(pos);
                let _ = p.reply.send(ok);
            }
        }
    }

    // ---------------- server: Screens ----------------

    fn screens_tab(&mut self, ui: &mut egui::Ui, cfg: &Config) {
        if !cfg!(windows) {
            card(ui, "Sharing from this computer isn't available yet", "", |ui| {
                ui.label("Sharing a keyboard and mouse is Windows-only for now. Choose “Use a shared keyboard & mouse” in Settings.");
            });
            return;
        }
        let connected: HashSet<String> = lk(&self.app.connected).iter().map(|c| c.name.clone()).collect();
        let active = lk(&self.app.active_screen).clone();
        let mons = platform::monitors();
        card(
            ui,
            "Screen layout",
            "Drag a computer against any monitor edge. Moving the mouse off that edge lands at the matching spot on the other screen.",
            |ui| self.layout_canvas(ui, cfg, &mons, &connected, &active),
        );
        ui.add_space(10.0);
        let muted = palette(ui.visuals().dark_mode).muted;
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Tip:").small().strong().color(muted));
            ui.label(
                RichText::new(
                    "Right-click a computer to stretch it corner-to-corner or place it automatically. \
                     Scroll Lock always brings the mouse back here.",
                )
                .small()
                .color(muted),
            );
        });
    }

    /// Display-settings-style editor: the real monitors, plus each client
    /// screen attached to a monitor edge.
    fn layout_canvas(
        &mut self,
        ui: &mut egui::Ui,
        cfg: &Config,
        mons: &[crate::layout::Monitor],
        connected: &HashSet<String>,
        active: &str,
    ) {
        if mons.is_empty() {
            ui.label("No monitors detected.");
            return;
        }
        let dark = ui.visuals().dark_mode;
        let p = palette(dark);
        let mut screens: Vec<ScreenPos> = cfg
            .screens
            .iter()
            .filter(|s| cfg.trusted_clients.iter().any(|t| t.name == s.name))
            .cloned()
            .collect();
        if let Some(d) = &self.dragging {
            if let Some(s) = screens.iter_mut().find(|s| s.name == d.name) {
                *s = d.clone();
            }
        }

        // Fit everything into the card.
        let mut bb = mons[0].rect;
        for m in mons {
            bb = bb.union(&m.rect);
        }
        for s in &screens {
            bb = bb.union(&s.rect());
        }
        let pad = bb.w().max(bb.h()) * 0.06;
        let bb = Rect::new(bb.l - pad, bb.t - pad, bb.r + pad, bb.b + pad);
        let avail = ui.available_width();
        let k = (avail as f64 / bb.w()).min(380.0 / bb.h());
        let size = egui::vec2((bb.w() * k) as f32, (bb.h() * k) as f32);
        let (resp, painter) = ui.allocate_painter(egui::vec2(avail, size.y), Sense::hover());
        let origin = resp.rect.min + egui::vec2((avail - size.x) / 2.0, 0.0);
        let to_px = |r: &Rect| {
            egui::Rect::from_min_max(
                origin + egui::vec2(((r.l - bb.l) * k) as f32, ((r.t - bb.t) * k) as f32),
                origin + egui::vec2(((r.r - bb.l) * k) as f32, ((r.b - bb.t) * k) as f32),
            )
        };
        painter.rect_filled(resp.rect, 10.0, p.canvas);
        // Subtle dot grid.
        let grid = if dark { Color32::from_white_alpha(10) } else { Color32::from_black_alpha(14) };
        let mut gx = resp.rect.left() + 12.0;
        while gx < resp.rect.right() {
            let mut gy = resp.rect.top() + 12.0;
            while gy < resp.rect.bottom() {
                painter.circle_filled(egui::pos2(gx, gy), 1.0, grid);
                gy += 18.0;
            }
            gx += 18.0;
        }

        let mouse_here = active == cfg.name;
        for m in mons {
            let r = to_px(&m.rect).shrink(2.0);
            painter.rect_filled(r, 6.0, p.monitor);
            painter.rect_stroke(r, 6.0, Stroke::new(1.0_f32, p.monitor_edge), egui::StrokeKind::Inside);
            let name = if m.label.is_empty() { "Display" } else { m.label.as_str() };
            let mut fs = (r.height() * 0.15).clamp(11.0, 17.0);
            while fs > 9.0 && name.chars().count() as f32 * fs * 0.6 > r.width() - 10.0 {
                fs -= 1.0;
            }
            painter.text(
                r.center() - egui::vec2(0.0, 7.0),
                egui::Align2::CENTER_CENTER,
                truncate(name, ((r.width() - 10.0) / (fs * 0.6)).max(4.0) as usize),
                FontId::proportional(fs),
                p.text,
            );
            painter.text(
                r.center() + egui::vec2(0.0, 10.0),
                egui::Align2::CENTER_CENTER,
                format!("{}×{}", m.rect.w() as i32, m.rect.h() as i32),
                FontId::proportional(10.5),
                p.muted,
            );
            if m.primary {
                let badge = egui::Rect::from_min_size(r.left_top() + egui::vec2(6.0, 6.0), egui::vec2(38.0, 16.0));
                painter.rect_filled(badge, 8.0, ACCENT.gamma_multiply(0.25));
                painter.text(badge.center(), egui::Align2::CENTER_CENTER, "MAIN", FontId::proportional(9.5), ACCENT);
                if mouse_here {
                    painter.circle_filled(r.right_top() + egui::vec2(-10.0, 10.0), 4.0, OK);
                }
            }
        }

        let mut dropped: Option<ScreenPos> = None;
        // (placement, place automatically instead of snapping)
        let mut refit: Option<(ScreenPos, bool)> = None;
        for s in &screens {
            let r = to_px(&s.rect()).shrink(2.0);
            let online = connected.contains(&s.name);
            let offline = if dark { Color32::from_rgb(74, 81, 98) } else { Color32::from_rgb(132, 140, 156) };
            let fill = if online { ACCENT } else { offline };
            let dragging_this = self.dragging.as_ref().map_or(false, |d| d.name == s.name);
            painter.rect_filled(r, 6.0, if dragging_this { fill.gamma_multiply(0.75) } else { fill });
            if active == s.name {
                painter.rect_stroke(r.expand(2.0), 8.0, Stroke::new(2.5_f32, OK), egui::StrokeKind::Outside);
            }
            painter.text(
                r.center() - egui::vec2(0.0, 7.0),
                egui::Align2::CENTER_CENTER,
                truncate(&s.name, ((r.width() - 10.0) / 7.5).max(6.0) as usize),
                FontId::proportional(13.5),
                Color32::WHITE,
            );
            painter.text(
                r.center() + egui::vec2(0.0, 10.0),
                egui::Align2::CENTER_CENTER,
                format!("{} · {}×{}{}", if online { "online" } else { "offline" }, s.w, s.h, if s.fit { " · stretched" } else { "" }),
                FontId::proportional(10.5),
                Color32::from_white_alpha(220),
            );

            let ir = ui.interact(r, egui::Id::new(("screen", s.name.clone())), Sense::click_and_drag());
            if ir.hovered() || ir.dragged() {
                ui.ctx().set_cursor_icon(if ir.dragged() { egui::CursorIcon::Grabbing } else { egui::CursorIcon::Grab });
            }
            if ir.drag_started() {
                self.dragging = Some(s.clone());
            }
            if ir.dragged() {
                if let Some(d) = &mut self.dragging {
                    let dd = ir.drag_delta();
                    d.x += dd.x as f64 / k;
                    d.y += dd.y as f64 / k;
                }
            }
            if ir.drag_stopped() {
                dropped = self.dragging.take();
            }
            let ir = ir.on_hover_text(format!("{} — drag onto a monitor edge · right-click for options", s.name));
            ir.context_menu(|ui| {
                let mut fit = s.fit;
                if ui.checkbox(&mut fit, "Stretch along the whole edge (corners line up)").clicked() {
                    let mut q = s.clone();
                    q.fit = fit;
                    if !fit {
                        q.scale = 1.0;
                    }
                    refit = Some((q, false));
                    ui.close_menu();
                }
                if ui.button("Place automatically").clicked() {
                    refit = Some((s.clone(), true));
                    ui.close_menu();
                }
            });
        }

        let mut save: Option<ScreenPos> = None;
        let others = |name: &str| -> Vec<Rect> { screens.iter().filter(|o| o.name != name).map(|o| o.rect()).collect() };
        if let Some(d) = dropped {
            // Snap to the nearest monitor edge; a drop that fits nowhere is undone.
            save = crate::layout::snap(mons, &others(&d.name), &d);
        }
        if let Some((q, auto)) = refit {
            save = if auto {
                Some(crate::layout::auto_place(mons, &others(&q.name), &q.name, q.w, q.h))
            } else {
                crate::layout::snap(mons, &others(&q.name), &q)
            };
        }
        if let Some(pos) = save {
            self.app.update_config(|c| {
                c.screens.retain(|s| s.name != pos.name);
                c.screens.push(pos);
            });
        }

        // Legend + computers not placed yet.
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            let swatch = |ui: &mut egui::Ui, c: Color32, t: &str| {
                let (r, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), Sense::hover());
                ui.painter().rect_filled(r, 3.0, c);
                ui.label(RichText::new(t).small().color(p.muted));
                ui.add_space(8.0);
            };
            swatch(ui, p.monitor, "This PC's monitors");
            swatch(ui, ACCENT, "Online");
            swatch(ui, if dark { Color32::from_rgb(74, 81, 98) } else { Color32::from_rgb(132, 140, 156) }, "Offline");
            let (r, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), Sense::hover());
            ui.painter().rect_stroke(r.shrink(1.0), 3.0, Stroke::new(2.0_f32, OK), egui::StrokeKind::Inside);
            ui.label(RichText::new("Mouse is here").small().color(p.muted));
        });
        let unplaced: Vec<&str> = cfg
            .trusted_clients
            .iter()
            .map(|t| t.name.as_str())
            .filter(|n| !screens.iter().any(|s| s.name == *n))
            .collect();
        if !unplaced.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Not placed yet:").color(p.muted));
                for n in unplaced {
                    if ui.small_button(format!("Place {n}")).clicked() {
                        let pos = crate::layout::auto_place(mons, &others(n), n, 0, 0);
                        self.app.update_config(|c| {
                            c.screens.retain(|s| s.name != pos.name);
                            c.screens.push(pos);
                        });
                    }
                }
            });
        }
    }

    // ---------------- server: Computers ----------------

    fn computers_tab(&mut self, ui: &mut egui::Ui, cfg: &Config) {
        let p = palette(ui.visuals().dark_mode);
        if cfg.trusted_clients.is_empty() {
            card(ui, "No computers yet", "", |ui| {
                ui.label("Add a computer in three steps:");
                ui.add_space(4.0);
                for (n, step) in [
                    "Install Edgehop on the other computer.",
                    "Open it and choose “Use a shared keyboard & mouse”. It finds this PC on its own.",
                    "Approve the pairing request that pops up here (check that the codes match).",
                ]
                .iter()
                .enumerate()
                {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("{}", n + 1)).strong().color(ACCENT));
                        ui.label(*step);
                    });
                }
            });
            return;
        }
        let infos: Vec<ConnectedInfo> = lk(&self.app.connected).clone();
        let mut forget: Option<String> = None;
        for peer in &cfg.trusted_clients {
            let info = infos.iter().find(|i| i.name == peer.name);
            card(ui, "", "", |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&peer.name).size(16.0).strong());
                            match info {
                                Some(_) => pill(ui, OK, "Online"),
                                None => pill(ui, IDLE, "Offline"),
                            }
                        });
                        if let Some(i) = info {
                            ui.label(
                                RichText::new(format!("{} · {}×{} · {}", os_name(&i.os), i.width, i.height, i.addr))
                                    .color(p.muted),
                            );
                        }
                        ui.label(RichText::new(format!("Key {}", net::fingerprint(&peer.key))).small().monospace().color(p.muted));
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.confirm_forget.as_deref() == Some(peer.key.as_str()) {
                            if danger_button(ui, "Forget it").clicked() {
                                forget = Some(peer.key.clone());
                            }
                            if ui.button("Keep").clicked() {
                                self.confirm_forget = None;
                            }
                            ui.label(RichText::new("It will need to pair again.").small().color(p.muted));
                        } else if ui.button("Forget…").clicked() {
                            self.confirm_forget = Some(peer.key.clone());
                        }
                    });
                });
            });
            ui.add_space(8.0);
        }
        if let Some(k) = forget {
            self.confirm_forget = None;
            let name = cfg.trusted_clients.iter().find(|p| p.key == k).map(|p| p.name.clone());
            self.app.update_config(|c| {
                c.trusted_clients.retain(|p| p.key != k);
                if let Some(n) = &name {
                    c.screens.retain(|s| &s.name != n);
                }
            });
            if self.app.is_running() {
                self.restart();
            }
        }
    }

    // ---------------- client: Connection ----------------

    fn connection_tab(&mut self, ui: &mut egui::Ui, cfg: &Config) {
        let p = palette(ui.visuals().dark_mode);
        let status = lk(&self.app.status).clone();
        let ok = *lk(&self.app.connected_ok);
        let code = lk(&self.app.pairing_code).clone();

        card(ui, "", "", |ui| {
            if let Some(code) = &code {
                ui.label(RichText::new("Waiting for approval").size(18.0).strong());
                ui.label(RichText::new("Approve this computer on the server. It should show this same code:").color(p.muted));
                ui.add_space(8.0);
                code_boxes(ui, code);
                return;
            }
            ui.horizontal(|ui| {
                let (color, title) = if ok {
                    (OK, cfg.trusted_server.as_ref().map(|t| format!("Connected to {}", t.name)).unwrap_or("Connected".into()))
                } else if self.app.is_running() {
                    (WARN, "Not connected".to_string())
                } else {
                    (IDLE, "Stopped".to_string())
                };
                dot(ui, color, 7.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(RichText::new(title).size(18.0).strong());
                    ui.label(RichText::new(&status).color(p.muted));
                });
            });
            if let Some(t) = &cfg.trusted_server {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("Paired with {} · key {}", t.name, net::fingerprint(&t.key))).small().color(p.muted));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Forget server").clicked() {
                            self.app.update_config(|c| c.trusted_server = None);
                            *lk(&self.app.key_mismatch) = None;
                            self.restart();
                        }
                    });
                });
            }
        });

        if let Some(fp) = lk(&self.app.key_mismatch).clone() {
            ui.add_space(10.0);
            card(ui, "⚠ The server's identity changed", "", |ui| {
                ui.label(format!(
                    "Expected key {}, got {fp}. If you reinstalled Edgehop on the server this is expected. \
                     Click “Forget server” to pair again. Otherwise something may be impersonating it.",
                    cfg.trusted_server.as_ref().map(|t| net::fingerprint(&t.key)).unwrap_or_default()
                ));
            });
        }

        ui.add_space(10.0);
        card(ui, "Server", "Leave the address empty to find the server automatically on your network.", |ui| {
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.addr_edit)
                        .hint_text("Automatic")
                        .desired_width(260.0),
                );
                if self.addr_edit.trim() != cfg.server_address && primary_button(ui, "Save").clicked() {
                    let a = self.addr_edit.trim().to_string();
                    self.app.update_config(|c| c.server_address = a);
                    self.restart();
                }
                if !cfg.server_address.is_empty() && ui.button("Use automatic").clicked() {
                    self.addr_edit.clear();
                    self.app.update_config(|c| c.server_address.clear());
                    self.restart();
                }
            });
            let found = lk(&self.app.discovered).clone();
            if !found.is_empty() {
                ui.add_space(6.0);
                ui.label(RichText::new("Found on your network").small().strong().color(p.muted));
                for d in found {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&d.info.name).strong());
                        ui.label(RichText::new(d.addr.to_string()).color(p.muted));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let current = cfg.trusted_server.as_ref().map_or(false, |t| t.key == d.info.key);
                            if current {
                                pill(ui, OK, "Paired");
                            } else if ui.small_button("Connect").clicked() {
                                let a = d.addr.to_string();
                                self.addr_edit = a.clone();
                                self.app.update_config(|c| c.server_address = a);
                                self.restart();
                            }
                        });
                    });
                }
            }
        });
    }

    // ---------------- settings ----------------

    fn settings_tab(&mut self, ui: &mut egui::Ui, cfg: &Config) {
        let p = palette(ui.visuals().dark_mode);

        card(ui, "This computer", "", |ui| {
            row(ui, "Name", "How other computers see this one.", |ui| {
                let changed = self.name_edit.trim() != cfg.name && !self.name_edit.trim().is_empty();
                if ui.add_enabled(changed, egui::Button::new("Rename")).clicked() {
                    let new = self.name_edit.trim().to_string();
                    self.app.update_config(|c| c.name = new);
                    if self.app.is_running() {
                        self.restart();
                    }
                }
                ui.add(egui::TextEdit::singleline(&mut self.name_edit).desired_width(200.0));
            });
            ui.add_space(6.0);
            ui.label("Role");
            ui.horizontal(|ui| {
                let mut role = cfg.role;
                for (r, title, sub) in [
                    (Role::Server, "Share this keyboard & mouse", "This computer controls the others"),
                    (Role::Client, "Use a shared keyboard & mouse", "Controlled by another computer"),
                ] {
                    let selected = role == r;
                    let frame = egui::Frame::new()
                        .fill(if selected { ACCENT.gamma_multiply(0.14) } else { p.canvas })
                        .stroke(Stroke::new(if selected { 1.5_f32 } else { 1.0_f32 }, if selected { ACCENT } else { p.border }))
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(Margin::symmetric(14, 10));
                    let resp = frame
                        .show(ui, |ui| {
                            ui.set_width(230.0);
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 2.0;
                                ui.label(RichText::new(title).strong().color(if selected { ACCENT } else { p.text }));
                                ui.label(RichText::new(sub).small().color(p.muted));
                            });
                        })
                        .response
                        .interact(Sense::click());
                    if resp.clicked() {
                        role = r;
                    }
                }
                if role != cfg.role {
                    self.app.update_config(|c| c.role = role);
                    self.restart();
                }
            });
            if cfg.role == Role::Server && !cfg!(windows) {
                ui.colored_label(WARN, "Sharing is Windows-only for now; use the other role on this computer.");
            }
            ui.add_space(4.0);
            ui.label(RichText::new(format!("Device key {}", net::fingerprint(&cfg.public_key))).small().monospace().color(p.muted));
        });
        ui.add_space(10.0);

        match cfg.role {
            Role::Server => {
                card(ui, "Mouse", "", |ui| {
                    row(ui, "Speed on other screens", "Multiplies pointer movement on the other computers.", |ui| {
                        let mut speed = cfg.mouse_speed;
                        if ui.add(egui::Slider::new(&mut speed, 0.3..=3.0).step_by(0.05).suffix("×")).changed() {
                            self.app.update_config(|c| c.mouse_speed = speed);
                        }
                    });
                    row(
                        ui,
                        "Edge resistance",
                        "How hard to push past another computer's edge before coming back. 0 = instant.",
                        |ui| {
                            let mut res = cfg.edge_resistance;
                            if ui.add(egui::Slider::new(&mut res, 0..=400).suffix(" px")).changed() {
                                self.app.update_config(|c| c.edge_resistance = res);
                            }
                        },
                    );
                });
                ui.add_space(10.0);
                card(ui, "Sharing", "", |ui| {
                    self.clipboard_row(ui, cfg);
                    row(ui, "Hide cursor while away", "Hide this PC's cursor while you're on another computer.", |ui| {
                        let mut hide = cfg.hide_cursor_when_away;
                        if toggle(ui, &mut hide).changed() {
                            self.app.update_config(|c| c.hide_cursor_when_away = hide);
                        }
                    });
                });
            }
            Role::Client => {
                card(ui, "While the mouse is on another computer", "", |ui| {
                    row(ui, "Hide the pointer", "", |ui| {
                        let mut hide = cfg.hide_pointer_when_away;
                        if toggle(ui, &mut hide).changed() {
                            self.app.update_config(|c| c.hide_pointer_when_away = hide);
                        }
                    });
                    row(ui, "Dim this screen", "Never while something is playing.", |ui| {
                        let mut dim = cfg.dim_when_away;
                        let mut level = cfg.dim_level;
                        // Right-to-left: the switch sits at the far right like the others.
                        if toggle(ui, &mut dim).changed() {
                            self.app.update_config(|c| c.dim_when_away = dim);
                        }
                        if ui.add_enabled(dim, egui::Slider::new(&mut level, 5..=90).suffix("%")).changed() {
                            self.app.update_config(|c| c.dim_level = level);
                        }
                    });
                });
                ui.add_space(10.0);
                card(ui, "Sharing", "", |ui| {
                    self.clipboard_row(ui, cfg);
                    row(
                        ui,
                        "Screen size override",
                        "Only affects pointer speed here. Use the scaled (logical) resolution.",
                        |ui| {
                            let mut on = cfg.screen_override.is_some();
                            if toggle(ui, &mut on).changed() {
                                self.app.update_config(|c| c.screen_override = if on { Some((1920, 1080)) } else { None });
                                self.restart();
                            }
                            if let Some((mut w, mut h)) = cfg.screen_override {
                                let b = ui.add(egui::DragValue::new(&mut h).range(240..=16000));
                                ui.label("×");
                                let a = ui.add(egui::DragValue::new(&mut w).range(320..=16000));
                                if (a.drag_stopped() || a.lost_focus() || b.drag_stopped() || b.lost_focus())
                                    && Some((w, h)) != cfg.screen_override
                                {
                                    self.app.update_config(|c| c.screen_override = Some((w, h)));
                                    self.restart();
                                } else if a.changed() || b.changed() {
                                    lk(&self.app.config).screen_override = Some((w, h));
                                }
                            }
                        },
                    );
                });
            }
        }
        ui.add_space(10.0);

        card(ui, "Startup", "", |ui| {
            row(ui, "Start Edgehop when I log in", "", |ui| {
                let mut auto = cfg.autostart;
                if toggle(ui, &mut auto).changed() {
                    match crate::autostart::set(auto) {
                        Ok(()) => {
                            self.app.update_config(|c| c.autostart = auto);
                            self.message = None;
                        }
                        Err(e) => self.message = Some(format!("Couldn't change start-at-login: {e}")),
                    }
                }
            });
            if cfg.role == Role::Server {
                row(ui, "Network port", "Restart sharing after changing.", |ui| {
                    let mut port = cfg.port;
                    if ui.add(egui::DragValue::new(&mut port).range(1024..=65000)).changed() {
                        self.app.update_config(|c| c.port = port);
                    }
                });
            }
            if let Some(m) = &self.message {
                ui.colored_label(WARN, m);
            }
            if self.tray.is_some() {
                ui.label(
                    RichText::new(if HIDE_TO_TRAY {
                        "Closing the window keeps Edgehop running in the system tray."
                    } else {
                        "Closing the window minimizes it; quit from the tray icon."
                    })
                    .small()
                    .color(p.muted),
                );
            }
        });
    }

    fn clipboard_row(&mut self, ui: &mut egui::Ui, cfg: &Config) {
        row(ui, "Shared clipboard", "Copy on one computer, paste on another: text and images.", |ui| {
            let mut clip = cfg.clipboard;
            if toggle(ui, &mut clip).changed() {
                self.app.update_config(|c| c.clipboard = clip);
            }
        });
    }

    // ---------------- activity ----------------

    fn activity_tab(&mut self, ui: &mut egui::Ui) {
        let p = palette(ui.visuals().dark_mode);
        let logs: Vec<String> = lk(&self.app.logs).iter().cloned().collect();
        card(ui, "Activity", "What Edgehop has been doing, newest at the bottom.", |ui| {
            ui.horizontal(|ui| {
                if ui.button("Copy log").clicked() {
                    ui.ctx().copy_text(logs.join("\n"));
                }
                if ui.button("Open log folder").clicked() {
                    open_folder(&crate::state::log_path());
                }
            });
            ui.add_space(6.0);
            egui::Frame::new()
                .fill(p.canvas)
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::same(10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    egui::ScrollArea::vertical().max_height(420.0).stick_to_bottom(true).show(ui, |ui| {
                        for l in &logs {
                            let color = if l.contains(" ERROR ") {
                                BAD
                            } else if l.contains(" WARN ") {
                                WARN
                            } else {
                                p.text
                            };
                            ui.label(RichText::new(l).monospace().color(color));
                        }
                    });
                });
        });
    }
}

/// iOS-style switch.
fn toggle(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let size = egui::vec2(38.0, 22.0);
    let (rect, mut resp) = ui.allocate_exact_size(size, Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool_responsive(resp.id, *on);
    let off = if ui.visuals().dark_mode { Color32::from_rgb(60, 66, 80) } else { Color32::from_rgb(200, 205, 215) };
    let bg = egui::lerp(egui::Rgba::from(off)..=egui::Rgba::from(ACCENT), t);
    ui.painter().rect_filled(rect, 11.0, Color32::from(bg));
    let x = egui::lerp(rect.left() + 11.0..=rect.right() - 11.0, t);
    ui.painter().circle_filled(egui::pos2(x, rect.center().y), 8.0, Color32::WHITE);
    resp
}

fn os_name(os: &str) -> &str {
    match os {
        "linux" => "Linux",
        "windows" => "Windows",
        "macos" => "macOS",
        other => other,
    }
}

fn open_folder(file: &std::path::Path) {
    let dir = file.parent().unwrap_or(file);
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer").arg(dir).spawn();
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(dir).spawn();
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n.saturating_sub(1)).collect::<String>())
    }
}
