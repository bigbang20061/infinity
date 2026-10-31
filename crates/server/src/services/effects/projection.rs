//! Source-effective values for an explicitly supplied projected dogma effect.
//! Effect identity and attributes come from the activating source; the fit evaluator
//! consumes these snapshots without loading the source ship. PR2 carries only
//! evaluator inputs; PR3 will add source ship, kind and expiry when the space
//! lifecycle owns runtime projection storage.

#[derive(Clone, Debug)]
pub(crate) struct Projection {
    pub source_module: i64,
    pub source_type: i32,
    pub effect_id: i32,
    pub values: Vec<(i32, f64)>,
    pub charge_type: Option<i32>,
}
