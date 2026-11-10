//! The control window: role, screen layout editor, pairing, and status.
use crate::config::{Config, Role};
use crate::layout::{Rect, ScreenPos};
use crate::net;
use crate::platform::{self, HIDE_TO_TRAY, WINDOW_TITLE};
use crate::state::{lk, AppState};
use crate::tray;
use eframe::egui::{self, Color32, RichText, Sense, Stroke};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

const GREEN: Color32 = Color32::from_rgb(34, 160, 90);
const AMBER: Color32 = Color32::from_rgb(234, 179, 8);
const BLUE: Color32 = Color32::from_rgb(37, 99, 235);

pub fn run(app: Arc<AppState>, minimized: bool) -> eframe::Result<()> {
    let icon = egui::IconData { rgba: crate::icon::rgba(64), width: 64, height: 64 };
    let mut vp = egui::ViewportBuilder::default()
        .with_title(WINDOW_TITLE)
        .with_inner_size([780.0, 700.0])
        .with_min_inner_size([600.0, 480.0])
        .with_icon(Arc::new(icon));
    if minimized {
        if HIDE_TO_TRAY {
            vp = vp.with_visible(false);
        }
    }
    let options = eframe::NativeOptions { viewport: vp, ..Default::default() };
    eframe::run_native("kvmlink", options, Box::new(move |cc| Ok(Box::new(Gui::new(cc, app, minimized)))))
}

struct Gui {
    app: Arc<AppState>,
    tray: Option<tray::Tray>,
    tray_tried: bool,
    name_edit: String,
    addr_edit: String,
    message: Option<String>,
    start_minimized: bool,
    dragging: Option<ScreenPos>,
}

impl Gui {
    fn new(cc: &eframe::CreationContext<'_>, app: Arc<AppState>, minimized: bool) -> Gui {
        *lk(&app.ctx) = Some(cc.egui_ctx.clone());
        let cfg = app.cfg();
        let mut style = (*cc.egui_ctx.style()).clone();
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        cc.egui_ctx.set_style(style);
        Gui {
            app,
            tray: None,
            tray_tried: false,
            name_edit: cfg.name.clone(),
            addr_edit: cfg.server_address.clone(),
            message: None,
            start_minimized: minimized && !HIDE_TO_TRAY,
            dragging: None,
        }
    }

    fn restart(&self) {
        self.app.start_engine();
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
        self.top_bar(ctx, &cfg);
        self.pairing_dialog(ctx);

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                self.this_computer(ui, &cfg);
                ui.add_space(6.0);
                match cfg.role {
                    Role::Server => self.server_panel(ui, &cfg),
                    Role::Client => self.client_panel(ui, &cfg),
                }
                ui.add_space(6.0);
                self.options(ui, &cfg);
                ui.add_space(6.0);
                self.log_panel(ui);
            });
        });

        ctx.request_repaint_after(Duration::from_millis(500));
    }
}

impl Gui {
    fn top_bar(&mut self, ctx: &egui::Context, cfg: &Config) {
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("kvmlink").size(22.0).strong());
                ui.separator();
                let ok = *lk(&self.app.connected_ok);
                let running = self.app.is_running();
                let dot = if !running {
                    Color32::GRAY
                } else if ok {
                    GREEN
                } else {
                    AMBER
                };
                ui.label(RichText::new("●").color(dot).size(16.0));
                ui.label(lk(&self.app.status).clone());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if running {
                        if ui.button("Stop").clicked() {
                            self.app.stop_engine();
                        }
                    } else if ui.button(RichText::new("Start").strong()).clicked() {
                        self.restart();
                    }
                    let active = lk(&self.app.active_screen).clone();
                    if running && !active.is_empty() && cfg.role == Role::Server {
                        ui.label(RichText::new(format!("Mouse is on: {active}")).color(BLUE));
                    }
                });
            });
            ui.add_space(4.0);
        });
    }

    fn pairing_dialog(&mut self, ctx: &egui::Context) {
        let first = lk(&self.app.pending)
            .first()
            .map(|p| (p.id, p.name.clone(), p.addr.clone(), p.code.clone(), p.key.clone()));
        let Some((id, name, addr, code, key)) = first else { return };
        let mut decision = None;
        egui::Window::new("New computer wants to connect")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(format!("\"{name}\" at {addr} wants to use this keyboard and mouse."));
                ui.label("Approve only if this code matches the one shown on that computer:");
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new(&code).size(36.0).strong().monospace());
                });
                ui.label(RichText::new(format!("Key fingerprint {}", net::fingerprint(&key))).small().weak());
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button(RichText::new("  Approve  ").strong()).clicked() {
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

    fn this_computer(&mut self, ui: &mut egui::Ui, cfg: &Config) {
        ui.group(|ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("This computer").strong());
            ui.horizontal(|ui| {
                ui.label("Name");
                let r = ui.add(egui::TextEdit::singleline(&mut self.name_edit).desired_width(200.0));
                let new = self.name_edit.trim().to_string();
                if (r.lost_focus() || ui.button("Rename").clicked()) && !new.is_empty() && new != cfg.name {
                    self.app.update_config(|c| {
                        c.name = new.clone();
                    });
                    if self.app.is_running() {
                        self.restart();
                    }
                }
                ui.label(RichText::new(format!("ID {}", net::fingerprint(&cfg.public_key))).weak().small());
            });
            ui.horizontal(|ui| {
                let mut role = cfg.role;
                ui.selectable_value(&mut role, Role::Server, "🖱 Share my keyboard & mouse (server)");
                ui.selectable_value(&mut role, Role::Client, "🖥 Use another computer's keyboard & mouse (client)");
                if role != cfg.role {
                    self.app.update_config(|c| c.role = role);
                    self.restart();
                }
            });
            if cfg.role == Role::Server && !cfg!(windows) {
                ui.colored_label(AMBER, "Server mode is Windows-only in this version. Use client mode here.");
            }
        });
    }

    // ---------------- server ----------------

    /// Display-settings-style editor: the real monitors, plus each client
    /// screen attached to a monitor edge.
    fn layout_editor(&mut self, ui: &mut egui::Ui, cfg: &Config, connected: &HashSet<String>, active: &str) {
        let mons = platform::monitors();
        ui.group(|ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("Screen layout").strong());
            ui.label(
                RichText::new(
                    "Drag a computer against the edge of any monitor; it snaps on. Moving the mouse off that \
                     edge lands at the matching spot on the other screen. Right-click a screen for options. \
                     Scroll Lock always brings you back here.",
                )
                .weak(),
            );
            if mons.is_empty() {
                ui.label("No monitors detected.");
                return;
            }

            let mut screens: Vec<ScreenPos> = cfg
                .screens
                .iter()
                .filter(|p| cfg.trusted_clients.iter().any(|t| t.name == p.name))
                .cloned()
                .collect();
            if let Some(d) = &self.dragging {
                if let Some(p) = screens.iter_mut().find(|p| p.name == d.name) {
                    *p = d.clone();
                }
            }

            // Fit everything into the panel.
            let mut bb = mons[0].rect;
            for m in &mons {
                bb = bb.union(&m.rect);
            }
            for p in &screens {
                bb = bb.union(&p.rect());
            }
            let pad = bb.w().max(bb.h()) * 0.05;
            let bb = Rect::new(bb.l - pad, bb.t - pad, bb.r + pad, bb.b + pad);
            let avail = ui.available_width();
            let k = (avail as f64 / bb.w()).min(360.0 / bb.h());
            let size = egui::vec2((bb.w() * k) as f32, (bb.h() * k) as f32);
            let (resp, painter) = ui.allocate_painter(egui::vec2(avail, size.y), Sense::hover());
            let origin = resp.rect.min + egui::vec2((avail - size.x) / 2.0, 0.0);
            let to_px = |r: &Rect| {
                egui::Rect::from_min_max(
                    origin + egui::vec2(((r.l - bb.l) * k) as f32, ((r.t - bb.t) * k) as f32),
                    origin + egui::vec2(((r.r - bb.l) * k) as f32, ((r.b - bb.t) * k) as f32),
                )
            };
            let dark = ui.visuals().dark_mode;
            painter.rect_filled(resp.rect, 8.0, ui.visuals().extreme_bg_color);

            for m in &mons {
                let r = to_px(&m.rect).shrink(1.5);
                let here = active == cfg.name;
                painter.rect_filled(r, 4.0, if dark { Color32::from_gray(68) } else { Color32::from_gray(205) });
                painter.rect_stroke(
                    r,
                    4.0,
                    Stroke::new(1.0_f32, if dark { Color32::from_gray(100) } else { Color32::from_gray(150) }),
                    egui::StrokeKind::Inside,
                );
                let text = if dark { Color32::WHITE } else { Color32::from_gray(30) };
                let name = if m.label.is_empty() { "Display" } else { m.label.as_str() };
                // Shrink the font until the model name fits the tile.
                let mut size = (r.height() * 0.16).clamp(11.0, 18.0);
                while size > 9.0 && name.chars().count() as f32 * size * 0.62 > r.width() - 8.0 {
                    size -= 1.0;
                }
                painter.text(
                    r.center() - egui::vec2(0.0, 6.0),
                    egui::Align2::CENTER_CENTER,
                    truncate(name, ((r.width() - 8.0) / (size * 0.62)).max(4.0) as usize),
                    egui::FontId::proportional(size),
                    text,
                );
                painter.text(
                    egui::pos2(r.center().x, r.bottom() - 10.0),
                    egui::Align2::CENTER_CENTER,
                    format!(
                        "{}×{}{}",
                        m.rect.w() as i32,
                        m.rect.h() as i32,
                        if m.primary && here { " · main · mouse here" } else if m.primary { " · main" } else { "" }
                    ),
                    egui::FontId::proportional(10.0),
                    text.gamma_multiply(0.7),
                );
            }

            let mut dropped: Option<ScreenPos> = None;
            // (placement, place automatically instead of snapping)
            let mut refit: Option<(ScreenPos, bool)> = None;
            for p in &screens {
                let r = to_px(&p.rect()).shrink(1.5);
                let online = connected.contains(&p.name);
                let fill = if online { GREEN } else { Color32::from_gray(125) };
                painter.rect_filled(r, 4.0, fill.gamma_multiply(if self.dragging.is_some() { 0.8 } else { 1.0 }));
                if active == p.name {
                    painter.rect_stroke(r.expand(1.5), 5.0, Stroke::new(2.5_f32, AMBER), egui::StrokeKind::Outside);
                }
                painter.text(
                    r.center() - egui::vec2(0.0, 6.0),
                    egui::Align2::CENTER_CENTER,
                    truncate(&p.name, 24),
                    egui::FontId::proportional(13.0),
                    Color32::WHITE,
                );
                painter.text(
                    r.center() + egui::vec2(0.0, 9.0),
                    egui::Align2::CENTER_CENTER,
                    format!(
                        "{}×{} · {}{}",
                        p.w,
                        p.h,
                        if online { "online" } else { "offline" },
                        if p.fit { " · stretched" } else { "" }
                    ),
                    egui::FontId::proportional(10.0),
                    Color32::from_white_alpha(215),
                );

                let ir = ui.interact(r, egui::Id::new(("screen", p.name.clone())), Sense::click_and_drag());
                if ir.drag_started() {
                    self.dragging = Some(p.clone());
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
                let ir = ir.on_hover_text(format!("{} — drag onto a monitor edge, right-click for options", p.name));
                ir.context_menu(|ui| {
                    let mut fit = p.fit;
                    if ui
                        .checkbox(&mut fit, "Stretch to the whole monitor edge (corners line up)")
                        .clicked()
                    {
                        let mut q = p.clone();
                        q.fit = fit;
                        if !fit {
                            q.scale = 1.0;
                        }
                        refit = Some((q, false));
                        ui.close_menu();
                    }
                    if ui.button("Place automatically").clicked() {
                        refit = Some((p.clone(), true));
                        ui.close_menu();
                    }
                });
            }

            let mut save: Option<ScreenPos> = None;
            let others = |name: &str| -> Vec<Rect> {
                screens.iter().filter(|o| o.name != name).map(|o| o.rect()).collect()
            };
            if let Some(d) = dropped {
                // Snap to the nearest monitor edge; a drop that fits nowhere is undone.
                save = crate::layout::snap(&mons, &others(&d.name), &d);
            }
            if let Some((q, auto)) = refit {
                if auto {
                    save = Some(crate::layout::auto_place(&mons, &others(&q.name), &q.name, q.w, q.h));
                } else {
                    save = crate::layout::snap(&mons, &others(&q.name), &q);
                }
            }
            if let Some(pos) = save {
                self.app.update_config(|c| {
                    c.screens.retain(|p| p.name != pos.name);
                    c.screens.push(pos);
                });
            }

            let unplaced: Vec<&str> = cfg
                .trusted_clients
                .iter()
                .map(|t| t.name.as_str())
                .filter(|n| !screens.iter().any(|p| p.name == *n))
                .collect();
            if !unplaced.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Not placed yet (placed automatically when they connect):").weak());
                    for n in unplaced {
                        if ui.small_button(format!("Place {n}")).clicked() {
                            let pos = crate::layout::auto_place(&mons, &others(n), n, 0, 0);
                            self.app.update_config(|c| {
                                c.screens.retain(|p| p.name != pos.name);
                                c.screens.push(pos);
                            });
                        }
                    }
                });
            }
        });
    }


    fn server_panel(&mut self, ui: &mut egui::Ui, cfg: &Config) {
        let connected: HashSet<String> = lk(&self.app.connected).iter().map(|c| c.name.clone()).collect();
        let active = lk(&self.app.active_screen).clone();
        self.layout_editor(ui, cfg, &connected, &active);

        ui.add_space(6.0);
        ui.group(|ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("Paired computers").strong());
            if cfg.trusted_clients.is_empty() {
                ui.label(
                    RichText::new(
                        "None yet. Install kvmlink on another computer and choose client mode. \
                         It finds this PC by itself, and a pairing prompt will appear here.",
                    )
                    .weak(),
                );
            }
            let infos = lk(&self.app.connected).clone();
            let mut forget = None;
            egui::Grid::new("clients").striped(true).spacing([14.0, 4.0]).show(ui, |ui| {
                for p in &cfg.trusted_clients {
                    let info = infos.iter().find(|i| i.name == p.name);
                    let (dot, state) = match info {
                        Some(i) => (GREEN, format!("online · {} · {}×{} · {}", i.addr, i.width, i.height, i.os)),
                        None => (Color32::GRAY, "offline".to_string()),
                    };
                    ui.label(RichText::new("●").color(dot));
                    ui.label(RichText::new(&p.name).strong());
                    ui.label(state);
                    ui.label(RichText::new(net::fingerprint(&p.key)).weak().small());
                    if ui.small_button("Forget").clicked() {
                        forget = Some(p.key.clone());
                    }
                    ui.end_row();
                }
            });
            if let Some(k) = forget {
                let name = cfg.trusted_clients.iter().find(|p| p.key == k).map(|p| p.name.clone());
                self.app.update_config(|c| {
                    c.trusted_clients.retain(|p| p.key != k);
                    if let Some(n) = &name {
                        c.screens.retain(|p| &p.name != n);
                    }
                });
                if self.app.is_running() {
                    self.restart();
                }
            }
        });

        ui.add_space(6.0);
        ui.group(|ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label("Mouse speed on other screens");
                let mut speed = cfg.mouse_speed;
                if ui.add(egui::Slider::new(&mut speed, 0.3..=3.0).step_by(0.05)).changed() {
                    self.app.update_config(|c| c.mouse_speed = speed);
                }
            });
            ui.horizontal(|ui| {
                ui.label("Edge resistance");
                let mut res = cfg.edge_resistance;
                if ui
                    .add(egui::Slider::new(&mut res, 0..=400).suffix(" px"))
                    .on_hover_text(
                        "How far you have to keep pushing past the edge of another computer's screen \
                         before the mouse jumps back. Raise it if you switch by accident when clicking \
                         near an edge; 0 switches instantly.",
                    )
                    .changed()
                {
                    self.app.update_config(|c| c.edge_resistance = res);
                }
            });
            ui.horizontal(|ui| {
                ui.label("Port");
                let mut port = cfg.port;
                if ui.add(egui::DragValue::new(&mut port).range(1024..=65000)).changed() {
                    self.app.update_config(|c| c.port = port);
                }
                ui.label(RichText::new("(restart sharing after changing)").weak().small());
            });
        });
    }

    // ---------------- client ----------------

    fn client_panel(&mut self, ui: &mut egui::Ui, cfg: &Config) {
        if let Some(code) = lk(&self.app.pairing_code).clone() {
            ui.group(|ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("Pairing").strong());
                ui.label("Approve this computer on the server. Check that the server shows the same code:");
                ui.vertical_centered(|ui| ui.label(RichText::new(code).size(40.0).strong().monospace()));
            });
            ui.add_space(6.0);
        }
        if let Some(fp) = lk(&self.app.key_mismatch).clone() {
            ui.group(|ui| {
                ui.set_width(ui.available_width());
                ui.colored_label(Color32::from_rgb(220, 60, 60), "⚠ The server's identity key has changed.");
                ui.label(format!(
                    "Expected {}, got {fp}. If you reinstalled kvmlink on the server this is expected: click \
                     \"Forget server\" to pair again. Otherwise someone may be impersonating it.",
                    cfg.trusted_server.as_ref().map(|t| net::fingerprint(&t.key)).unwrap_or_default()
                ));
            });
            ui.add_space(6.0);
        }

        ui.group(|ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("Server").strong());
            match &cfg.trusted_server {
                Some(t) => {
                    ui.horizontal(|ui| {
                        ui.label("Paired with");
                        ui.label(RichText::new(&t.name).strong());
                        ui.label(RichText::new(net::fingerprint(&t.key)).weak().small());
                        if ui.small_button("Forget server").clicked() {
                            self.app.update_config(|c| c.trusted_server = None);
                            *lk(&self.app.key_mismatch) = None;
                            self.restart();
                        }
                    });
                }
                None => {
                    ui.label(RichText::new("Not paired yet").weak());
                }
            }
            ui.horizontal(|ui| {
                ui.label("Address");
                ui.add(
                    egui::TextEdit::singleline(&mut self.addr_edit)
                        .hint_text("automatic (find on the network)")
                        .desired_width(240.0),
                );
                if self.addr_edit.trim() != cfg.server_address && ui.button("Save").clicked() {
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
                ui.add_space(4.0);
                ui.label(RichText::new("Servers found on your network").weak());
                for d in found {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&d.info.name).strong());
                        ui.label(d.addr.to_string());
                        ui.label(RichText::new(net::fingerprint(&d.info.key)).weak().small());
                        let current = cfg.trusted_server.as_ref().map(|t| t.key == d.info.key).unwrap_or(false);
                        if current {
                            ui.label(RichText::new("paired").color(GREEN));
                        } else if ui.small_button("Connect").clicked() {
                            let a = d.addr.to_string();
                            self.addr_edit = a.clone();
                            self.app.update_config(|c| c.server_address = a);
                            self.restart();
                        }
                    });
                }
            }
        });

        ui.add_space(6.0);
        ui.group(|ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let mut on = cfg.screen_override.is_some();
                if ui.checkbox(&mut on, "Override screen size").changed() {
                    self.app.update_config(|c| c.screen_override = if on { Some((1920, 1080)) } else { None });
                    self.restart();
                }
                if let Some((mut w, mut h)) = cfg.screen_override {
                    let a = ui.add(egui::DragValue::new(&mut w).range(320..=16000));
                    ui.label("×");
                    let b = ui.add(egui::DragValue::new(&mut h).range(240..=16000));
                    if (a.drag_stopped() || a.lost_focus() || b.drag_stopped() || b.lost_focus())
                        && Some((w, h)) != cfg.screen_override
                    {
                        self.app.update_config(|c| c.screen_override = Some((w, h)));
                        self.restart();
                    } else if a.changed() || b.changed() {
                        lk(&self.app.config).screen_override = Some((w, h));
                    }
                }
            });
            ui.label(
                RichText::new("Only changes how fast the pointer moves here. Use your scaled (logical) resolution.")
                    .weak()
                    .small(),
            );
        });
    }

    // ---------------- common ----------------

    fn options(&mut self, ui: &mut egui::Ui, cfg: &Config) {
        ui.group(|ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("Options").strong());
            let mut clip = cfg.clipboard;
            if ui.checkbox(&mut clip, "Share clipboard text between computers").changed() {
                self.app.update_config(|c| c.clipboard = clip);
            }
            match cfg.role {
                Role::Server => {
                    let mut hide = cfg.hide_cursor_when_away;
                    if ui.checkbox(&mut hide, "Hide this computer's cursor while I'm using another one").changed() {
                        self.app.update_config(|c| c.hide_cursor_when_away = hide);
                    }
                }
                Role::Client => {
                    let mut hide = cfg.hide_pointer_when_away;
                    if ui.checkbox(&mut hide, "Hide the pointer here while the mouse is on another computer").changed() {
                        self.app.update_config(|c| c.hide_pointer_when_away = hide);
                    }
                    ui.horizontal(|ui| {
                        let mut dim = cfg.dim_when_away;
                        if ui.checkbox(&mut dim, "Dim this screen while the mouse is elsewhere").changed() {
                            self.app.update_config(|c| c.dim_when_away = dim);
                        }
                        let mut level = cfg.dim_level;
                        if ui
                            .add_enabled(dim, egui::Slider::new(&mut level, 5..=90).suffix("% brightness"))
                            .changed()
                        {
                            self.app.update_config(|c| c.dim_level = level);
                        }
                    });
                }
            }
            let mut auto = cfg.autostart;
            if ui.checkbox(&mut auto, "Start kvmlink when I log in").changed() {
                match crate::autostart::set(auto) {
                    Ok(()) => {
                        self.app.update_config(|c| c.autostart = auto);
                        self.message = None;
                    }
                    Err(e) => self.message = Some(format!("Couldn't change start-at-login: {e}")),
                }
            }
            if let Some(m) = &self.message {
                ui.colored_label(AMBER, m);
            }
            if self.tray.is_some() {
                ui.label(
                    RichText::new(if HIDE_TO_TRAY {
                        "Closing this window keeps kvmlink running in the system tray."
                    } else {
                        "Closing this window minimizes it; quit from the tray icon."
                    })
                    .weak()
                    .small(),
                );
            }
        });
    }

    fn log_panel(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("Activity log").default_open(false).show(ui, |ui| {
            let logs: Vec<String> = lk(&self.app.logs).iter().cloned().collect();
            egui::ScrollArea::vertical().max_height(180.0).stick_to_bottom(true).show(ui, |ui| {
                for l in logs {
                    ui.label(RichText::new(l).monospace().small());
                }
            });
        });
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n - 1).collect::<String>())
    }
}
