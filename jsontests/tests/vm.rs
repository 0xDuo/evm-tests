use evm_jsontests::vm as vmtests;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::BufReader;
use std::path::PathBuf;

pub fn run(dir: &str) {
	let _ = env_logger::try_init();

	let mut dest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
	dest.push(dir);

	let repository_root = evm_jsontests::get_repository_root().unwrap();
	// Assumes `devm` is located in the folder next to this repository root
	let devm_path = repository_root
		.parent()
		.unwrap_or(&repository_root)
		.join("devm");

	let mut entries = fs::read_dir(dest)
		.unwrap()
		.map(|res| res.map(|e| e.path()))
		.filter(|p| {
			!p.as_ref()
				.unwrap()
				.file_name()
				.unwrap()
				.to_str()
				.unwrap()
				.starts_with('.')
		})
		.collect::<Result<Vec<_>, std::io::Error>>()
		.unwrap();
	entries.sort();

	for path in entries {
		let file = File::open(path).expect("Open file failed");
		let reader = BufReader::new(file);
		let coll = serde_json::from_reader::<_, HashMap<String, vmtests::Test>>(reader)
			.expect("Parse test cases failed");

		for (name, test) in coll {
			vmtests::test(&name, test, &devm_path);
		}
	}
}

// TODO: upgrade to GeneralStateTests/VMTests instead of using LegacyTests version
#[test]
fn vm_arithmetic() {
	run("res/ethtests/LegacyTests/Constantinople/VMTests/vmArithmeticTest");
}
#[test]
fn vm_bitwise_logic() {
	run("res/ethtests/LegacyTests/Constantinople/VMTests/vmBitwiseLogicOperation");
}
#[test]
fn vm_block_info() {
	run("res/ethtests/LegacyTests/Constantinople/VMTests/vmBlockInfoTest");
}
#[test]
fn vm_environmental_info() {
	run("res/ethtests/LegacyTests/Constantinople/VMTests/vmEnvironmentalInfo");
}
#[test]
fn vm_io_and_flow() {
	run("res/ethtests/LegacyTests/Constantinople/VMTests/vmIOandFlowOperations");
}
#[test]
fn vm_log() {
	run("res/ethtests/LegacyTests/Constantinople/VMTests/vmLogTest");
}
#[test]
#[ignore]
fn vm_performance() {
	run("res/ethtests/LegacyTests/Constantinople/VMTests/vmPerformance");
}
#[test]
fn vm_push_dup_swap() {
	run("res/ethtests/LegacyTests/Constantinople/VMTests/vmPushDupSwapTest");
}
#[test]
fn vm_random() {
	run("res/ethtests/LegacyTests/Constantinople/VMTests/vmRandomTest");
}
#[test]
fn vm_sha3() {
	run("res/ethtests/LegacyTests/Constantinople/VMTests/vmSha3Test");
}
#[test]
fn vm_system() {
	run("res/ethtests/LegacyTests/Constantinople/VMTests/vmSystemOperations");
}
#[test]
fn vm_other() {
	run("res/ethtests/LegacyTests/Constantinople/VMTests/vmTests");
}
