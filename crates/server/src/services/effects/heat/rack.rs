//! Pure rack heat evolution and published-research damage probability.
//! Mirrors `dogma/attributes/heatAttribute.py` for the heat curve; damage
//! probability follows EVE University's published overheating research.
//! Inputs are evaluated Fit attributes and physical slot positions, not SDE defaults.
//! This module does not roll damage or persist module condition.

use crate::space::ms_to_seconds;

/// Client heatAttribute.py treats smaller incoming heat as cooling.
const INCOMING_EPSILON: f64 = 5e-8;

/// Evolve heat for a nonnegative interval in milliseconds, preserving client operation order.
pub(crate) fn curve(heat: f64, capacity: f64, incoming: f64, generation: f64, dissipation: f64, elapsed_ms: f64) -> f64 {
    let seconds = ms_to_seconds(elapsed_ms);
    if incoming < INCOMING_EPSILON {
        let heat = heat * (-seconds * dissipation).exp();
        if heat.round() <= 0.0 { 0.0 } else { heat }
    } else {
        capacity - capacity * (-seconds * incoming * generation).exp()
            + heat * (-seconds * incoming * generation).exp()
    }
}

/// Probability for an online recipient; slot distance includes empty and offline slots.
/// The published-research policy uses all online rack modules and all fitted ship slots.
pub(crate) fn damage_chance(heat: f64, capacity: f64, online_count: usize, total_slots: usize, attenuation: f64, source_slot: i32, recipient_slot: i32, recipient_online: bool) -> f64 {
    if !recipient_online || capacity <= 0.0 || total_slots == 0 { return 0.0; }
    (heat / capacity).clamp(0.0, 1.0) * (online_count as f64 / total_slots as f64)
        * attenuation.powf(f64::from(source_slot.abs_diff(recipient_slot)))
}
