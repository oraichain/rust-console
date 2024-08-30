pub mod app;
pub mod mock;

pub use anyhow::Result as MockResult;
pub use app::*;
pub use cw_multi_test::*;
pub use mock::*;
pub use osmosis_test_tube as test_tube;

#[macro_export]
macro_rules! log {
    ($api: expr, $($a:tt)*) => {
        #[cfg(debug_assertions)]
        $api.debug(&cosmwasm_schema::schemars::_serde_json::to_string_pretty(&($($a)*)).unwrap());
    };
}
