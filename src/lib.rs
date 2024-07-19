pub mod app;
pub mod mock;

pub use app::*;
pub use cw_multi_test::*;
pub use mock::*;

#[macro_export]
macro_rules! log {
    ($($a:tt)*) => {
        &cosmwasm_schema::schemars::_serde_json::to_string_pretty(&($($a)*)).unwrap()
    };
}
