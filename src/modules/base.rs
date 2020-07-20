use core::num::NonZeroU32;
use std::collections::HashMap;
use std::io;
use std::io::{BufRead, Read, Write};
use std::str::FromStr;

use clap::ArgMatches;
use crypto::aes::{ctr, KeySize};
use crypto::buffer::{RefReadBuffer, RefWriteBuffer};
use crypto::symmetriccipher::{Decryptor, Encryptor};
use ring::digest::SHA256;
use ring::pbkdf2::derive;
use serde::{de::DeserializeOwned, Deserialize, Serialize, Serializer};

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

#[derive(Serialize)]
struct Error {
	code: i32,
	message: String,
}

#[derive(Serialize)]
struct Output<T: Serialize> {
	#[serde(skip_serializing_if = "Option::is_none")]
	result: Option<T>,
	#[serde(skip_serializing_if = "Option::is_none")]
	error: Option<Error>,
}

#[derive(Debug)]
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

	let client = reqwest::Client::new();
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

const SALT: &'static [u8] = b"yee-utils-key";
const ITERATIONS: u32 = 1024;
const KEY_SIZE: KeySize = KeySize::KeySize256;
const KEYSTORE_VERSION: &'static str = "1.0";

#[derive(Serialize, Deserialize)]
struct KeyStore {
	pub version: String,
	pub secret_key: String,
}

pub fn put_key(secret_key: &[u8], password: &str, keystore_path: &str) -> Result<(), String> {
	let cipher = aes_enc(&secret_key, &password)?;

	let cipher = format!("0x{}", hex::encode(cipher));

	let keystore = KeyStore {
		version: KEYSTORE_VERSION.to_string(),
		secret_key: cipher,
	};

	let content = serde_json::to_string(&keystore).map_err(|_| "Keystore encode failed")?;

	put_to_file(content.as_bytes(), keystore_path)?;

	Ok(())
}

pub fn get_key(password: &str, keystore_path: &str) -> Result<Vec<u8>, String> {
	let content = get_from_file(keystore_path)?;

	let keystore: KeyStore =
		serde_json::from_slice(&content).map_err(|_| "Keystore decode failed")?;

	if keystore.version != KEYSTORE_VERSION {
		return Err("Invalid keystore version".to_string());
	}

	let cipher = keystore.secret_key;
	let cipher = cipher.trim_start_matches("0x");

	let cipher: Vec<u8> = hex::decode(cipher).map_err(|_| "Keystore decode failed")?;

	let secret_key = aes_dec(&cipher, password)?;

	Ok(secret_key)
}

pub fn put_to_file(content: &[u8], file_path: &str) -> Result<(), String> {
	let mut file = std::fs::File::create(file_path).map_err(|_| "File creation failed")?;
	file.write_all(content).map_err(|_| "Write failed")?;
	Ok(())
}

pub fn get_from_file(file_path: &str) -> Result<Vec<u8>, String> {
	let mut file = std::fs::File::open(file_path).map_err(|_| "Open file failed")?;
	let mut content = vec![];
	file.read_to_end(&mut content)
		.map_err(|_| "Read file failed")?;
	Ok(content)
}

pub fn aes_enc(plain: &[u8], password: &str) -> Result<Vec<u8>, String> {
	let (key, iv) = password_to_key(&password);

	let mut a = ctr(KEY_SIZE, &key, &iv);
	let mut result = vec![0u8; plain.len()];
	a.encrypt(
		&mut RefReadBuffer::new(&plain),
		&mut RefWriteBuffer::new(&mut result),
		true,
	)
	.map_err(|_| "Enc failed")?;

	Ok(result)
}

pub fn aes_dec(cipher: &[u8], password: &str) -> Result<Vec<u8>, String> {
	let (key, iv) = password_to_key(&password);
	let mut a = ctr(KEY_SIZE, &key, &iv);
	let mut result = vec![0u8; cipher.len()];
	let mut buffer = RefWriteBuffer::new(&mut result);
	a.decrypt(&mut RefReadBuffer::new(&cipher), &mut buffer, true)
		.map_err(|_| "Dec failed")?;

	Ok(result)
}

fn password_to_key(password: &str) -> ([u8; 32], [u8; 32]) {
	let secret = password.as_bytes();
	let iterations = NonZeroU32::new(ITERATIONS).expect("qed");
	let mut result = [0u8; 64];
	derive(&SHA256, iterations, SALT, &secret, &mut result);

	let mut key = [0u8; 32];
	let mut iv = [0u8; 32];
	key.copy_from_slice(&result[0..32]);
	iv.copy_from_slice(&result[32..]);

	(key, iv)
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
