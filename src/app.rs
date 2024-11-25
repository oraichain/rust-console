use crate::MockResult;
use cosmwasm_schema::serde::de::DeserializeOwned;
use cosmwasm_schema::serde::Serialize;
use cosmwasm_std::testing::{MockApi, MockStorage};
use cosmwasm_std::{
    Addr, AllBalanceResponse, BankQuery, Coin, Empty, IbcMsg, IbcQuery, QuerierWrapper,
    QueryRequest, StdResult, Uint128,
};
use cw20::TokenInfoResponse;
use cw_multi_test::{
    next_block, App, AppResponse, BankKeeper, BasicAppBuilder, Contract, ContractWrapper,
    DistributionKeeper, Executor, FailingModule, StakeKeeper, WasmKeeper,
};
use std::any::TypeId;
use std::collections::HashMap;
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

pub struct MockApp {
    pub app: AppWrapped,
    token_map: HashMap<String, Addr>, // map token name to address
    token_id: u64,
    tokenfactory_id: u64,
}

#[allow(dead_code)]
impl MockApp {
    pub fn new(init_balances: &[(&str, &[Coin])]) -> Self {
        Self::new_with_creation_fee(init_balances, vec![])
    }

    pub fn token_id(&self) -> u64 {
        self.token_id
    }

    pub fn tokenfactory_id(&self) -> u64 {
        self.tokenfactory_id
    }

    pub fn new_with_creation_fee(
        init_balances: &[(&str, &[Coin])],
        denom_creation_fee: Vec<Coin>,
    ) -> Self {
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

        Self {
            app,
            token_id,
            token_map: HashMap::new(),
            tokenfactory_id,
        }
    }

    pub fn set_token_contract(&mut self, code: Code) {
        self.token_id = self.upload(code);
    }

    pub fn upload(&mut self, code: Code) -> u64 {
        let code_id = self.app.store_code(code);
        self.app.update_block(next_block);
        code_id
    }

    pub fn instantiate<T: Serialize>(
        &mut self,
        code_id: u64,
        sender: Addr,
        init_msg: &T,
        send_funds: &[Coin],
        label: &str,
    ) -> MockResult<Addr> {
        let contract_addr = self.app.instantiate_contract(
            code_id,
            sender.clone(),
            init_msg,
            send_funds,
            label,
            Some(sender.to_string()),
        )?;
        self.app.update_block(next_block);
        Ok(contract_addr)
    }

    pub fn migrate<T: Serialize + std::fmt::Debug>(
        &mut self,
        sender: Addr,
        contract_addr: Addr,
        msg: &T,
        new_code_id: u64,
    ) -> Result<AppResponse, String> {
        let response = self
            .app
            .migrate_contract(sender, contract_addr, msg, new_code_id)
            .map_err(|err| err.to_string())?;
        self.app.update_block(next_block);
        Ok(response)
    }

    pub fn execute<T: Serialize + std::fmt::Debug + Clone + 'static>(
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

    pub fn query<T: DeserializeOwned, U: Serialize + Clone + 'static>(
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

    pub fn query_balance(&self, account_addr: Addr, denom: String) -> MockResult<Uint128> {
        let balance = self.app.wrap().query_balance(account_addr, denom)?;
        Ok(balance.amount)
    }

    pub fn query_all_balances(&self, account_addr: Addr) -> MockResult<Vec<Coin>> {
        let all_balances: AllBalanceResponse =
            self.app
                .wrap()
                .query(&QueryRequest::Bank(BankQuery::AllBalances {
                    address: account_addr.to_string(),
                }))?;
        Ok(all_balances.amount)
    }

    pub fn register_token(&mut self, contract_addr: Addr) -> MockResult<String> {
        let res: cw20::TokenInfoResponse =
            self.query(contract_addr.clone(), &cw20::Cw20QueryMsg::TokenInfo {})?;
        self.token_map.insert(res.symbol.clone(), contract_addr);
        Ok(res.symbol)
    }

    pub fn query_token_balance(
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

    pub fn query_token_info(&self, contract_addr: Addr) -> StdResult<TokenInfoResponse> {
        self.query(contract_addr, &cw20::Cw20QueryMsg::TokenInfo {})
    }

    pub fn query_token_balances(&self, account_addr: &str) -> MockResult<Vec<Coin>> {
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

    pub fn as_querier(&self) -> QuerierWrapper<'_, TokenFactoryQuery> {
        self.app.wrap()
    }

    pub fn get_token_addr(&self, token: &str) -> Option<Addr> {
        self.token_map.get(token).cloned()
    }

    pub fn create_token(&mut self, owner: &str, token: &str, initial_amount: u128) -> Addr {
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

    pub fn set_balances_from(&mut self, sender: Addr, balances: &[(&str, &[(&str, u128)])]) {
        for (denom, balance) in balances {
            // send for each recipient
            for (recipient, amount) in balance.iter() {
                self.app
                    .send_tokens(
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

    pub fn mint_token(
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

    pub fn set_token_balances_from(
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

    pub fn set_balances(&mut self, owner: &str, balances: &[(&str, &[(&str, u128)])]) {
        self.set_balances_from(Addr::unchecked(owner), balances)
    }

    // configure the mint whitelist mock querier
    pub fn set_token_balances(
        &mut self,
        owner: &str,
        balances: &[(&str, &[(&str, u128)])],
    ) -> MockResult<Vec<Addr>> {
        self.set_token_balances_from(owner, balances)
    }

    pub fn approve_token(
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

    /// external method
    pub fn create_tokenfactory(&mut self, sender: Addr) -> MockResult<Addr> {
        let addr = self.instantiate(
            self.tokenfactory_id,
            sender,
            &tokenfactory::msg::InstantiateMsg {},
            &[],
            "tokenfactory",
        )?;
        Ok(addr)
    }
}
