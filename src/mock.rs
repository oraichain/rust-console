use bech32::{FromBase32, ToBase32};
use cosmwasm_schema::{
    schemars::JsonSchema,
    serde::{de::DeserializeOwned, Serialize},
};
use cosmwasm_std::{from_binary, Addr, Coin, ContractResult, Response};
use cosmwasm_vm::{
    testing::{
        execute, instantiate, migrate, mock_env, mock_info, query, sudo, MockInstanceOptions,
        MockQuerier, MockStorage,
    },
    Backend, BackendApi, BackendError, BackendResult, GasInfo, Instance, InstanceOptions, Storage,
    VmResult,
};

const GAS_COST_HUMANIZE: u64 = 44; // TODO: these seem very low
const GAS_COST_CANONICALIZE: u64 = 55;
pub const GAS_PER_US: u64 = 1_000_000;

// MockPrecompiles zero pads all human addresses to make them fit the canonical_length
// it trims off zeros for the reverse operation.
// not really smart, but allows us to see a difference (and consistent length for canonical adddresses)
#[derive(Copy, Clone)]
pub struct MockApi {
    /// Length of canonical addresses created with this API. Contracts should not make any assumptions
    /// what this value is.
    pub canonical_length: usize,
}

impl Default for MockApi {
    fn default() -> Self {
        Self {
            canonical_length: 20,
        }
    }
}

impl BackendApi for MockApi {
    fn human_address(&self, canonical: &[u8]) -> BackendResult<String> {
        let gas_info = GasInfo::with_cost(GAS_COST_HUMANIZE);
        let result = match bech32::encode(
            "orai",
            canonical.to_vec().to_base32(),
            bech32::Variant::Bech32,
        ) {
            Ok(human) => Ok(human),
            Err(error) => Err(BackendError::Unknown {
                msg: format!("addr_humanize errored: {}", error),
            }),
        };
        (result, gas_info)
    }

    fn canonical_address(&self, human: &str) -> BackendResult<Vec<u8>> {
        let gas_info = GasInfo::with_cost(GAS_COST_CANONICALIZE);
        let result = match bech32::decode(human) {
            Ok((_, canon, _)) => Ok(Vec::from_base32(&canon).unwrap().into()),
            Err(error) => Err(BackendError::Unknown {
                msg: format!("addr_canonicalize errored: {}", error),
            }),
        };
        (result, gas_info)
    }
}

pub struct MockContract {
    instance: Instance<MockApi, MockStorage, MockQuerier>,
    address: Addr,
}

impl MockContract {
    pub fn new(wasm: &[u8], address: Addr, options: MockInstanceOptions) -> Self {
        let backend = Backend {
            api: MockApi::default(),
            storage: MockStorage::default(),
            querier: MockQuerier::new(&options.balances),
        };
        let memory_limit = options.memory_limit;
        let options = InstanceOptions {
            gas_limit: options.gas_limit,
            print_debug: options.print_debug,
        };
        Self {
            address,
            instance: Instance::from_code(wasm, backend, options, memory_limit).unwrap(),
        }
    }

    pub fn load_state(&mut self, state: &[u8]) -> VmResult<()> {
        self.instance.with_storage(|store| {
            // first 4 bytes is for uint32 be
            // 1 byte key length + key
            // 2 bytes value length + value
            let mut ind = 4;

            while ind < state.len() {
                let key_length = state[ind];
                ind += 1;
                let key = &state[ind..ind + key_length as usize];
                ind += key_length as usize;
                let value_length = u16::from_be_bytes(state[ind..ind + 2].try_into().unwrap());
                ind += 2;
                let value = &state[ind..ind + value_length as usize];
                ind += value_length as usize;
                store.set(key, value).0.unwrap();
            }
            Ok(())
        })
    }

    pub fn migrate<M: Serialize + JsonSchema>(
        &mut self,
        msg: M,
    ) -> ContractResult<(Response, u64)> {
        let mut env = mock_env();
        env.contract.address = self.address.clone();
        let gas_before = self.instance.get_gas_left();
        let ret = match migrate(&mut self.instance, env, msg) {
            ContractResult::Ok(ret) => ret,
            ContractResult::Err(error) => return ContractResult::Err(error),
        };
        let gas_used = (gas_before - self.instance.get_gas_left()) / GAS_PER_US;
        ContractResult::Ok((ret, gas_used))
    }

    pub fn instantiate<M: Serialize + JsonSchema>(
        &mut self,
        msg: M,
        sender: &str,
        funds: &[Coin],
    ) -> ContractResult<(Response, u64)> {
        let mut env = mock_env();
        env.contract.address = self.address.clone();
        let info = mock_info(sender, funds);
        let gas_before = self.instance.get_gas_left();
        let ret = match instantiate(&mut self.instance, env, info, msg) {
            ContractResult::Ok(ret) => ret,
            ContractResult::Err(error) => return ContractResult::Err(error),
        };
        let gas_used = (gas_before - self.instance.get_gas_left()) / GAS_PER_US;
        ContractResult::Ok((ret, gas_used))
    }

    pub fn execute<M: Serialize + JsonSchema>(
        &mut self,
        msg: M,
        sender: &str,
        funds: &[Coin],
    ) -> ContractResult<(Response, u64)> {
        let mut env = mock_env();
        env.contract.address = self.address.clone();
        let info = mock_info(sender, funds);
        let gas_before = self.instance.get_gas_left();
        let ret = match execute(&mut self.instance, env, info, msg) {
            ContractResult::Ok(ret) => ret,
            ContractResult::Err(error) => return ContractResult::Err(error),
        };
        let gas_used = (gas_before - self.instance.get_gas_left()) / GAS_PER_US;
        ContractResult::Ok((ret, gas_used))
    }

    pub fn query<M: Serialize + JsonSchema, T: DeserializeOwned>(
        &mut self,
        msg: M,
    ) -> ContractResult<(T, u64)> {
        let mut env = mock_env();
        env.contract.address = self.address.clone();
        let gas_before = self.instance.get_gas_left();
        let ret: T = match query(&mut self.instance, env, msg) {
            ContractResult::Ok(binary) => match from_binary(&binary) {
                Ok(ret) => ret,
                Err(error) => return ContractResult::Err(error.to_string()),
            },
            ContractResult::Err(error) => return ContractResult::Err(error),
        };
        let gas_used = (gas_before - self.instance.get_gas_left()) / GAS_PER_US;
        ContractResult::Ok((ret, gas_used))
    }

    pub fn sudo<M: Serialize + JsonSchema>(&mut self, msg: M) -> ContractResult<(Response, u64)> {
        let mut env = mock_env();
        env.contract.address = self.address.clone();
        let gas_before = self.instance.get_gas_left();
        let ret = match sudo(&mut self.instance, env, msg) {
            ContractResult::Ok(ret) => ret,
            ContractResult::Err(error) => return ContractResult::Err(error),
        };
        let gas_used = (gas_before - self.instance.get_gas_left()) / GAS_PER_US;
        ContractResult::Ok((ret, gas_used))
    }
}
