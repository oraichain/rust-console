pub mod mock;

#[macro_export]
macro_rules! log {
    ($($a:tt)*) => {
        &cosmwasm_schema::schemars::_serde_json::to_string_pretty(&($($a)*)).unwrap()
    };
}
