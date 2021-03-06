use std::collections::HashMap;
use std::io;
use std::io::{BufRead, Read, Write};
use std::str::FromStr;
use std::time::Duration;

use clap::ArgMatches;
use serde::{
	de::{self, DeserializeOwned},
	Deserialize, Deserializer, Serialize, Serializer,
};

use crate::modules::Command;

#[allow(dead_code)]
pub fn input_string(matches: &ArgMatches) -> Result<String, String> {
	match matches.value_of("INPUT") {
		Some(input) => Ok(input.to_string()),
		None => io::stdin()
			.lock()
			.lines()
			.collect::<Result<Vec<String>, io::Error>>()
			.map(|x| x.join("\n"))
			.map_err(|_| "Invalid input".to_string()),
	}
}

#[allow(dead_code)]
pub fn input_bytes(matches: &ArgMatches) -> Result<Vec<u8>, String> {
	match matches.value_of("INPUT") {
		Some(input) => Ok(input.bytes().collect::<Vec<u8>>()),
		None => io::stdin()
			.bytes()
			.collect::<Result<Vec<u8>, io::Error>>()
			.map_err(|_| "Invalid input".to_string()),
	}
}

pub fn output<T: Serialize>(t: T) -> Result<Vec<String>, String> {
	let output = serde_json::to_string_pretty(&Output {
		result: Some(t),
		error: None,
	})
	.map_err(|_| "Json encode failed")?;
	Ok(vec![output])
}

pub fn output_error(s: String) -> String {
	let output: Output<()> = Output {
		result: None,
		error: Some(Error {
			code: 1,
			message: s,
		}),
	};
	let output = serde_json::to_string_pretty(&output).expect("qed");
	output
}

pub fn get_rpc(matches: &ArgMatches) -> String {
	let rpc = matches.value_of("RPC").expect("qed");
	let rpc = match rpc.parse::<u32>() {
		Ok(port) => format!("http://localhost:{}", port),
		Err(_) => format!("{}", rpc),
	};
	rpc
}

#[derive(Serialize, Deserialize)]
pub struct Error {
	code: i32,
	message: String,
}

#[derive(Serialize, Deserialize)]
pub struct Output<T: Serialize> {
	#[serde(skip_serializing_if = "Option::is_none")]
	pub result: Option<T>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub error: Option<Error>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hex(Vec<u8>);

impl FromStr for Hex {
	type Err = String;
	fn from_str(s: &str) -> Result<Self, Self::Err> {
		let s = hex::decode(s.trim_start_matches("0x")).map_err(|_| "Invalid hex".to_string())?;
		Ok(Self(s))
	}
}

impl From<Vec<u8>> for Hex {
	fn from(f: Vec<u8>) -> Self {
		Self(f)
	}
}

impl Into<String> for Hex {
	fn into(self) -> String {
		format!("0x{}", hex::encode(self.0))
	}
}

impl Into<Vec<u8>> for Hex {
	fn into(self) -> Vec<u8> {
		self.0
	}
}

impl Serialize for Hex {
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		serializer.serialize_str(&format!("0x{}", hex::encode(&self.0)))
	}
}

impl<'de> Deserialize<'de> for Hex {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: Deserializer<'de>,
	{
		let str = String::deserialize(deserializer)?;
		let hex = Hex::from_str(&str).map_err(|e| de::Error::custom(e))?;
		Ok(hex)
	}
}

pub fn run<'a, 'b, 'a1, 'b1, GSC, GC>(
	matches: &ArgMatches<'a>,
	get_sub_commands: GSC,
	get_commands: GC,
) -> Result<Vec<String>, String>
where
	GSC: Fn() -> Vec<Command<'a, 'b>>,
	GC: Fn() -> Vec<Command<'a1, 'b1>>,
	'a: 'b,
	'a1: 'b1,
{
	let sub_commands = get_sub_commands();
	let map = sub_commands
		.iter()
		.map(|sub_command| (sub_command.app.get_name(), sub_command.f))
		.collect::<HashMap<_, _>>();

	let (name, matches) = matches.subcommand();

	let f = map.get(name);
	match (f, matches) {
		(Some(f), Some(matches)) => f(matches),
		_ => {
			get_commands()[0].app.print_help().unwrap_or(());
			println!();
			Ok(vec![])
		}
	}
}

pub async fn rpc_call<P: Serialize, R: DeserializeOwned>(
	rpc: &str,
	method: &str,
	params: &P,
) -> Result<RpcResponse<R>, String> {
	let request = RpcRequest {
		jsonrpc: "2.0",
		method,
		params,
		id: 1,
	};

	let client = reqwest::ClientBuilder::new()
		.connect_timeout(Duration::from_secs(3))
		.timeout(Duration::from_secs(5))
		.build()
		.map_err(|_e| "Build client error")?;

	let res = client
		.post(rpc)
		.json(&request)
		.send()
		.await
		.map_err(|e| format!("Request failed: {:?}", e))?;
	let response: RpcResponse<R> = res
		.json()
		.await
		.map_err(|e| format!("Response failed: {:?}", e))?;

	Ok(response)
}

#[derive(Serialize)]
pub struct RpcRequest<'a, 'b, P> {
	pub jsonrpc: &'static str,
	pub method: &'a str,
	pub params: &'b P,
	pub id: i32,
}

#[derive(Debug, Deserialize)]
pub struct RpcResponse<T> {
	pub jsonrpc: String,
	pub result: Option<T>,
	pub error: Option<RpcError>,
	pub id: i32,
}

#[derive(Debug, Deserialize)]
pub struct RpcError {
	pub code: i32,
	pub message: String,
}

pub fn put_to_file(content: &[u8], file_path: &str) -> Result<(), String> {
	let mut file = std::fs::File::create(file_path).map_err(|_| "File creation failed")?;
	file.write_all(content).map_err(|_| "Write failed")?;
	Ok(())
}

pub fn get_from_file(file_path: &str) -> Result<Vec<u8>, String> {
	let mut file =
		std::fs::File::open(file_path).map_err(|e| format!("Open file failed: {:?}", e))?;
	let mut content = vec![];
	file.read_to_end(&mut content)
		.map_err(|_| "Read file failed")?;
	Ok(content)
}

#[cfg(test)]
pub mod test {
	use crate::modules::Module;

	pub fn test_module(module: Module) {
		let commands = module.commands;
		let cases = (module.get_cases)();
		for command in commands {
			let app = &command.app;
			let cases = cases.get(app.get_name());

			if let Some(cases) = cases {
				assert!(cases.len() > 0, "{} should have cases", app.get_name());

				let f = &command.f.clone();
				for case in cases {
					if case.is_test {
						let mut ori_input = case
							.input
							.clone()
							.into_iter()
							.map(|x| {
								let x = x.trim_start_matches("'");
								let x = x.trim_end_matches("'");
								x.to_string()
							})
							.collect();
						let mut input = vec![app.get_name().to_string()];
						input.append(&mut ori_input);
						let expected_output = Ok((&case.output).clone());
						let matches = app.clone().get_matches_from(input.clone());
						let output = f(&matches);
						assert_eq!(output, expected_output, "Test: {}", input.join(" "));
					}
				}
			}
		}
	}
}
