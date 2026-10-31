//! Local repair phase handlers for SDE repair effects.
//! Client dogma applies shield at cycle start and armor and hull at cycle end.
//! Amounts use the cycle-start fitted snapshot; remote and fueled repair are unsupported.
use crate::services::{combat::{Ctx, groups_health::repair_player}, fit::{Fit, Key}};

fn applies_at_start(layer: usize) -> bool { layer == 0 }

pub(super) fn amount(fit: &Fit, module: i64, amount_attr: i32) -> f64 {
    fit.value(Key::Module(module), amount_attr)
}

pub(super) async fn apply(cx: &Ctx, layer: usize, amount: f64, start: bool) {
    if applies_at_start(layer) != start { return; }
    let _ = repair_player(&cx.db, &cx.world, cx.system_id, cx.ship_id, cx.char_id, layer, amount).await;
}

#[cfg(test)] mod tests;
