use crate::MockResult;
use cosmwasm_schema::serde::de::DeserializeOwned;
use cosmwasm_schema::serde::Serialize;
use cosmwasm_std::testing::{MockApi, MockStorage};
use cosmwasm_std::{
    coins, Addr, AllBalanceResponse, BankQuery, Binary, Coin, Empty, IbcMsg, IbcQuery,
    QuerierWrapper, QueryRequest, StdError, StdResult, Uint128,
};
use cw20::TokenInfoResponse;
use cw_multi_test::{
    next_block, App, AppResponse, BankKeeper, BasicAppBuilder, Contract, ContractWrapper,
    DistributionKeeper, Executor, FailingModule, StakeKeeper, WasmKeeper,
};
use osmosis_test_tube::cosmrs::proto::cosmos::bank::v1beta1::{
    MsgSend, QueryAllBalancesRequest, QueryBalanceRequest, QuerySupplyOfRequest,
};
use osmosis_test_tube::cosmrs::tx::MessageExt;
use osmosis_test_tube::{Account, SigningAccount};
use osmosis_test_tube::{Module, OraichainTestApp, Wasm};
use std::any::TypeId;
use std::collections::HashMap;
use std::str::FromStr;
use token_bindings::{TokenFactoryMsg, TokenFactoryQuery};
use token_bindings_test::TokenFactoryModule;

pub type AppWrapped = App<
    BankKeeper,
    MockApi,
    MockStorage,
    TokenFactoryModule,
    WasmKeeper<TokenFactoryMsg, TokenFactoryQuery>,
    StakeKeeper,
    DistributionKeeper,
    FailingModule<IbcMsg, IbcQuery, Empty>,
>;
pub type Code = Box<dyn Contract<TokenFactoryMsg, TokenFactoryQuery>>;

pub trait MockAppExtensions {
    fn new(init_balances: &[(&str, &[Coin])]) -> (Self, Vec<String>)
    where
        Self: Sized;
    fn instantiate<T: Serialize>(
        &mut self,
        code_id: u64,
        sender: Addr,
        init_msg: &T,
        send_funds: &[Coin],
        label: &str,
    ) -> MockResult<Addr>;
    fn execute<T: Serialize + std::fmt::Debug + Clone + 'static>(
        &mut self,
        sender: Addr,
        contract_addr: Addr,
        msg: &T,
        send_funds: &[Coin],
    ) -> MockResult<AppResponse>;
    fn sudo<T: Serialize>(&mut self, contract_addr: Addr, msg: &T) -> MockResult<AppResponse>;
    fn query<T: DeserializeOwned, U: Serialize + Clone + 'static>(
        &self,
        contract_addr: Addr,
        msg: &U,
    ) -> StdResult<T>;

    fn query_balance(&self, account_addr: Addr, denom: String) -> MockResult<Uint128>;

    fn query_all_balances(&self, account_addr: Addr) -> MockResult<Vec<Coin>>;

    fn send_coins(
        &mut self,
        sender: Addr,
        recipient: Addr,
        amount: &[Coin],
    ) -> MockResult<AppResponse>;

    fn query_supply(&self, denom: &str) -> MockResult<Coin>;
}

#[allow(dead_code)]
pub struct MultiTestMockApp {
    pub app: AppWrapped,
    token_map: HashMap<String, Addr>, // map token name to address
    token_id: u64,
    tokenfactory_id: u64,
}

impl MultiTestMockApp {
    pub fn new_with_creation_fee(
        init_balances: &[(&str, &[Coin])],
        denom_creation_fee: Vec<Coin>,
    ) -> (Self, Vec<String>) {
        let mut accounts = vec![];
        let mut app = BasicAppBuilder::<TokenFactoryMsg, TokenFactoryQuery>::new_custom()
            .with_custom(TokenFactoryModule::new(denom_creation_fee))
            .build(|router, _, storage| {
                for (owner, init_funds) in init_balances.iter() {
                    router
                        .bank
                        .init_balance(
                            storage,
                            &Addr::unchecked(owner.to_owned()),
                            init_funds.to_vec(),
                        )
                        .unwrap();

                    accounts.push(owner.to_string());
                }
            });

        // default token is cw20_base
        let token_id = app.store_code(Box::new(ContractWrapper::new_with_empty(
            cw20_base::contract::execute,
            cw20_base::contract::instantiate,
            cw20_base::contract::query,
        )));

        let tokenfactory_id = app.store_code(Box::new(ContractWrapper::new(
            tokenfactory::contract::execute,
            tokenfactory::contract::instantiate,
            tokenfactory::contract::query,
        )));

        (
            Self {
                app,
                token_id,
                token_map: HashMap::new(),
                tokenfactory_id,
            },
            accounts,
        )
    }

    pub fn set_token_contract(&mut self, code: Code) {
        self.token_id = self.upload(code);
    }

    pub fn upload(&mut self, code: Code) -> u64 {
        let code_id = self.app.store_code(code);
        self.app.update_block(next_block);
        code_id
    }

    pub fn as_querier(&self) -> QuerierWrapper<'_, TokenFactoryQuery> {
        self.app.wrap()
    }
}

impl MockAppExtensions for MultiTestMockApp {
    fn new(init_balances: &[(&str, &[Coin])]) -> (Self, Vec<String>) {
        Self::new_with_creation_fee(init_balances, coins(10_000_000u128, "orai"))
    }
    fn instantiate<T: Serialize>(
        &mut self,
        code_id: u64,
        sender: Addr,
        init_msg: &T,
        send_funds: &[Coin],
        label: &str,
    ) -> MockResult<Addr> {
        let admin = Some(sender.to_string());
        let contract_addr = self
            .app
            .instantiate_contract(code_id, sender, init_msg, send_funds, label, admin)?;
        self.app.update_block(next_block);
        Ok(contract_addr)
    }

    fn execute<T: Serialize + std::fmt::Debug + Clone + 'static>(
        &mut self,
        sender: Addr,
        contract_addr: Addr,
        msg: &T,
        send_funds: &[Coin],
    ) -> MockResult<AppResponse> {
        let response = if TypeId::of::<T>() == TypeId::of::<TokenFactoryMsg>() {
            let value = msg.clone();
            let dest = unsafe { std::ptr::read(&value as *const T as *const TokenFactoryMsg) };
            std::mem::forget(value);
            self.app.execute(contract_addr, dest.into())?
        } else {
            self.app
                .execute_contract(sender, contract_addr, msg, send_funds)?
        };

        self.app.update_block(next_block);

        Ok(response)
    }

    fn sudo<T: Serialize>(&mut self, contract_addr: Addr, msg: &T) -> MockResult<AppResponse> {
        let response = self.app.wasm_sudo(contract_addr, msg)?;

        self.app.update_block(next_block);

        Ok(response)
    }

    fn query<T: DeserializeOwned, U: Serialize + Clone + 'static>(
        &self,
        contract_addr: Addr,
        msg: &U,
    ) -> StdResult<T> {
        if TypeId::of::<U>() == TypeId::of::<TokenFactoryQuery>() {
            let value = msg.clone();
            let dest = unsafe { std::ptr::read(&value as *const U as *const TokenFactoryQuery) };
            std::mem::forget(value);
            self.app.wrap().query(&dest.into())
        } else {
            self.app.wrap().query_wasm_smart(contract_addr, msg)
        }
    }

    fn query_balance(&self, account_addr: Addr, denom: String) -> MockResult<Uint128> {
        let balance = self.app.wrap().query_balance(account_addr, denom)?;
        Ok(balance.amount)
    }

    fn send_coins(
        &mut self,
        sender: Addr,
        recipient: Addr,
        amount: &[Coin],
    ) -> MockResult<AppResponse> {
        self.app.send_tokens(sender, recipient, amount)
    }

    fn query_all_balances(&self, account_addr: Addr) -> MockResult<Vec<Coin>> {
        let all_balances: AllBalanceResponse =
            self.app
                .wrap()
                .query(&QueryRequest::Bank(BankQuery::AllBalances {
                    address: account_addr.to_string(),
                }))?;
        Ok(all_balances.amount)
    }

    fn query_supply(&self, denom: &str) -> MockResult<Coin> {
        let supply = self.app.wrap().query_supply(denom)?;
        Ok(supply)
    }
}

static CW20_BYTES: &[u8] = include_bytes!("./testdata/cw20-base.wasm");
static TOKENFACTORY_BYTES: &[u8] = include_bytes!("./testdata/tokenfactory.wasm");

#[allow(dead_code)]
pub struct TestTubeMockApp {
    pub app: OraichainTestApp,
    owner: SigningAccount,
    token_map: HashMap<String, Addr>, // map token name to address
    account_map: HashMap<String, SigningAccount>, // map token name to address
    account_name_map: HashMap<String, String>, // map name to account address
    token_id: u64,
    tokenfactory_id: u64,
    pub block_time: u64,
}

impl TestTubeMockApp {
    pub fn set_token_contract(&mut self, code: &[u8]) {
        self.token_id = self.upload(code);
    }

    pub fn upload(&mut self, code: &[u8]) -> u64 {
        let wasm = Wasm::new(&self.app);
        let code_id = wasm
            .store_code(code, None, &self.owner)
            .unwrap()
            .data
            .code_id;
        self.app.increase_time(self.block_time);
        code_id
    }

    fn get_signer(&self, sender: &Addr) -> MockResult<&SigningAccount> {
        let sender_addr = if let Some(sender_addr) = self.account_name_map.get(sender.as_str()) {
            sender_addr
        } else {
            sender.as_str()
        };

        let Some(signer) = self.account_map.get(sender_addr) else {
            return Err(anyhow::Error::msg("Account not existed"));
        };

        Ok(signer)
    }

    fn get_funds_and_signer(
        &self,
        sender: &Addr,
        send_funds: &[Coin],
    ) -> MockResult<(
        &SigningAccount,
        Vec<osmosis_test_tube::cosmrs::proto::cosmos::base::v1beta1::Coin>,
    )> {
        let signer = self.get_signer(sender)?;
        let funds: Vec<_> = send_funds
            .iter()
            .map(
                |fund| osmosis_test_tube::cosmrs::proto::cosmos::base::v1beta1::Coin {
                    amount: fund.amount.to_string(),
                    denom: fund.denom.to_string(),
                },
            )
            .collect();

        Ok((signer, funds))
    }
}

impl MockAppExtensions for TestTubeMockApp {
    fn new(init_balances: &[(&str, &[Coin])]) -> (Self, Vec<String>) {
        let app = OraichainTestApp::new();
        let mut accounts = vec![];
        let mut account_map = HashMap::default();
        let mut account_name_map = HashMap::default();
        for (owner, init_funds) in init_balances.iter() {
            let acc = app.init_account(init_funds).unwrap();
            let acc_addr = acc.address();
            account_map.insert(acc_addr.to_string(), acc);
            account_name_map.insert(owner.to_string(), acc_addr.to_string());
            accounts.push(acc_addr.to_string());
        }

        let wasm = Wasm::new(&app);

        let owner = app
            .init_account(&coins(5_000_000_000_000u128, "orai"))
            .unwrap();

        let tokenfactory_id = wasm
            .store_code(TOKENFACTORY_BYTES, None, &owner)
            .unwrap()
            .data
            .code_id;
        let token_id = wasm
            .store_code(CW20_BYTES, None, &owner)
            .unwrap()
            .data
            .code_id;

        (
            Self {
                token_map: HashMap::new(),
                account_map,
                account_name_map,
                token_id,
                tokenfactory_id,
                owner,
                app,
                block_time: 5u64,
            },
            accounts,
        )
    }

    fn instantiate<T: Serialize>(
        &mut self,
        code_id: u64,
        sender: Addr,
        init_msg: &T,
        send_funds: &[Coin],
        label: &str,
    ) -> MockResult<Addr> {
        let wasm = Wasm::new(&self.app);
        let (signer, funds) = self.get_funds_and_signer(&sender, send_funds)?;
        let contract_addr = wasm
            .instantiate(
                code_id,
                init_msg,
                Some(signer.address().as_str()),
                Some(label),
                &funds,
                signer,
            )?
            .data
            .address;

        self.app.increase_time(self.block_time);

        Ok(Addr::unchecked(contract_addr))
    }

    fn execute<T: Serialize + std::fmt::Debug + Clone + 'static>(
        &mut self,
        sender: Addr,
        contract_addr: Addr,
        msg: &T,
        send_funds: &[Coin],
    ) -> MockResult<AppResponse> {
        // Wasm::Execute
        let wasm = Wasm::new(&self.app);
        let (signer, funds) = self.get_funds_and_signer(&sender, send_funds)?;
        let execute_res = wasm.execute(contract_addr.as_str(), msg, &funds, signer)?;

        self.app.increase_time(self.block_time);

        Ok(AppResponse {
            events: execute_res.events,
            data: Some(Binary::from(execute_res.data.data)),
        })
    }

    fn sudo<T: Serialize>(&mut self, contract_addr: Addr, msg: &T) -> MockResult<AppResponse> {
        // let wasm = Wasm::new(&self.app);
        let response = self.app.wasm_sudo(contract_addr.as_str(), msg)?;

        self.app.increase_time(self.block_time);

        Ok(AppResponse {
            events: vec![],
            data: Some(Binary::from(response)),
        })
    }

    fn query<T: DeserializeOwned, U: Serialize + Clone + 'static>(
        &self,
        contract_addr: Addr,
        msg: &U,
    ) -> StdResult<T> {
        let wasm = Wasm::new(&self.app);
        let response = wasm
            .query(contract_addr.as_str(), msg)
            .map_err(|err| StdError::generic_err(err.to_string()))?;
        Ok(response)
    }

    fn query_balance(&self, account_addr: Addr, denom: String) -> MockResult<Uint128> {
        let bank = osmosis_test_tube::Bank::new(&self.app);
        let balance = bank.query_balance(&QueryBalanceRequest {
            address: account_addr.to_string(),
            denom,
        })?;
        Ok(balance
            .balance
            .map(|b| Uint128::from_str(&b.amount).unwrap())
            .unwrap_or_default())
    }

    fn query_all_balances(&self, account_addr: Addr) -> MockResult<Vec<Coin>> {
        let bank = osmosis_test_tube::Bank::new(&self.app);
        let all_balances = bank.query_all_balances(&QueryAllBalancesRequest {
            address: account_addr.to_string(),
            pagination: None,
        })?;
        Ok(all_balances
            .balances
            .iter()
            .map(|c| Coin {
                amount: Uint128::from_str(&c.amount).unwrap(),
                denom: c.denom.to_string(),
            })
            .collect())
    }

    fn send_coins(
        &mut self,
        sender: Addr,
        recipient: Addr,
        amount: &[Coin],
    ) -> MockResult<AppResponse> {
        let bank = osmosis_test_tube::Bank::new(&self.app);
        let (signer, funds) = self.get_funds_and_signer(&sender, amount)?;
        let response = bank.send(
            MsgSend {
                from_address: sender.to_string(),
                to_address: recipient.to_string(),
                amount: funds,
            },
            signer,
        )?;

        Ok(AppResponse {
            data: Some(Binary::from(response.data.to_bytes()?)),
            events: response.events,
        })
    }

    fn query_supply(&self, denom: &str) -> MockResult<Coin> {
        let bank = osmosis_test_tube::Bank::new(&self.app);
        let res = bank.query_supply_of(&QuerySupplyOfRequest {
            denom: denom.to_string(),
        })?;
        Ok(res
            .amount
            .map(|a| Coin {
                amount: Uint128::from_str(&a.amount).unwrap(),
                denom: a.denom,
            })
            .unwrap_or_default())
    }
}

// this is for common implementation that depends on MockApp
pub trait MockTokenExtensions {
    fn token_id(&self) -> u64;

    fn tokenfactory_id(&self) -> u64;

    fn create_tokenfactory(&mut self, sender: Addr) -> MockResult<Addr>;

    fn register_token(&mut self, contract_addr: Addr) -> MockResult<String>;

    fn query_token_balance(&self, contract_addr: &str, account_addr: &str) -> MockResult<Uint128>;

    fn query_token_info(&self, contract_addr: Addr) -> StdResult<TokenInfoResponse>;

    fn query_token_balances(&self, account_addr: &str) -> MockResult<Vec<Coin>>;

    fn get_token_addr(&self, token: &str) -> Option<Addr>;

    fn create_token(&mut self, owner: &str, token: &str, initial_amount: u128) -> Addr;

    fn set_balances_from(&mut self, sender: Addr, balances: &[(&str, &[(&str, u128)])]);

    fn mint_token(
        &mut self,
        sender: &str,
        recipient: &str,
        cw20_addr: &str,
        amount: u128,
    ) -> MockResult<AppResponse>;

    fn set_token_balances_from(
        &mut self,
        sender: &str,
        balances: &[(&str, &[(&str, u128)])],
    ) -> MockResult<Vec<Addr>>;

    fn set_balances(&mut self, owner: &str, balances: &[(&str, &[(&str, u128)])]);

    // configure the mint whitelist mock querier
    fn set_token_balances(
        &mut self,
        owner: &str,
        balances: &[(&str, &[(&str, u128)])],
    ) -> MockResult<Vec<Addr>>;

    fn approve_token(
        &mut self,
        token: &str,
        approver: &str,
        spender: &str,
        amount: u128,
    ) -> MockResult<AppResponse>;
}

#[macro_export]
macro_rules! impl_mock_token_trait {
    ($impl:tt) => {
        impl MockTokenExtensions for $impl {
            fn token_id(&self) -> u64 {
                self.token_id
            }

            fn tokenfactory_id(&self) -> u64 {
                self.tokenfactory_id
            }

            fn create_tokenfactory(&mut self, sender: Addr) -> MockResult<Addr> {
                let addr = self.instantiate(
                    self.tokenfactory_id,
                    sender,
                    &tokenfactory::msg::InstantiateMsg {},
                    &[],
                    "tokenfactory",
                )?;
                Ok(addr)
            }

            fn register_token(&mut self, contract_addr: Addr) -> MockResult<String> {
                let res: cw20::TokenInfoResponse =
                    self.query(contract_addr.clone(), &cw20::Cw20QueryMsg::TokenInfo {})?;
                self.token_map.insert(res.symbol.clone(), contract_addr);
                Ok(res.symbol)
            }

            fn query_token_balance(
                &self,
                contract_addr: &str,
                account_addr: &str,
            ) -> MockResult<Uint128> {
                let res: cw20::BalanceResponse = self.query(
                    Addr::unchecked(contract_addr),
                    &cw20::Cw20QueryMsg::Balance {
                        address: account_addr.to_string(),
                    },
                )?;
                Ok(res.balance)
            }

            fn query_token_info(&self, contract_addr: Addr) -> StdResult<TokenInfoResponse> {
                self.query(contract_addr, &cw20::Cw20QueryMsg::TokenInfo {})
            }

            fn query_token_balances(&self, account_addr: &str) -> MockResult<Vec<Coin>> {
                let mut balances = vec![];
                for (denom, contract_addr) in self.token_map.iter() {
                    let res: cw20::BalanceResponse = self.query(
                        contract_addr.clone(),
                        &cw20::Cw20QueryMsg::Balance {
                            address: account_addr.to_string(),
                        },
                    )?;
                    balances.push(Coin {
                        denom: denom.clone(),
                        amount: res.balance,
                    });
                }
                Ok(balances)
            }

            fn get_token_addr(&self, token: &str) -> Option<Addr> {
                self.token_map.get(token).cloned()
            }

            fn create_token(&mut self, owner: &str, token: &str, initial_amount: u128) -> Addr {
                let addr = self
                    .instantiate(
                        self.token_id,
                        Addr::unchecked(owner),
                        &cw20_base::msg::InstantiateMsg {
                            name: token.to_string(),
                            symbol: token.to_string(),
                            decimals: 6,
                            initial_balances: vec![cw20::Cw20Coin {
                                address: owner.to_string(),
                                amount: initial_amount.into(),
                            }],
                            mint: Some(cw20::MinterResponse {
                                minter: owner.to_string(),
                                cap: None,
                            }),
                            marketing: None,
                        },
                        &[],
                        "cw20",
                    )
                    .unwrap();
                self.token_map.insert(token.to_string(), addr.clone());
                addr
            }

            fn set_balances_from(&mut self, sender: Addr, balances: &[(&str, &[(&str, u128)])]) {
                for (denom, balance) in balances {
                    // send for each recipient
                    for (recipient, amount) in balance.iter() {
                        self.send_coins(
                            sender.clone(),
                            Addr::unchecked(*recipient),
                            &[Coin {
                                denom: denom.to_string(),
                                amount: Uint128::from(*amount),
                            }],
                        )
                        .unwrap();
                    }
                }
            }

            fn mint_token(
                &mut self,
                sender: &str,
                recipient: &str,
                cw20_addr: &str,
                amount: u128,
            ) -> MockResult<AppResponse> {
                self.execute(
                    Addr::unchecked(sender),
                    Addr::unchecked(cw20_addr),
                    &cw20::Cw20ExecuteMsg::Mint {
                        recipient: recipient.to_string(),
                        amount: amount.into(),
                    },
                    &[],
                )
            }

            fn set_token_balances_from(
                &mut self,
                sender: &str,
                balances: &[(&str, &[(&str, u128)])],
            ) -> MockResult<Vec<Addr>> {
                let mut contract_addrs = vec![];
                for (token, balances) in balances {
                    let contract_addr = match self.token_map.get(*token) {
                        None => self.create_token(sender, token, 0),
                        Some(addr) => addr.clone(),
                    };
                    contract_addrs.push(contract_addr.clone());

                    // mint for each recipient
                    for (recipient, amount) in balances.iter() {
                        if *amount > 0u128 {
                            self.mint_token(sender, recipient, contract_addr.as_str(), *amount)?;
                        }
                    }
                }
                Ok(contract_addrs)
            }

            fn set_balances(&mut self, owner: &str, balances: &[(&str, &[(&str, u128)])]) {
                self.set_balances_from(Addr::unchecked(owner), balances)
            }

            // configure the mint whitelist mock querier
            fn set_token_balances(
                &mut self,
                owner: &str,
                balances: &[(&str, &[(&str, u128)])],
            ) -> MockResult<Vec<Addr>> {
                self.set_token_balances_from(owner, balances)
            }

            fn approve_token(
                &mut self,
                token: &str,
                approver: &str,
                spender: &str,
                amount: u128,
            ) -> MockResult<AppResponse> {
                let token_addr = match self.token_map.get(token) {
                    Some(v) => v.to_owned(),
                    None => Addr::unchecked(token),
                };

                self.execute(
                    Addr::unchecked(approver),
                    token_addr,
                    &cw20::Cw20ExecuteMsg::IncreaseAllowance {
                        spender: spender.to_string(),
                        amount: amount.into(),
                        expires: None,
                    },
                    &[],
                )
            }
        }
    };
}

#[cfg(feature = "test-tube")]
pub type MockApp = TestTubeMockApp;
#[cfg(not(feature = "test-tube"))]
pub type MockApp = MultiTestMockApp;

impl_mock_token_trait!(MockApp);
