use crate::{exit_reason_to_u8, utils::*, Event, EventListener};
use evm::backend::{ApplyBackend, MemoryAccount, MemoryBackend, MemoryVicinity};
use evm::executor::stack::{MemoryStackState, StackExecutor, StackSubstateMetadata};
use evm::Config;
use evm_runtime::tracing::using;
use primitive_types::{H160, H256, U256};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs::write;
use std::path::Path;
use std::rc::Rc;

#[derive(Deserialize, Debug)]
pub struct Test(ethjson::vm::Vm);

impl Test {
	pub fn unwrap_to_pre_state(&self) -> BTreeMap<H160, MemoryAccount> {
		unwrap_to_state(&self.0.pre_state)
	}

	pub fn unwrap_to_vicinity(&self) -> MemoryVicinity {
		let block_randomness = self.0.env.random.map(|r| {
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
		});

		MemoryVicinity {
			gas_price: self.0.transaction.gas_price.clone().into(),
			origin: self.0.transaction.origin.clone().into(),
			block_hashes: Vec::new(),
			block_number: self.0.env.number.clone().into(),
			block_coinbase: self.0.env.author.clone().into(),
			block_timestamp: self.0.env.timestamp.clone().into(),
			block_difficulty: self.0.env.difficulty.clone().into(),
			block_gas_limit: self.0.env.gas_limit.clone().into(),
			chain_id: U256::zero(),
			block_base_fee_per_gas: self.0.transaction.gas_price.clone().into(),
			block_randomness,
		}
	}

	pub fn unwrap_to_code(&self) -> Rc<Vec<u8>> {
		Rc::new(self.0.transaction.code.clone().into())
	}

	pub fn unwrap_to_data(&self) -> Rc<Vec<u8>> {
		Rc::new(self.0.transaction.data.clone().into())
	}

	pub fn unwrap_to_context(&self) -> evm::Context {
		evm::Context {
			address: self.0.transaction.address.clone().into(),
			caller: self.0.transaction.sender.clone().into(),
			apparent_value: self.0.transaction.value.clone().into(),
		}
	}

	pub fn unwrap_to_return_value(&self) -> Vec<u8> {
		self.0.output.clone().unwrap().into()
	}

	pub fn unwrap_to_gas_limit(&self) -> u64 {
		self.0.transaction.gas.clone().into()
	}

	pub fn unwrap_to_post_gas(&self) -> u64 {
		self.0.gas_left.clone().unwrap().into()
	}
}

pub fn generate_move_test_file(test: &Test, devm_path: &Path) {
	let mut content = String::from("");
	content.push_str("#[test_only]\n");
	content.push_str("module devm::steps {\n");
	content.push_str("  #[test(owner = @devm)]\n");
	content.push_str("  fun test(owner: signer) {\n");
	content.push_str(
		"    aptos_framework::account::create_account_for_test(std::signer::address_of(&owner));\n",
	);
	content.push_str("    devm::evm::initialize(&owner);\n");
	content.push_str("    let changes = &mut devm::state::new_changes();\n");
	content.push_str(&format!(
		"    devm::state::set_basic(changes, @{:?}, {}, {});\n",
		test.0.transaction.sender.0, 0, 1_000_000_000
	));
	for (address, account) in test.unwrap_to_pre_state().into_iter() {
		content.push_str(&format!(
			"    devm::state::set_basic(changes, @{:?}, {}, {});\n",
			address, account.nonce, account.balance
		));
		if account.code.len() > 0 {
			content.push_str(&format!(
				"    devm::state::set_code(changes, @{:?}, x\"{}\");\n",
				address,
				hex::encode(account.code)
			));
		}
		if account.storage.len() > 0 {
			for (index, value) in account.storage.iter() {
				content.push_str(&format!(
					"    devm::state::set_storage(changes, @{:?}, {:?}, {:?});\n",
					address, index, value
				));
			}
		}
	}
	content.push_str(&format!("    devm::state::apply(changes);\n\n"));
	let context = test.unwrap_to_context();
	content.push_str(&format!("    let params = devm::evm::new_run_params(@{:?}, @{:?}, devm::state::get_code(changes, @{:?}), {}, x\"{}\", {:#x}, {:#x});\n", context.caller, context.address, context.address, context.apparent_value, hex::encode(test.unwrap_to_data().to_vec()), test.0.transaction.gas.0.as_u64(), test.0.transaction.gas_price.0.as_u64()));
	content.push_str("    let (output, exit_reason, logs, gas) = devm::evm::run(params, &mut devm::state::new_changes(), true);\n");
	content.push_str("    devm::evm::print_output(output, exit_reason, logs, gas);\n");
	content.push_str("  }\n");
	content.push_str("}\n");

	let file_path = devm_path.join("tests").join("steps.move");
	write(file_path, content).expect("Unable to write the steps test file");
}

pub fn test(name: &str, test: Test, devm_path: &Path) {
	print!("Running test {} ... ", name);
	flush();

	let original_state = test.unwrap_to_pre_state();
	let vicinity = test.unwrap_to_vicinity();
	let config = Config::shanghai();
	let mut backend = MemoryBackend::new(&vicinity, original_state);
	let metadata = StackSubstateMetadata::new(test.unwrap_to_gas_limit(), &config);
	let state = MemoryStackState::new(metadata, &mut backend);
	let precompile = BTreeMap::new();
	let mut executor = StackExecutor::new_with_precompiles(state, &config, &precompile);

	let code = test.unwrap_to_code();
	let data = test.unwrap_to_data();
	let context = test.unwrap_to_context();
	let mut runtime =
		evm::Runtime::new(code, data, context, config.stack_limit, config.memory_limit);

	generate_move_test_file(&test, devm_path);
	let steps = crate::run_move_test(devm_path);

	let mut el = EventListener { events: vec![] };
	let (reason, output) = using(&mut el, || {
		// let mut el2 = EventListener { events: vec![] };
		// evm::gasometer::tracing::using(&mut el2, || {
		executor.execute(&mut runtime)
		// })
	});

	let gas = executor.gas();
	let (values, logs) = executor.into_state().deconstruct();
	let logs: Vec<_> = logs.into_iter().collect();
	backend.apply(values, logs.clone(), false);

	el.events.push(crate::Event::Exit {
		output,
		exit_reason: exit_reason_to_u8(&reason),
		logs,
		gas,
	});

	let mut steps = steps.unwrap_or_else(|_| {
		println!("There's a problem with dEVM");
		vec![]
	});
	Event::copy_static_cafe_values(&mut steps, &el.events);

	if steps == el.events {
		println!("Same steps");
	} else {
		Event::print_compare(&steps, &el.events);
		if let Some(gas_left) = test.0.gas_left {
			println!("Gas Start: {:#x}", test.0.transaction.gas.0.as_u64());
			println!("Gas Left:  {:#x}", gas_left.0.as_u64());
			println!(
				"Gas Used:  {}",
				test.0.transaction.gas.0.as_u64() - gas_left.0.as_u64()
			);
		}
		// panic!("The steps are not equal");
	}

	println!("succeed");
}
