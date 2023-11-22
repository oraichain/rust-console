use bech32::{FromBase32, ToBase32};
use cosmwasm_schema::{
    schemars::JsonSchema,
    serde::{de::DeserializeOwned, Serialize},
};
use cosmwasm_std::{
    from_binary, Addr, Api, CanonicalAddr, Coin, ContractResult, Order, Record, RecoverPubkeyError,
    Response, StdError, StdResult, Storage as StdStorage, VerificationError,
};
use cosmwasm_vm::{
    testing::{
        execute, instantiate, migrate, mock_env, mock_info, query, sudo, MockInstanceOptions,
        MockQuerier,
    },
    Backend, BackendApi, BackendError, BackendResult, GasInfo, Instance, InstanceOptions, Storage,
    VmResult,
};
use std::ops::{Bound, RangeBounds};
use std::{
    collections::{BTreeMap, HashMap},
    iter,
};

const GAS_COST_HUMANIZE: u64 = 44; // TODO: these seem very low
const GAS_COST_CANONICALIZE: u64 = 55;
pub const GAS_PER_US: u64 = 1_000_000;
const GAS_COST_LAST_ITERATION: u64 = 37;
const GAS_COST_RANGE: u64 = 11;

#[derive(Default, Debug, Clone)]
struct Iter {
    data: Vec<Record>,
    position: usize,
}

#[derive(Default, Debug)]
pub struct MockStorage {
    data: BTreeMap<Vec<u8>, Vec<u8>>,
    iterators: HashMap<u32, Iter>,
}

impl MockStorage {
    pub fn new() -> Self {
        MockStorage::default()
    }

    pub fn wrap(&mut self) -> StorageWrapper {
        StorageWrapper { store: self }
    }

    pub fn all(&mut self, iterator_id: u32) -> BackendResult<Vec<Record>> {
        let mut out: Vec<Record> = Vec::new();
        let mut total = GasInfo::free();
        loop {
            let (result, info) = self.next(iterator_id);
            total += info;
            match result {
                Err(err) => return (Err(err), total),
                Ok(ok) => {
                    if let Some(v) = ok {
                        out.push(v);
                    } else {
                        break;
                    }
                }
            }
        }
        (Ok(out), total)
    }
}

impl Storage for MockStorage {
    fn get(&self, key: &[u8]) -> BackendResult<Option<Vec<u8>>> {
        let gas_info = GasInfo::with_externally_used(key.len() as u64);
        (Ok(self.data.get(key).cloned()), gas_info)
    }

    fn scan(
        &mut self,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        order: Order,
    ) -> BackendResult<u32> {
        let gas_info = GasInfo::with_externally_used(GAS_COST_RANGE);
        let bounds = range_bounds(start, end);

        let values: Vec<Record> = match (bounds.start_bound(), bounds.end_bound()) {
            // BTreeMap.range panics if range is start > end.
            // However, this cases represent just empty range and we treat it as such.
            (Bound::Included(start), Bound::Excluded(end)) if start > end => Vec::new(),
            _ => match order {
                Order::Ascending => self.data.range(bounds).map(clone_item).collect(),
                Order::Descending => self.data.range(bounds).rev().map(clone_item).collect(),
            },
        };

        let last_id: u32 = self
            .iterators
            .len()
            .try_into()
            .expect("Found more iterator IDs than supported");
        let new_id = last_id + 1;
        let iter = Iter {
            data: values,
            position: 0,
        };
        self.iterators.insert(new_id, iter);

        (Ok(new_id), gas_info)
    }

    fn next(&mut self, iterator_id: u32) -> BackendResult<Option<Record>> {
        let iterator = match self.iterators.get_mut(&iterator_id) {
            Some(i) => i,
            None => {
                return (
                    Err(BackendError::iterator_does_not_exist(iterator_id)),
                    GasInfo::free(),
                )
            }
        };

        let (value, gas_info): (Option<Record>, GasInfo) =
            if iterator.data.len() > iterator.position {
                let item = iterator.data[iterator.position].clone();
                iterator.position += 1;
                let gas_cost = (item.0.len() + item.1.len()) as u64;
                (Some(item), GasInfo::with_cost(gas_cost))
            } else {
                (None, GasInfo::with_externally_used(GAS_COST_LAST_ITERATION))
            };

        (Ok(value), gas_info)
    }

    fn set(&mut self, key: &[u8], value: &[u8]) -> BackendResult<()> {
        self.data.insert(key.to_vec(), value.to_vec());
        let gas_info = GasInfo::with_externally_used((key.len() + value.len()) as u64);
        (Ok(()), gas_info)
    }

    fn remove(&mut self, key: &[u8]) -> BackendResult<()> {
        self.data.remove(key);
        let gas_info = GasInfo::with_externally_used(key.len() as u64);
        (Ok(()), gas_info)
    }
}

fn range_bounds(start: Option<&[u8]>, end: Option<&[u8]>) -> impl RangeBounds<Vec<u8>> {
    (
        start.map_or(Bound::Unbounded, |x| Bound::Included(x.to_vec())),
        end.map_or(Bound::Unbounded, |x| Bound::Excluded(x.to_vec())),
    )
}

/// The BTreeMap specific key-value pair reference type, as returned by BTreeMap<Vec<u8>, Vec<u8>>::range.
/// This is internal as it can change any time if the map implementation is swapped out.
type BTreeMapRecordRef<'a> = (&'a Vec<u8>, &'a Vec<u8>);

fn clone_item(item_ref: BTreeMapRecordRef) -> Record {
    let (key, value) = item_ref;
    (key.clone(), value.clone())
}

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

impl Api for MockApi {
    fn addr_validate(&self, input: &str) -> StdResult<Addr> {
        let canonical = self.addr_canonicalize(input)?;
        let normalized = self.addr_humanize(&canonical)?;
        if input != normalized {
            return Err(StdError::generic_err(
                "Invalid input: address not normalized",
            ));
        }

        Ok(Addr::unchecked(input))
    }

    fn addr_canonicalize(&self, human: &str) -> StdResult<CanonicalAddr> {
        match self.canonical_address(human).0 {
            Ok(addr) => Ok(addr.into()),
            Err(error) => Err(StdError::generic_err(error.to_string())),
        }
    }

    fn addr_humanize(&self, canonical: &CanonicalAddr) -> StdResult<Addr> {
        match self.human_address(canonical).0 {
            Ok(addr) => Ok(Addr::unchecked(addr)),
            Err(error) => Err(StdError::generic_err(error.to_string())),
        }
    }

    fn secp256k1_verify(
        &self,
        message_hash: &[u8],
        signature: &[u8],
        public_key: &[u8],
    ) -> Result<bool, VerificationError> {
        todo!()
    }

    fn secp256k1_recover_pubkey(
        &self,
        message_hash: &[u8],
        signature: &[u8],
        recovery_param: u8,
    ) -> Result<Vec<u8>, RecoverPubkeyError> {
        todo!()
    }

    fn ed25519_verify(
        &self,
        message: &[u8],
        signature: &[u8],
        public_key: &[u8],
    ) -> Result<bool, VerificationError> {
        todo!()
    }

    fn ed25519_batch_verify(
        &self,
        messages: &[&[u8]],
        signatures: &[&[u8]],
        public_keys: &[&[u8]],
    ) -> Result<bool, VerificationError> {
        todo!()
    }

    fn debug(&self, message: &str) {
        println!("{message}");
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

    pub fn address(&self) -> &str {
        self.address.as_str()
    }

    pub fn api(&self) -> MockApi {
        self.instance.api().clone()
    }

    pub fn with_storage<F: FnOnce(&mut dyn StdStorage) -> VmResult<T>, T>(
        &mut self,
        func: F,
    ) -> VmResult<T> {
        self.instance.with_storage(|store| func(&mut store.wrap()))
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

pub struct StorageWrapper<'a> {
    store: &'a mut MockStorage,
}

impl StdStorage for StorageWrapper<'_> {
    fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.store.get(key).0.unwrap()
    }

    fn set(&mut self, key: &[u8], value: &[u8]) {
        if value.is_empty() {
            panic!("TL;DR: Value must not be empty in Storage::set but in most cases you can use Storage::remove instead. Long story: Getting empty values from storage is not well supported at the moment. Some of our internal interfaces cannot differentiate between a non-existent key and an empty value. Right now, you cannot rely on the behaviour of empty values. To protect you from trouble later on, we stop here. Sorry for the inconvenience! We highly welcome you to contribute to CosmWasm, making this more solid one way or the other.");
        }

        self.store.set(key, value).0.unwrap()
    }

    fn remove(&mut self, key: &[u8]) {
        self.store.remove(key).0.unwrap()
    }

    fn range<'a>(
        &'a self,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        order: Order,
    ) -> Box<dyn Iterator<Item = Record> + 'a> {
        let bounds = range_bounds(start, end);

        // BTreeMap.range panics if range is start > end.
        // However, this cases represent just empty range and we treat it as such.
        match (bounds.start_bound(), bounds.end_bound()) {
            (Bound::Included(start), Bound::Excluded(end)) if start > end => {
                return Box::new(iter::empty());
            }
            _ => {}
        }

        let iter = self.store.data.range(bounds);
        match order {
            Order::Ascending => Box::new(iter.map(clone_item)),
            Order::Descending => Box::new(iter.rev().map(clone_item)),
        }
    }
}
