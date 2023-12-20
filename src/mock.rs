use bech32::{decode, encode, FromBase32, ToBase32, Variant};
use cosmwasm_schema::{
    schemars::JsonSchema,
    serde::{de::DeserializeOwned, Serialize},
};
use cosmwasm_std::{
    from_binary, Addr, Api, CanonicalAddr, Coin, ContractResult, Ibc3ChannelOpenResponse,
    IbcBasicResponse, IbcChannelCloseMsg, IbcChannelConnectMsg, IbcChannelOpenMsg, IbcPacketAckMsg,
    IbcPacketReceiveMsg, IbcPacketTimeoutMsg, IbcReceiveResponse, Order, Record,
    RecoverPubkeyError, Reply, Response, StdError, StdResult, Storage as StdStorage,
    VerificationError,
};
use cosmwasm_vm::{
    call_execute, call_ibc_channel_close, call_ibc_channel_connect, call_ibc_channel_open,
    call_ibc_packet_ack, call_ibc_packet_receive, call_ibc_packet_timeout, call_instantiate,
    call_migrate, call_query, call_reply, call_sudo,
    testing::{mock_env, mock_info, MockInstanceOptions, MockQuerier},
    to_vec, Backend, BackendApi, BackendError, BackendResult, GasInfo, Instance, InstanceOptions,
    Storage, VmResult,
};
use std::ops::{Bound, RangeBounds};
use std::{
    collections::{BTreeMap, HashMap},
    iter,
};

const BECH32_PREFIX: &str = "orai";
const GAS_COST_HUMANIZE: u64 = 44; // TODO: these seem very low
const GAS_COST_CANONICALIZE: u64 = 55;
pub const GAS_PER_US: u64 = 1_000_000;
const GAS_COST_LAST_ITERATION: u64 = 37;
const GAS_COST_RANGE: u64 = 11;

/// The equivalent of the `?` operator, but for a [`BackendResult`]
macro_rules! try_br {
    ($res: expr $(,)?) => {
        let (result, gas) = $res;

        match result {
            Ok(v) => v,
            Err(e) => return (Err(e), gas),
        }
    };
}

/// Does basic validation of the number of bytes in a canonical address
fn validate_length(bytes: &[u8]) -> Result<(), BackendError> {
    match bytes.len() {
        1..=255 => Ok(()),
        _ => Err(BackendError::user_err("Invalid canonical address length")),
    }
}

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

#[derive(Copy, Clone)]
pub struct MockApi(MockApiImpl);

#[derive(Copy, Clone)]
enum MockApiImpl {
    /// With this variant, all calls to the API fail with BackendError::Unknown
    /// containing the given message
    Error(&'static str),
    /// This variant implements Bech32 addresses.
    Bech32 {
        /// Prefix used for creating addresses in Bech32 encoding.
        bech32_prefix: &'static str,
    },
}

impl MockApi {
    pub fn new_failing(backend_error: &'static str) -> Self {
        Self(MockApiImpl::Error(backend_error))
    }

    /// Returns [MockApi] with Bech32 prefix set to provided value.
    ///
    /// Bech32 prefix must not be empty.
    ///
    /// # Example
    ///
    /// ```
    /// # use cosmwasm_std::Addr;
    /// # use cosmwasm_std::testing::MockApi;
    /// #
    /// let mock_api = MockApi::default().with_prefix("juno");
    /// let addr = mock_api.addr_make("creator");
    ///
    /// assert_eq!(addr.as_str(), "juno1h34lmpywh4upnjdg90cjf4j70aee6z8qqfspugamjp42e4q28kqsksmtyp");
    /// ```
    pub fn with_prefix(self, prefix: &'static str) -> Self {
        Self(MockApiImpl::Bech32 {
            bech32_prefix: prefix,
        })
    }
}

impl Default for MockApi {
    fn default() -> Self {
        Self(MockApiImpl::Bech32 {
            bech32_prefix: BECH32_PREFIX,
        })
    }
}

impl BackendApi for MockApi {
    fn canonical_address(&self, input: &str) -> BackendResult<Vec<u8>> {
        let gas_info = GasInfo::with_cost(GAS_COST_CANONICALIZE);

        // handle error case
        let bech32_prefix = match self.0 {
            MockApiImpl::Error(e) => return (Err(BackendError::unknown(e)), gas_info),
            MockApiImpl::Bech32 { bech32_prefix } => bech32_prefix,
        };

        match decode(input) {
            Ok((prefix, _, _)) if prefix != bech32_prefix => {
                (Err(BackendError::user_err("Wrong bech32 prefix")), gas_info)
            }
            Ok((_, _, Variant::Bech32m)) => (
                Err(BackendError::user_err("Wrong bech32 variant")),
                gas_info,
            ),
            Err(_) => (
                Err(BackendError::user_err("Error decoding bech32")),
                gas_info,
            ),
            Ok((_, decoded, Variant::Bech32)) => match Vec::<u8>::from_base32(&decoded) {
                Ok(bytes) => {
                    try_br!((validate_length(&bytes), gas_info));
                    (Ok(bytes), gas_info)
                }
                Err(_) => (Err(BackendError::user_err("Invalid bech32 data")), gas_info),
            },
        }
    }

    fn human_address(&self, canonical: &[u8]) -> BackendResult<String> {
        let gas_info = GasInfo::with_cost(GAS_COST_HUMANIZE);

        // handle error case
        let bech32_prefix = match self.0 {
            MockApiImpl::Error(e) => return (Err(BackendError::unknown(e)), gas_info),
            MockApiImpl::Bech32 { bech32_prefix } => bech32_prefix,
        };

        try_br!((validate_length(canonical), gas_info));

        let result = encode(bech32_prefix, canonical.to_base32(), Variant::Bech32)
            .map_err(|_| BackendError::user_err("Invalid bech32 prefix"));

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
        Ok(cosmwasm_crypto::secp256k1_verify(
            message_hash,
            signature,
            public_key,
        )?)
    }

    fn secp256k1_recover_pubkey(
        &self,
        message_hash: &[u8],
        signature: &[u8],
        recovery_param: u8,
    ) -> Result<Vec<u8>, RecoverPubkeyError> {
        Ok(cosmwasm_crypto::secp256k1_recover_pubkey(
            message_hash,
            signature,
            recovery_param,
        )?)
    }

    fn ed25519_verify(
        &self,
        message: &[u8],
        signature: &[u8],
        public_key: &[u8],
    ) -> Result<bool, VerificationError> {
        Ok(cosmwasm_crypto::ed25519_verify(
            message, signature, public_key,
        )?)
    }

    fn ed25519_batch_verify(
        &self,
        messages: &[&[u8]],
        signatures: &[&[u8]],
        public_keys: &[&[u8]],
    ) -> Result<bool, VerificationError> {
        Ok(cosmwasm_crypto::ed25519_batch_verify(
            messages,
            signatures,
            public_keys,
        )?)
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
        let serialized_msg =
            to_vec(&msg).expect("Testing error: Could not seralize request message");
        let ret = match call_migrate(&mut self.instance, &env, &serialized_msg) {
            Ok(ContractResult::Ok(ret)) => ret,
            Ok(ContractResult::Err(error)) => return ContractResult::Err(error),
            Err(error) => return ContractResult::Err(error.to_string()),
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

        let serialized_msg =
            to_vec(&msg).expect("Testing error: Could not seralize request message");

        let ret = match call_instantiate(&mut self.instance, &env, &info, &serialized_msg) {
            Ok(ContractResult::Ok(ret)) => ret,
            Ok(ContractResult::Err(error)) => return ContractResult::Err(error),
            Err(error) => {
                return ContractResult::Err(error.to_string());
            }
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
        let serialized_msg =
            to_vec(&msg).expect("Testing error: Could not seralize request message");
        let ret = match call_execute(&mut self.instance, &env, &info, &serialized_msg) {
            Ok(ContractResult::Ok(ret)) => ret,
            Ok(ContractResult::Err(error)) => return ContractResult::Err(error),
            Err(error) => return ContractResult::Err(error.to_string()),
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
        let serialized_msg =
            to_vec(&msg).expect("Testing error: Could not seralize request message");
        let ret: T = match call_query(&mut self.instance, &env, &serialized_msg) {
            Ok(ContractResult::Ok(binary)) => match from_binary(&binary) {
                Ok(ret) => ret,
                Err(error) => return ContractResult::Err(error.to_string()),
            },
            Ok(ContractResult::Err(error)) => return ContractResult::Err(error),
            Err(error) => return ContractResult::Err(error.to_string()),
        };
        let gas_used = (gas_before - self.instance.get_gas_left()) / GAS_PER_US;
        ContractResult::Ok((ret, gas_used))
    }

    pub fn reply(&mut self, msg: Reply) -> ContractResult<(Response, u64)> {
        let mut env = mock_env();
        env.contract.address = self.address.clone();
        let gas_before = self.instance.get_gas_left();
        let ret = match call_reply(&mut self.instance, &env, &msg) {
            Ok(ContractResult::Ok(ret)) => ret,
            Ok(ContractResult::Err(error)) => return ContractResult::Err(error),
            Err(error) => return ContractResult::Err(error.to_string()),
        };
        let gas_used = (gas_before - self.instance.get_gas_left()) / GAS_PER_US;
        ContractResult::Ok((ret, gas_used))
    }

    pub fn sudo<M: Serialize + JsonSchema>(&mut self, msg: M) -> ContractResult<(Response, u64)> {
        let mut env = mock_env();
        env.contract.address = self.address.clone();
        let gas_before = self.instance.get_gas_left();
        let serialized_msg =
            to_vec(&msg).expect("Testing error: Could not seralize request message");
        let ret = match call_sudo(&mut self.instance, &env, &serialized_msg) {
            Ok(ContractResult::Ok(ret)) => ret,
            Ok(ContractResult::Err(error)) => return ContractResult::Err(error),
            Err(error) => return ContractResult::Err(error.to_string()),
        };
        let gas_used = (gas_before - self.instance.get_gas_left()) / GAS_PER_US;
        ContractResult::Ok((ret, gas_used))
    }

    pub fn ibc_channel_open(
        &mut self,
        msg: IbcChannelOpenMsg,
    ) -> ContractResult<(Option<Ibc3ChannelOpenResponse>, u64)> {
        let mut env = mock_env();
        env.contract.address = self.address.clone();
        let gas_before = self.instance.get_gas_left();
        let ret = match call_ibc_channel_open(&mut self.instance, &env, &msg) {
            Ok(ContractResult::Ok(ret)) => ret,
            Ok(ContractResult::Err(error)) => return ContractResult::Err(error),
            Err(error) => return ContractResult::Err(error.to_string()),
        };
        let gas_used = (gas_before - self.instance.get_gas_left()) / GAS_PER_US;
        ContractResult::Ok((ret, gas_used))
    }

    pub fn ibc_channel_connect(
        &mut self,
        msg: IbcChannelConnectMsg,
    ) -> ContractResult<(IbcBasicResponse, u64)> {
        let mut env = mock_env();
        env.contract.address = self.address.clone();
        let gas_before = self.instance.get_gas_left();
        let ret = match call_ibc_channel_connect(&mut self.instance, &env, &msg) {
            Ok(ContractResult::Ok(ret)) => ret,
            Ok(ContractResult::Err(error)) => return ContractResult::Err(error),
            Err(error) => return ContractResult::Err(error.to_string()),
        };
        let gas_used = (gas_before - self.instance.get_gas_left()) / GAS_PER_US;
        ContractResult::Ok((ret, gas_used))
    }

    pub fn ibc_channel_close(
        &mut self,
        msg: IbcChannelCloseMsg,
    ) -> ContractResult<(IbcBasicResponse, u64)> {
        let mut env = mock_env();
        env.contract.address = self.address.clone();
        let gas_before = self.instance.get_gas_left();
        let ret = match call_ibc_channel_close(&mut self.instance, &env, &msg) {
            Ok(ContractResult::Ok(ret)) => ret,
            Ok(ContractResult::Err(error)) => return ContractResult::Err(error),
            Err(error) => return ContractResult::Err(error.to_string()),
        };
        let gas_used = (gas_before - self.instance.get_gas_left()) / GAS_PER_US;
        ContractResult::Ok((ret, gas_used))
    }

    pub fn ibc_packet_receive(
        &mut self,
        msg: IbcPacketReceiveMsg,
    ) -> ContractResult<(IbcReceiveResponse, u64)> {
        let mut env = mock_env();
        env.contract.address = self.address.clone();
        let gas_before = self.instance.get_gas_left();
        let ret = match call_ibc_packet_receive(&mut self.instance, &env, &msg) {
            Ok(ContractResult::Ok(ret)) => ret,
            Ok(ContractResult::Err(error)) => return ContractResult::Err(error),
            Err(error) => return ContractResult::Err(error.to_string()),
        };
        let gas_used = (gas_before - self.instance.get_gas_left()) / GAS_PER_US;
        ContractResult::Ok((ret, gas_used))
    }

    pub fn ibc_packet_ack(
        &mut self,
        msg: IbcPacketAckMsg,
    ) -> ContractResult<(IbcBasicResponse, u64)> {
        let mut env = mock_env();
        env.contract.address = self.address.clone();
        let gas_before = self.instance.get_gas_left();
        let ret = match call_ibc_packet_ack(&mut self.instance, &env, &msg) {
            Ok(ContractResult::Ok(ret)) => ret,
            Ok(ContractResult::Err(error)) => return ContractResult::Err(error),
            Err(error) => return ContractResult::Err(error.to_string()),
        };
        let gas_used = (gas_before - self.instance.get_gas_left()) / GAS_PER_US;
        ContractResult::Ok((ret, gas_used))
    }

    pub fn ibc_packet_timeout(
        &mut self,
        msg: IbcPacketTimeoutMsg,
    ) -> ContractResult<(IbcBasicResponse, u64)> {
        let mut env = mock_env();
        env.contract.address = self.address.clone();
        let gas_before = self.instance.get_gas_left();
        let ret = match call_ibc_packet_timeout(&mut self.instance, &env, &msg) {
            Ok(ContractResult::Ok(ret)) => ret,
            Ok(ContractResult::Err(error)) => return ContractResult::Err(error),
            Err(error) => return ContractResult::Err(error.to_string()),
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
