use crate::Event;
use crate::exit_reason_to_u8;
use crate::utils::*;
use crate::EventListener;
use ethjson::maybe::MaybeEmpty;
use ethjson::spec::ForkSpec;
use ethjson::transaction::Transaction;
use evm::backend::{ApplyBackend, MemoryAccount, MemoryBackend, MemoryVicinity};
use evm::executor::stack::{
	MemoryStackState, PrecompileFailure, PrecompileFn, PrecompileOutput, StackExecutor,
	StackSubstateMetadata,
};
use evm::{Config, Context, ExitError, ExitSucceed};
use evm_runtime::tracing::using;
use lazy_static::lazy_static;
use libsecp256k1::SecretKey;
use primitive_types::{H160, H256, U256};
use serde::Deserialize;
use sha3::{Digest, Keccak256};
use std::collections::BTreeMap;
use std::convert::TryInto;
use std::fs::write;

const GAS_CREATE: u64 = 32000; // Intrinsic create cost
const GAS_TRANSACTION: u64 = 21000; // Intrinsic transaction cost

#[derive(Deserialize, Debug)]
pub struct Test(ethjson::test_helpers::state::State);

impl Test {
	pub fn unwrap_to_pre_state(&self) -> BTreeMap<H160, MemoryAccount> {
		unwrap_to_state(&self.0.pre_state)
	}

	pub fn unwrap_caller(&self) -> H160 {
		let hash: H256 = self.0.transaction.secret.unwrap().into();
		let mut secret_key = [0; 32];
		secret_key.copy_from_slice(hash.as_bytes());
		let secret = SecretKey::parse(&secret_key);
		let public = libsecp256k1::PublicKey::from_secret_key(&secret.unwrap());
		let mut res = [0u8; 64];
		res.copy_from_slice(&public.serialize()[1..65]);

		H160::from(H256::from_slice(Keccak256::digest(res).as_slice()))
	}

	pub fn unwrap_to_vicinity(&self, spec: &ForkSpec) -> Option<MemoryVicinity> {
		let block_base_fee_per_gas = self.0.env.block_base_fee_per_gas.0;
		let gas_price = if self.0.transaction.gas_price.0.is_zero() {
			let max_fee_per_gas = self.0.transaction.max_fee_per_gas.0;

			// max_fee_per_gas is only defined for London and later
			if !max_fee_per_gas.is_zero() && spec < &ForkSpec::London {
				return None;
			}

			// Cannot specify a lower fee than the base fee
			if max_fee_per_gas < block_base_fee_per_gas {
				return None;
			}

			let max_priority_fee_per_gas = self.0.transaction.max_priority_fee_per_gas.0;

			// priority fee must be lower than regaular fee
			if max_fee_per_gas < max_priority_fee_per_gas {
				return None;
			}

			let priority_fee_per_gas = std::cmp::min(
				max_priority_fee_per_gas,
				max_fee_per_gas - block_base_fee_per_gas,
			);
			priority_fee_per_gas + block_base_fee_per_gas
		} else {
			self.0.transaction.gas_price.0
		};

		// gas price cannot be lower than base fee
		if gas_price < block_base_fee_per_gas {
			return None;
		}

		let block_randomness = if spec.is_eth2() {
			self.0.env.random.map(|r| {
                // Convert between U256 and H256. U256 is in little-endian but since H256 is just
                // a string-like byte array, it's big endian (MSB is the first element of the array).
                //
                // Byte order here is important because this opcode has the same value as DIFFICULTY
                // (0x44), and so for older forks of Ethereum, the threshold value of 2^64 is used to
                // distinguish between the two: if it's below, the value corresponds to the DIFFICULTY
                // opcode, otherwise to the PREVRANDAO opcode.
                let mut buf = [0u8; 32];
                r.0.to_big_endian(&mut buf);
                H256(buf)
            })
		} else {
			None
		};


		Some(MemoryVicinity {
			gas_price,
			origin: self.unwrap_caller(),
			block_hashes: Vec::new(),
			block_number: self.0.env.number.into(),
			block_coinbase: self.0.env.author.into(),
			block_timestamp: self.0.env.timestamp.into(),
			block_difficulty: self.0.env.difficulty.into(),
			block_gas_limit: self.0.env.gas_limit.into(),
			chain_id: U256::one(),
			block_base_fee_per_gas,
			block_randomness,
		})
	}
}

lazy_static! {
	static ref ISTANBUL_BUILTINS: BTreeMap<H160, ethcore_builtin::Builtin> =
		JsonPrecompile::builtins("./res/istanbul_builtins.json");
}

lazy_static! {
	static ref BERLIN_BUILTINS: BTreeMap<H160, ethcore_builtin::Builtin> =
		JsonPrecompile::builtins("./res/berlin_builtins.json");
}

macro_rules! precompile_entry {
	($map:expr, $builtins:expr, $index:expr) => {
		let x: PrecompileFn =
			|input: &[u8], gas_limit: Option<u64>, _context: &Context, _is_static: bool| {
				let builtin = $builtins.get(&H160::from_low_u64_be($index)).unwrap();
				Self::exec_as_precompile(builtin, input, gas_limit)
			};
		$map.insert(H160::from_low_u64_be($index), x);
	};
}

pub struct JsonPrecompile;

impl JsonPrecompile {
	pub fn precompile(spec: &ForkSpec) -> Option<BTreeMap<H160, PrecompileFn>> {
		match spec {
			ForkSpec::Istanbul => {
				let mut map = BTreeMap::new();
				precompile_entry!(map, ISTANBUL_BUILTINS, 1);
				precompile_entry!(map, ISTANBUL_BUILTINS, 2);
				precompile_entry!(map, ISTANBUL_BUILTINS, 3);
				precompile_entry!(map, ISTANBUL_BUILTINS, 4);
				precompile_entry!(map, ISTANBUL_BUILTINS, 5);
				precompile_entry!(map, ISTANBUL_BUILTINS, 6);
				precompile_entry!(map, ISTANBUL_BUILTINS, 7);
				precompile_entry!(map, ISTANBUL_BUILTINS, 8);
				precompile_entry!(map, ISTANBUL_BUILTINS, 9);
				Some(map)
			}
			ForkSpec::Berlin => {
				let mut map = BTreeMap::new();
				precompile_entry!(map, BERLIN_BUILTINS, 1);
				precompile_entry!(map, BERLIN_BUILTINS, 2);
				precompile_entry!(map, BERLIN_BUILTINS, 3);
				precompile_entry!(map, BERLIN_BUILTINS, 4);
				precompile_entry!(map, BERLIN_BUILTINS, 5);
				precompile_entry!(map, BERLIN_BUILTINS, 6);
				precompile_entry!(map, BERLIN_BUILTINS, 7);
				precompile_entry!(map, BERLIN_BUILTINS, 8);
				precompile_entry!(map, BERLIN_BUILTINS, 9);
				Some(map)
			}
			// precompiles for London and Berlin are the same
			ForkSpec::London => Self::precompile(&ForkSpec::Berlin),
			// precompiles for Merge and Berlin are the same
			ForkSpec::Merge => Self::precompile(&ForkSpec::Berlin),
			// precompiles for Shanghai and Berlin are the same
			ForkSpec::Shanghai => Self::precompile(&ForkSpec::Berlin),
			_ => None,
		}
	}

	fn builtins(spec_path: &str) -> BTreeMap<H160, ethcore_builtin::Builtin> {
		let reader = std::fs::File::open(spec_path).unwrap();
		let builtins: BTreeMap<ethjson::hash::Address, ethjson::spec::builtin::BuiltinCompat> =
			serde_json::from_reader(reader).unwrap();
		builtins
			.into_iter()
			.map(|(address, builtin)| {
				(
					address.into(),
					ethjson::spec::Builtin::from(builtin).try_into().unwrap(),
				)
			})
			.collect()
	}

	fn exec_as_precompile(
		builtin: &ethcore_builtin::Builtin,
		input: &[u8],
		gas_limit: Option<u64>,
	) -> Result<(PrecompileOutput, u64), PrecompileFailure> {
		let cost = builtin.cost(input, 0);

		if let Some(target_gas) = gas_limit {
			if cost > U256::from(u64::MAX) || target_gas < cost.as_u64() {
				return Err(PrecompileFailure::Error {
					exit_status: ExitError::OutOfGas,
				});
			}
		}

		let mut output = Vec::new();
		match builtin.execute(input, &mut parity_bytes::BytesRef::Flexible(&mut output)) {
			Ok(()) => Ok((
				PrecompileOutput {
					exit_status: ExitSucceed::Stopped,
					output,
				},
				cost.as_u64(),
			)),
			Err(e) => Err(PrecompileFailure::Error {
				exit_status: ExitError::Other(e.into()),
			}),
		}
	}
}

pub fn generate_move_test_file(test: &Test, transaction: &Transaction) {
	let mut content = String::from("");
	content.push_str("#[test_only]\n");
	content.push_str("module devm::steps {\n");
	content.push_str("  #[test(owner = @devm)]\n");
  content.push_str("  fun test(owner: signer) {\n");
  content.push_str("    aptos_framework::account::create_account_for_test(std::signer::address_of(&owner));\n");
  content.push_str("    devm::evm::initialize(&owner);\n");
	content.push_str("    let changes = &mut devm::state::new_changes();\n");
	content.push_str(&format!("    devm::state::set_basic(changes, @{:?}, {}, {});\n", test.unwrap_caller(), 0, 1_000_000_000));
	for (address, account) in test.unwrap_to_pre_state().into_iter() {
		content.push_str(&format!("    devm::state::set_basic(changes, @{:?}, {}, {});\n", address, account.nonce, account.balance));
		if account.code.len() > 0 {
			content.push_str(&format!("    devm::state::set_code(changes, @{:?}, x\"{}\");\n", address, hex::encode(account.code)));
		}
		if account.storage.len() > 0 {
			for (index, value) in account.storage.iter() {
				content.push_str(&format!("    devm::state::set_storage(changes, @{:?}, {:?}, {:?});\n", address, index, value));
			}
		}
	}
	content.push_str(&format!("    devm::state::apply(changes);\n\n"));
	if let MaybeEmpty::Some(to) = transaction.to { // Regular transaction
		content.push_str(&format!("    let params = devm::evm::new_run_params(@{:?}, @{:?}, devm::state::get_code(changes, @{:?}), {}, x\"{}\", {:#x}, {:#x});\n", test.unwrap_caller(), to.0, to.0, transaction.value.0, hex::encode(transaction.data.to_vec()), transaction.gas_limit.0.as_u64() - GAS_TRANSACTION, transaction.gas_price.0.as_u64()));
		content.push_str("    let (output, exit_reason, logs, gas) = devm::evm::run(params, &mut devm::state::new_changes(), true);\n");
		content.push_str("    devm::evm::print_output(output, exit_reason, logs, gas);\n");
	} else { // Create transaction
		content.push_str(&format!("    let params = devm::evm::new_run_params(@{:?}, @0x6295ee1b4f6dd65047762f924ecd367c17eabf8f, x\"{}\", {}, x\"\", {:#x}, {:#x});\n", test.unwrap_caller(), hex::encode(transaction.data.to_vec()), transaction.value.0, transaction.gas_limit.0.as_u64() - GAS_CREATE - GAS_TRANSACTION, transaction.gas_price.0.as_u64()));
		content.push_str("    let (_, exit_reason, logs, gas) = devm::evm::run(params, &mut devm::state::new_changes(), true);\n");
		content.push_str("    devm::evm::print_output(vector[], exit_reason, logs, gas);\n"); // Create doesn't print the output
	}
	content.push_str("  }\n");
	content.push_str("}\n");

	let file_path = "/Users/bulent/Desktop/Blockchain/EVM/devm/tests/steps.move";
	write(file_path, content).expect("Unable to write the steps test file");
}

pub fn test(name: &str, test: Test) {
	use std::thread;

	const STACK_SIZE: usize = 16 * 1024 * 1024;

	let name = name.to_string();
	// Spawn thread with explicit stack size
	let child = thread::Builder::new()
		.stack_size(STACK_SIZE)
		.spawn(move || test_run(&name, test))
		.unwrap();

	// Wait for thread to join
	child.join().unwrap();
}

fn test_run(name: &str, test: Test) {
	for (spec, states) in &test.0.post_states {
		let (gasometer_config, delete_empty) = match spec {
			ethjson::spec::ForkSpec::Istanbul => continue,
			ethjson::spec::ForkSpec::Berlin => continue,
			ethjson::spec::ForkSpec::London => (Config::london(), true),
			ethjson::spec::ForkSpec::Merge => (Config::merge(), true),
			ethjson::spec::ForkSpec::Shanghai => (Config::shanghai(), true),
			spec => {
				println!("Skip spec {spec:?}");
				continue;
			}
		};

		let original_state = test.unwrap_to_pre_state();
		let vicinity = test.unwrap_to_vicinity(spec);
		if vicinity.is_none() {
			// if vicinity could not be computed then the transaction was invalid so we simply
			// check the original state and move on
			assert_valid_hash(&states.first().unwrap().hash.0, &original_state);
			continue;
		}
		let vicinity = vicinity.unwrap();
		let caller = test.unwrap_caller();
		let caller_balance = original_state.get(&caller).unwrap().balance;

		for (i, state) in states.iter().enumerate() {
			// if i != 0 { continue; }
			print!("Running {}:{:?}:{} ... ", name, spec, i);
			flush();

			let transaction = test.0.transaction.select(&state.indexes);
			let mut backend = MemoryBackend::new(&vicinity, original_state.clone());

			generate_move_test_file(&test, &transaction);
			let steps = crate::run_move_test();

			// Only execute valid transactions
			if let Ok(transaction) = crate::utils::transaction::validate(
				transaction,
				test.0.env.gas_limit.0,
				caller_balance,
				&gasometer_config,
			) {
				let gas_limit: u64 = transaction.gas_limit.into();
				let data: Vec<u8> = transaction.data.into();
				let metadata =
					StackSubstateMetadata::new(transaction.gas_limit.into(), &gasometer_config);
				let executor_state = MemoryStackState::new(metadata, &backend);
				let precompile = JsonPrecompile::precompile(spec).unwrap();
				let mut executor = StackExecutor::new_with_precompiles(
					executor_state,
					&gasometer_config,
					&precompile,
				);
				let total_fee = vicinity.gas_price * gas_limit;

				executor.state_mut().withdraw(caller, total_fee).unwrap();

				let access_list = transaction
					.access_list
					.into_iter()
					.map(|(address, keys)| (address.0, keys.into_iter().map(|k| k.0).collect()))
					.collect();

				let mut el = EventListener { events: vec![] };
				let Transaction { to, value, ..} = transaction;
				let reason = using(&mut el, || {
				// let mut el2 = EventListener { events: vec![] };
				// evm::tracing::using(&mut el2, || {
				// let mut el2 = EventListener { events: vec![] };
				// evm::gasometer::tracing::using(&mut el2, || {
				match to {
					ethjson::maybe::MaybeEmpty::Some(to) => {
						let data = data;
						let value = value.into();

						executor.transact_call(
							caller,
							to.into(),
							value,
							data,
							gas_limit,
							access_list,
						)
					}
					ethjson::maybe::MaybeEmpty::None => {
						let code = data;
						let value = value.into();

						executor.transact_create(caller, value, code, gas_limit, access_list)
					}
				}
				// })
				// })
				});

				let actual_fee = executor.fee(vicinity.gas_price);
				// Forks after London burn miner rewards and thus have different gas fee
				// calculation (see EIP-1559)
				let miner_reward = if spec.is_eth2() {
					let max_priority_fee_per_gas = test.0.transaction.max_priority_fee_per_gas();
					let max_fee_per_gas = test.0.transaction.max_fee_per_gas();
					let base_fee_per_gas = vicinity.block_base_fee_per_gas;
					let priority_fee_per_gas =
						std::cmp::min(max_priority_fee_per_gas, max_fee_per_gas - base_fee_per_gas);
					executor.fee(priority_fee_per_gas)
				} else {
					actual_fee
				};

				executor
					.state_mut()
					.deposit(vicinity.block_coinbase, miner_reward);
				executor.state_mut().deposit(caller, total_fee - actual_fee);

				let used_gas = executor.used_gas();
				let (values, logs) = executor.into_state().deconstruct();

				backend.apply(values, logs, delete_empty);
				el.events.push(crate::Event::Exit { output: reason.1, exit_reason: exit_reason_to_u8(&reason.0), logs: backend.logs().to_owned(), gas: gas_limit - used_gas });

				let mut steps = steps.unwrap_or_else(|_| {println!("There's a problem with dEVM"); vec![]});
				Event::copy_static_cafe_values(&mut steps, &el.events);

				if steps == el.events {
					print!("Same steps ... ");
				} else {
					Event::print_compare(&steps, &el.events);
					println!("Gas Start: {:#x}", gas_limit);
					println!("Gas Left:  {:#x}", gas_limit - used_gas);
					println!("Gas Used:  {}", used_gas);
					// panic!("The steps are not equal");
				}
			}

			assert_valid_hash(&state.hash.0, backend.state());

			println!("passed");
		}
	}
}
