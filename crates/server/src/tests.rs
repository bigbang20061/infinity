//! Shared initialization for unit tests using the default game configuration.

use std::sync::Once;

pub(crate) fn init_config() -> &'static crate::config::game::GameConfig {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        crate::config::init_game(Default::default()).expect("initializing test game configuration");
    });
    crate::config::game()
}
