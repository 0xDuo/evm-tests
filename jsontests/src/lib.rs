mod utils;

pub mod state;
pub mod tracing;
pub mod vm;

use ethereum::Log;
use evm_runtime::{ExitError, ExitReason};
use primitive_types::{H160, H256};
use serde::Deserialize;
use std::{
	path::{Path, PathBuf},
	process::Command,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TestStatus {
	Passed,
	Failed,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum Event {
	Step {
		sender: H160,
		contract: H160,
		position: usize,
		opcode: u8,
		stack: Vec<H256>,
		#[serde(with = "hex_serde")]
		memory: Vec<u8>,
		gas_limit: u64,
		gas_cost: u64,
		depth: u32,
	},
	SLoad {
		address: H160,
		index: H256,
		value: H256,
	},
	SStore {
		address: H160,
		index: H256,
		value: H256,
	},
	Exit {
		#[serde(with = "hex_serde")]
		output: Vec<u8>,
		exit_reason: u8,
		logs: Vec<Log>,
		gas: u64,
	},
}

impl Event {
	fn print_compare(steps: &Vec<Event>, events: &Vec<Event>) {
		println!();
		// .zip or .iter
		// for (step, event) in steps.iter().zip(events)
		// Going to need a big match statements to show differences for each Event type
		for i in 0..std::cmp::min(steps.len(), events.len()) {
			let step = steps.get(i).unwrap();
			let event = events.get(i).unwrap();
			match step {
				// Show human readable opcode
				Self::Step { opcode, .. } => println!("\n{} Opcode", format!("{:#x}", opcode)),
				_ => println!(""),
			}
			if step == event {
				println!("{:?}", step);
			} else {
				println!("===> DEVM");
				println!("{:?}", step);
				println!("===> RUST EVM");
				println!("{:?}", event);
			}
		}
		for i in events.len()..steps.len() {
			println!("\n-------> Extra DEVM: {:?}", steps.get(i).unwrap());
		}
		for i in steps.len()..events.len() {
			println!("\n-------> Extra RUST EVM:{:?}", events.get(i).unwrap());
		}
		println!();
	}
}

/// Filled by EventListener as it processes tracing from sputnikVM.
/// Eventually pushed as `Event::Step` when finalized by `StepResult`.
#[derive(Debug, Default)]
struct IntermediateStep {
	sender: H160,
	contract: H160,
	position: usize,
	opcode: u8,
	stack: Vec<H256>,
	memory: Vec<u8>,
	gas_limit: u64,
	gas_cost: u64,
	depth: u32,
}

#[derive(Debug, Default)]
struct IntermediateExit {
	output: Vec<u8>,
	exit_reason: u8,
	gas: u64,
}

#[derive(Debug, Default)]
pub struct EventListener {
	pub events: Vec<Event>,
	current_step: IntermediateStep,
	current_step_consumed: bool,
	current_memory_gas: Vec<u64>,
	intermediate_exit: IntermediateExit,
}

impl EventListener {
	fn save_current_step(&mut self) {
		if !self.current_step_consumed {
			// By definition `OpCode::INVALID` consumes all the remaining gas,
			// but it is difficult to get Sputnik and devm to exactly agree on what the
			// gas limit was before that happened. It's just a tracing issue, not
			// a logic issue, so for consistency I choose to manually force both to 0.
			if self.current_step.opcode == 0xfe {
				self.current_step.gas_limit = 0;
				self.current_step.gas_cost = 0;
			}
			self.events.push(crate::Event::Step {
				sender: self.current_step.sender,
				contract: self.current_step.contract,
				position: self.current_step.position,
				opcode: self.current_step.opcode,
				stack: self.current_step.stack.clone(),
				memory: self.current_step.memory.clone(),
				gas_limit: self.current_step.gas_limit,
				gas_cost: self.current_step.gas_cost,
				depth: self.current_step.depth,
			});
		}
	}

	pub fn finish(&mut self, logs: Vec<Log>) {
		let new_event = Event::Exit {
			output: self.intermediate_exit.output.clone(),
			exit_reason: self.intermediate_exit.exit_reason,
			logs,
			gas: self.intermediate_exit.gas,
		};
		self.events.push(new_event);
	}

	/// Prune the trailing zeros
	pub fn prune_memory(memory: &[u8]) -> Vec<u8> {
		let last_non_zero = memory
			.iter()
			.rposition(|x| *x != 0)
			.map(|i| i + 1)
			.unwrap_or_default();
		memory[0..last_non_zero].to_owned()
	}
}

impl evm::tracing::EventListener for EventListener {
	fn event(&mut self, event: evm::tracing::Event<'_>) {
		use evm::tracing::Event;
		match event {
			Event::Call { .. } | Event::Create { .. } => {
				self.current_step.depth += 1;
				// Each call frame has its own memory gas
				self.current_memory_gas.push(0);
			}
			Event::Exit {
				reason,
				return_value,
			} => {
				ExitBehavior::new(reason).execute(self);
				self.current_step.depth = self.current_step.depth.saturating_sub(1);
				self.current_memory_gas.pop();
				self.intermediate_exit.exit_reason = exit_reason_to_u8(reason);
				self.intermediate_exit.output = return_value.to_vec();
			}
			Event::Suicide { .. }
			| Event::PrecompileSubcall { .. }
			| Event::TransactCall { .. }
			| Event::TransactCreate { .. }
			| Event::TransactCreate2 { .. } => (), // no useful information
		}
	}
}

impl evm::gasometer::tracing::EventListener for EventListener {
	fn event(&mut self, event: evm::gasometer::tracing::Event) {
		use evm::gasometer::tracing::Event;
		match event {
			Event::RecordCost { cost, snapshot } => {
				self.current_step.gas_cost = cost;
				if let Some(snapshot) = snapshot {
					self.current_step.gas_limit = snapshot
						.gas_limit
						.saturating_sub(snapshot.used_gas + snapshot.memory_gas);
				}
			}
			Event::RecordDynamicCost {
				gas_cost,
				memory_gas,
				gas_refund: _,
				snapshot,
			} => {
				if self.current_memory_gas.is_empty() {
					self.current_memory_gas.push(0);
				}
				// Unwrap is safe because the `if` statement above guarantees at least one item in the Vec.
				let current_memory_gas = self.current_memory_gas.last_mut().unwrap();
				// In SputnikVM memory gas is cumulative (ie this event always shows the total) gas
				// spent on memory up to this point. But geth traces simply show how much gas each step
				// took, regardless of how that gas was used. So if this step caused an increase to the
				// memory gas then we need to record that.
				let memory_cost_diff = if memory_gas > *current_memory_gas {
					memory_gas - *current_memory_gas
				} else {
					0
				};
				*current_memory_gas = memory_gas;
				self.current_step.gas_cost = gas_cost + memory_cost_diff;
				if let Some(snapshot) = snapshot {
					self.current_step.gas_limit = snapshot
						.gas_limit
						.saturating_sub(snapshot.used_gas + snapshot.memory_gas);
				}
			}
			Event::RecordRefund {
				refund: _,
				snapshot,
			} => {
				// This one seems to show up at the end of a transaction, so it
				// can be used to set the total gas used.
				if let Some(snapshot) = snapshot {
					self.intermediate_exit.gas = snapshot.gas_limit - snapshot.used_gas;
				}
			}
			Event::RecordTransaction { .. } | Event::RecordStipend { .. } => (), // not useful
		}
	}
}

impl evm_runtime::tracing::EventListener for EventListener {
	fn event(&mut self, event: evm_runtime::tracing::Event<'_>) {
		use evm_runtime::tracing::Event as RuntimeEvent;
		match event {
			RuntimeEvent::Step {
				context,
				opcode,
				position,
				stack,
				memory,
			} => {
				self.current_step.sender = context.caller;
				self.current_step.contract = context.address;
				self.current_step.position = *position.as_ref().unwrap();
				self.current_step.opcode = opcode.0;
				self.current_step.stack = stack.data().clone();
				self.current_step.memory = Self::prune_memory(&memory.data());
				self.current_step_consumed = false;
			}
			RuntimeEvent::SLoad {
				address,
				index,
				value,
			} => self.events.push(Event::SLoad {
				address: address.clone(),
				index: index.clone(),
				value: value.clone(),
			}),
			RuntimeEvent::SStore {
				address,
				index,
				value,
			} => self.events.push(Event::SStore {
				address: address.clone(),
				index: index.clone(),
				value: value.clone(),
			}),
			RuntimeEvent::StepResult {
				result: _,
				return_value: _,
			} => {
				self.save_current_step();
				self.current_step_consumed = true;
			}
		};
	}
}

pub fn exit_reason_to_u8(exit_reason: &ExitReason) -> u8 {
	match exit_reason {
		evm_runtime::ExitReason::Succeed(s) => s.clone() as u8,
		evm_runtime::ExitReason::Revert(_) => 0x10,
		evm_runtime::ExitReason::Fatal(_) => 0x20,
		evm_runtime::ExitReason::Error(e) => match e {
			ExitError::StackUnderflow => 0x30,
			ExitError::StackOverflow => 0x31,
			ExitError::InvalidJump => 0x32,
			ExitError::InvalidRange => 0x33,
			ExitError::DesignatedInvalid => 0x34,
			ExitError::CallTooDeep => 0x35,
			ExitError::OutOfOffset => 0x38,
			ExitError::OutOfGas => 0x39,
			ExitError::Other(_) => 0x3d,
			ExitError::InvalidCode(_) => 0x3f,
			_ => 0x30,
		},
	}
}

pub fn run_move_test(devm_path: &Path) -> anyhow::Result<Vec<Event>> {
	let aptos_path_env = std::env::var("APTOS_PATH");
	let aptos_program = aptos_path_env.as_deref().unwrap_or("aptos");
	// aptos move test --bytecode-version 6 -i 10000000 -f steps
	let command_output = Command::new(aptos_program)
		.current_dir(devm_path)
		.arg("move")
		.arg("test")
		.args(["--bytecode-version", "6"])
		.args(["-i", "10000000"])
		.args(["-f", "steps"])
		.output()
		.expect("Failed to call 'aptos move test' command");

	let output = String::from_utf8(command_output.stdout).expect("Failed to read command output");
	let output = output.replace("[debug] ", "");
	let output = output.replace("\"", "");

	let output = regex::Regex::new(r"INCLUDING.*")
		.unwrap()
		.replace_all(&output, "");
	let output = regex::Regex::new(r"BUILDING.*")
		.unwrap()
		.replace(&output, "");
	let output = regex::Regex::new(r"Running.*")
		.unwrap()
		.replace(&output, "");
	let output = regex::Regex::new(r"\[ PASS.*")
		.unwrap()
		.replace(&output, "");
	let output = regex::Regex::new(r"Test.*")
		.unwrap()
		.replace_all(&output, "");
	let output = regex::Regex::new(r"\{.*").unwrap().replace_all(&output, "");
	let output = regex::Regex::new(r"\}.*").unwrap().replace_all(&output, "");
	let output = regex::Regex::new(r".*Result.*")
		.unwrap()
		.replace(&output, "");

	let output = regex::Regex::new(r"\[ FAIL.*")
		.unwrap()
		.replace(&output, "");
	let output = regex::Regex::new(r"Failures.*")
		.unwrap()
		.replace(&output, "");
	let output = regex::Regex::new(r"┌.*").unwrap().replace(&output, "");
	let output = regex::Regex::new(r"│.*").unwrap().replace_all(&output, "");
	let output = regex::Regex::new(r"└.*").unwrap().replace(&output, "");
	let output = regex::Regex::new(r".*Error.*")
		.unwrap()
		.replace(&output, "");
	// println!("{}", output);
	let events: Vec<Event> = serde_yaml::from_str(&output).map_err(anyhow::Error::from)?;
	Ok(events
		.into_iter()
		.map(|mut e| {
			if let Event::Step {
				memory,
				opcode,
				gas_limit,
				gas_cost,
				..
			} = &mut e
			{
				let pruned = EventListener::prune_memory(memory);
				*memory = pruned;
				if *opcode == 0xfe {
					*gas_cost = 0;
					*gas_limit = 0;
				}
			}
			e
		})
		.collect())
}

pub fn get_repository_root() -> anyhow::Result<PathBuf> {
	let output = Command::new("git")
		.args(["rev-parse", "--show-toplevel"])
		.output()?;

	if !output.status.success() {
		return Err(anyhow::Error::msg(
			"Command `git rev-parse --show-toplevel` failed",
		));
	}

	let output = String::from_utf8(output.stdout)?;
	let path = PathBuf::try_from(output.trim())?;
	Ok(path)
}

struct ExitBehavior {
	set_remaining_gas_to_zero: bool,
	save_current_step: bool,
	subtract_cost: bool,
}

impl ExitBehavior {
	fn new(reason: &ExitReason) -> Self {
		// Certain errors require manual intervention in the tracing to match devm.
		// This manual intervention is needed only because spunik events may or may not
		// be emitted depending on where exactly the error happens.
		let (set_remaining_gas_to_zero, save_current_step, subtract_cost) = match reason {
			ExitReason::Error(ExitError::OutOfOffset)
			| ExitReason::Error(ExitError::InvalidCode(evm::Opcode::INVALID)) => (true, true, false),
			ExitReason::Error(ExitError::OutOfGas) => (true, false, false),
			ExitReason::Error(ExitError::StackUnderflow) => (false, true, true),
			_ => (false, true, false),
		};
		Self {
			set_remaining_gas_to_zero,
			save_current_step,
			subtract_cost,
		}
	}

	fn execute(self, listener: &mut EventListener) {
		if self.set_remaining_gas_to_zero {
			listener.intermediate_exit.gas = 0;
		}
		if self.subtract_cost {
			listener.current_step.gas_limit -= listener.current_step.gas_cost;
			listener.current_step.gas_cost = 0;
		}
		if self.save_current_step {
			listener.save_current_step();
		}
	}
}

#[test]
fn test_prune_memory() {
	// prune_memory strips tailing zeros, therefore an invariant of the function is
	// `prune_memory(bytes || (x + 1) || zeros) == bytes || (x + 1)`, where `||` is
	// concatenation, `bytes` is an arbitrary byte string, `x` is an arbitrary byte,
	// and `zeros` is an arbitrary slice of zeros.
	// This test checks that invariant.

	fn check_prune_memory(bytes: &[u8], x: u8, num_zeros: usize) {
		let prefix = [bytes, &[x.saturating_add(1)]].concat();
		let input = [prefix.as_slice(), &vec![0_u8; num_zeros]].concat();
		let result = EventListener::prune_memory(&input);
		assert_eq!(result, prefix, "prune_memory failed for input {input:?}");
	}

	// Check the cases of empty result separately
	assert_eq!(EventListener::prune_memory(&[]), Vec::<u8>::new());
	assert_eq!(EventListener::prune_memory(&[0]), Vec::<u8>::new());
	assert_eq!(EventListener::prune_memory(&[0, 0, 0]), Vec::<u8>::new());

	// Check invariant
	check_prune_memory(&[], 0, 0);
	check_prune_memory(&[], 0, 10);
	check_prune_memory(&[], 5, 0);
	check_prune_memory(&[], 5, 5);
	check_prune_memory(&[0, 0, 0], 7, 3);
	check_prune_memory(&[0, 0, 1], 7, 3);
	check_prune_memory(&[0, 1, 0], 7, 3);
	check_prune_memory(&[1, 0, 0], 7, 3);
}
