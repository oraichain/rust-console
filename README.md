## MockApp testing for CosmWasm that implements for both cw-multi-test and osmosis-test-tube packages

#### How to use?

```rust

// cargo test --features test-tube ...
#[cfg(not(feature = "test-tube"))]
pub type TestMockApp = cosmwasm_testing_util::MultiTestMockApp;
#[cfg(feature = "test-tube")]
pub type TestMockApp = cosmwasm_testing_util::TestTubeMockApp;

// extends TestMockApp methods
#[derive(Deref, DerefMut)]
pub struct MockApp {
    #[deref]
    #[deref_mut]
    app: TestMockApp,
    // ...
}

let (mut app, accounts) = MockApp::new(&[("sender", &coins(100_000_000_000, "orai"))]);
let sender = &accounts[0];

// deploy contract based on implemented type
let code_id;
#[cfg(not(feature = "test-tube"))]
{
    code_id = app.upload(Box::new(
        cosmwasm_testing_util::ContractWrapper::new_with_empty(
            crate::contract::execute,
            crate::contract::instantiate,
            crate::contract::query,
        ),
    ));
}
#[cfg(feature = "test-tube")]
{
    static CW_BYTES: &[u8] = include_bytes!("./testdata/contract.wasm");
    code_id = app.upload(CW_BYTES);
}

// instantiate contract
let contract_addr = app.instantiate(
    code_id,
    Addr::unchecked(owner),
    &msg::InstantiateMsg { protocol_fee },
    &[],
    "oraiswap_v3",
).unwrap();

// execute contract
let res = app.execute(
    Addr::unchecked(sender),
    Addr::unchecked(contract_addr),
    &msg::ExecuteMsg::Increase {  },
    &[],
).unwrap();

println!("gas used {}", res.gas_info.gas_used);

```
