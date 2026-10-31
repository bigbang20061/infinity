use super::*;
use crate::services::fit::{OP_MOD_ADD, OP_POST_PERCENT};


#[test]
fn falloff_scales_operands_around_operation_identity() {
    assert_eq!(scaled(-60.0, OP_POST_PERCENT, 0.5), -30.0);
    assert_eq!(scaled(4.0, OP_MOD_ADD, 0.5), 2.0);
    assert_eq!(scaled(0.4, OP_POST_MUL, 0.5), 0.7);
    assert_eq!(scaled(2.0, OP_POST_DIV, 0.0), 1.0);
    assert_eq!(scaled(2.0, OP_PRE_MUL, 1.0), 2.0);
}
