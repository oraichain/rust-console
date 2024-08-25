pub mod app;
pub mod mock;

pub use anyhow::Result as MockResult;
pub use app::*;
pub use cw_multi_test::*;
pub use mock::*;

#[macro_export]
macro_rules! log {
    ($($a:tt)*) => {
        #[cfg(debug_assertions)]
        println!("{}", &cosmwasm_schema::schemars::_serde_json::to_string_pretty(&($($a)*)).unwrap())
    };
}
