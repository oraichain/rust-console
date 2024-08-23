pub mod app;
pub mod mock;

pub use anyhow::Result as MockResult;
pub use app::TestMockApp;
pub use cw_multi_test::*;
pub use mock::*;

pub use crate::app::{MockAppExtensions, MockTokenExtensions};

#[macro_export]
macro_rules! log {
    ($($a:tt)*) => {
        #[cfg(debug_assertions)]
        println!("{}", &cosmwasm_schema::schemars::_serde_json::to_string_pretty(&($($a)*)).unwrap())
    };
}
