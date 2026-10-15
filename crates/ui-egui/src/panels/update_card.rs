//! The update card, bottom right: a newer EffectCraft is ready to install (Windows: Restart to
//! Update) or available to download (other builds). Later hides it until the next launch.
//! Engine side: `effectcraft_engine::update`.

use effectcraft_engine::update::State;
use egui::{Align2, Rect, Stroke, pos2, vec2};
use serde_json::json;

use super::panel_kit as kit;
use crate::EffectcraftApp;
use crate::i18n::{tr, tr_args};
use crate::theme::Tokens;
use crate::widgets;

pub fn show(app: &mut EffectcraftApp, ui: &mut egui::Ui, full: Rect) {
    let Some(offer) = app.session.update.offer() else { return };
    let (title, notes, ready) = match &offer {
        State::Ready { version, notes, .. } => (tr_args("EffectCraft {} is ready to install", &[&version]), notes.clone(), true),
        State::Available { version, notes, .. } => (tr_args("EffectCraft {} is available", &[&version]), notes.clone(), false),
        _ => return,
    };
    let t = app.tokens;
    let w = 340.0_f32.min(full.width() - 32.0).max(160.0);
    let h = 92.0;
    let r = Rect::from_min_size(pos2(full.max.x - 16.0 - w, full.max.y - 16.0 - h), vec2(w, h));
    let p = ui.painter();
    p.rect_filled(r, 6.0, t.panel_bg);
    p.rect_stroke(r, 6.0, Stroke::new(1.0, t.accent), egui::StrokeKind::Inside);
    let x0 = r.min.x + 12.0;
    let inner = w - 24.0;
    widgets::text_fit(p, pos2(x0, r.min.y + 16.0), Align2::LEFT_CENTER, &title, Tokens::medium(12.5), inner, t.text);
    let line = notes.lines().map(str::trim).find(|l| !l.is_empty()).map(|l| l.trim_start_matches(['-', '*', ' ']).to_string());
    let sub = if ready {
        tr("Restart EffectCraft to install it. You'll be asked to save first.").to_string()
    } else {
        line.unwrap_or_else(|| tr("Download it from the release page.").to_string())
    };
    widgets::text_fit(p, pos2(x0, r.min.y + 36.0), Align2::LEFT_CENTER, &sub, Tokens::ui(11.5), inner, t.text_dim);
    app.auto.add("update.card", r, &title);
    let (bw, bh) = (if ready { 132.0 } else { 96.0 }, 24.0);
    let y = r.max.y - 12.0 - bh;
    let main = Rect::from_min_size(pos2(r.max.x - 12.0 - bw, y), vec2(bw, bh));
    let later = Rect::from_min_size(pos2(main.min.x - 8.0 - 64.0, y), vec2(64.0, bh));
    let (id, label) = if ready { ("app.update.install", tr("Restart to Update").to_string()) } else { ("app.update.openPage", tr("Download").to_string()) };
    if kit::button(app, ui, main, "update.primary", &label, true) {
        kit::exec(app, id, json!({}));
    }
    if kit::button(app, ui, later, "update.later", tr("Later"), false) {
        kit::exec(app, "app.update.dismiss", json!({}));
    }
}
