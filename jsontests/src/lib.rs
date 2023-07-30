mod utils;

pub mod state;
pub mod vm;

use ethereum::Log;
use evm_runtime::{ExitReason, ExitError};
use primitive_types::{H160, H256};
use serde::Deserialize;
use std::process::Command;

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
      match step { // Show human readable opcode
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

  fn copy_static_cafe_values(steps: &mut Vec<Event>, events: &Vec<Event>) {
    // We copy the stack gas value on the next step after 0x5a opcode
    let cafe = H256::from_low_u64_be(0xcafe);
    if steps.len() == events.len() {
      // let mut found = false;
      for s in steps.iter_mut().zip(events) {
        if let Event::Step { stack, .. } = s.0 {
          if let Event::Step { stack: el_stack, .. } = s.1 {
            if stack.len() == el_stack.len() {
              for st in stack.iter_mut().zip(el_stack) {
                if st.0 == &cafe {
                  *st.0 = *st.1; // Copy stack value from events
                }
              }
            }
          }
        }
        if let Event::SLoad { value, .. } = s.0 {
          if let Event::SLoad { value: el_value, .. } = s.1 {
            if value == &cafe {
              *value = *el_value;
            }
          }
        }
        if let Event::SStore { value, .. } = s.0 {
          if let Event::SStore { value: el_value, .. } = s.1 {
            if value == &cafe {
              *value = *el_value;
            }
          }
        }
      }
    }
  }
}

pub struct EventListener {
  pub events: Vec<Event>,
}

impl evm::tracing::EventListener for EventListener {
  fn event(&mut self, event: evm::tracing::Event<'_>) {
    match event {
      evt @ _ => println!("Evm Event: {:?}", evt),
    };
  }
}

impl evm::gasometer::tracing::EventListener for EventListener {
  fn event(&mut self, event: evm::gasometer::tracing::Event) {
    match event {
      evt @ _ => println!("{:?}", evt),
    };
  }
}

impl evm_runtime::tracing::EventListener for EventListener {
  fn event(&mut self, event: evm_runtime::tracing::Event<'_>) {
    use evm_runtime::tracing::Event as RuntimeEvent;
    match event {
      RuntimeEvent::Step { context, opcode, position, stack, memory } =>
        self.events.push(Event::Step {
          sender: context.caller.clone(),
          contract: context.address.clone(),
          position: position.clone().unwrap(),
          opcode: opcode.0,
          stack: stack.data().clone(),
          memory: memory.data().clone(),
        }),
        RuntimeEvent::SLoad { address, index, value } =>
				self.events.push(Event::SLoad {
					address: address.clone(),
					index: index.clone(),
					value: value.clone(),
				}),
        RuntimeEvent::SStore { address, index, value } =>
				self.events.push(Event::SStore {
					address: address.clone(),
					index: index.clone(),
					value: value.clone(),
				}),
      _ => (),
    };
  }
}

pub fn exit_reason_to_u8(exit_reason: &ExitReason) -> u8 {
  match exit_reason {
    evm_runtime::ExitReason::Succeed(s) => s.clone() as u8,
    evm_runtime::ExitReason::Revert(_) => 0x10,
    evm_runtime::ExitReason::Fatal(_) => 0x20,
    evm_runtime::ExitReason::Error(e) => {
      match e {
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
      }
    },
  }
}

pub fn run_move_test() -> anyhow::Result<Vec<Event>> {
	// aptos move test --bytecode-version 6 -i 10000000 -f steps
	let command_output = Command::new("aptos")
		.current_dir("/Users/bulent/Desktop/Blockchain/EVM/devm")
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

	let output = regex::Regex::new(r"INCLUDING.*").unwrap().replace_all(&output, "");
	let output = regex::Regex::new(r"BUILDING.*").unwrap().replace(&output, "");
	let output = regex::Regex::new(r"Running.*").unwrap().replace(&output, "");
	let output = regex::Regex::new(r"\[ PASS.*").unwrap().replace(&output, "");
	let output = regex::Regex::new(r"Test.*").unwrap().replace_all(&output, "");
	let output = regex::Regex::new(r"\{.*").unwrap().replace_all(&output, "");
	let output = regex::Regex::new(r"\}.*").unwrap().replace_all(&output, "");
	let output = regex::Regex::new(r".*Result.*").unwrap().replace(&output, "");

  let output = regex::Regex::new(r"\[ FAIL.*").unwrap().replace(&output, "");
  let output = regex::Regex::new(r"Failures.*").unwrap().replace(&output, "");
  let output = regex::Regex::new(r"┌.*").unwrap().replace(&output, "");
  let output = regex::Regex::new(r"│.*").unwrap().replace_all(&output, "");
  let output = regex::Regex::new(r"└.*").unwrap().replace(&output, "");
  let output = regex::Regex::new(r".*Error.*").unwrap().replace(&output, "");
	// println!("{}", output);
	serde_yaml::from_str(&output).map_err(anyhow::Error::from)
}
