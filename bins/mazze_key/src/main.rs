// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

// Copyright 2015-2019 Parity Technologies (UK) Ltd.
// This file is part of Parity Ethereum.

// Parity Ethereum is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Parity Ethereum is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Parity Ethereum.  If not, see <http://www.gnu.org/licenses/>.

extern crate docopt;
extern crate env_logger;
extern crate mazze_addr;
extern crate mazzekey;
extern crate panic_hook;
extern crate parity_wordlist;
extern crate rustc_hex;
extern crate serde;
extern crate threadpool;

#[macro_use]
extern crate serde_derive;

use std::{env, fmt, io, num::ParseIntError, process, sync};

use docopt::Docopt;
use mazze_addr::{mazze_addr_encode, EncodingOptions, Network};
use mazzekey::{
    brain_recover, sign, verify_address, verify_public, Brain, BrainPrefix,
    Error as EthkeyError, Generator, KeyPair, Prefix, Random,
};
use rustc_hex::{FromHex, FromHexError};

const USAGE: &str = r#"
Mazze keys generator.

Usage:
    mazzekey info <secret-or-phrase> [options]
    mazzekey generate random [options]
    mazzekey generate prefix <prefix> [options]
    mazzekey sign <secret> <message>
    mazzekey verify public <public> <signature> <message>
    mazzekey verify address <address> <signature> <message>
    mazzekey recover <address> <known-phrase>
    mazzekey [-h | --help]

Options:
    -h, --help         Display this message and exit.
    -s, --secret       Display only the secret key.
    -p, --public       Display only the public key.
    -a, --address      Display only the address.
    -b, --brain        Use parity brain wallet algorithm. Not recommended.
    -n, --network <network-id>  Display base32 formatted address with network prefix.

Commands:
    info               Display public key and address of the secret.
    generate random    Generates new random Ethereum key.
    generate prefix    Random generation, but address must start with a prefix ("vanity address").
    sign               Sign message using a secret key.
    verify             Verify signer of the signature by public key or address.
    recover            Try to find brain phrase matching given address from partial phrase.
"#;

#[derive(Debug, Deserialize)]
struct Args {
    cmd_info: bool,
    cmd_generate: bool,
    cmd_random: bool,
    cmd_prefix: bool,
    cmd_sign: bool,
    cmd_verify: bool,
    cmd_public: bool,
    cmd_address: bool,
    cmd_recover: bool,
    arg_prefix: String,
    arg_secret: String,
    arg_secret_or_phrase: String,
    arg_known_phrase: String,
    arg_message: String,
    arg_public: String,
    arg_address: String,
    arg_signature: String,
    flag_secret: bool,
    flag_public: bool,
    flag_address: bool,
    flag_brain: bool,
    flag_network: Option<String>,
}

#[derive(Debug)]
enum Error {
    Ethkey(EthkeyError),
    FromHex(FromHexError),
    ParseInt(ParseIntError),
    Docopt(docopt::Error),
    Io(io::Error),
}

impl From<EthkeyError> for Error {
    fn from(err: EthkeyError) -> Self {
        Error::Ethkey(err)
    }
}

impl From<FromHexError> for Error {
    fn from(err: FromHexError) -> Self {
        Error::FromHex(err)
    }
}

impl From<ParseIntError> for Error {
    fn from(err: ParseIntError) -> Self {
        Error::ParseInt(err)
    }
}

impl From<docopt::Error> for Error {
    fn from(err: docopt::Error) -> Self {
        Error::Docopt(err)
    }
}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self {
        Error::Io(err)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        match *self {
            Error::Ethkey(ref e) => write!(f, "{}", e),
            Error::FromHex(ref e) => write!(f, "{}", e),
            Error::ParseInt(ref e) => write!(f, "{}", e),
            Error::Docopt(ref e) => write!(f, "{}", e),
            Error::Io(ref e) => write!(f, "{}", e),
        }
    }
}

enum DisplayMode {
    KeyPair,
    Secret,
    Public,
    Address,
}

impl DisplayMode {
    fn new(args: &Args) -> Self {
        if args.flag_secret {
            DisplayMode::Secret
        } else if args.flag_public {
            DisplayMode::Public
        } else if args.flag_address {
            DisplayMode::Address
        } else {
            DisplayMode::KeyPair
        }
    }
}

fn main() {
    panic_hook::set_abort();
    env_logger::try_init().expect("Logger initialized only once.");

    match execute(env::args()) {
        Ok(ok) => println!("{}", ok),
        Err(Error::Docopt(ref e)) => e.exit(),
        Err(err) => {
            eprintln!("{}", err);
            process::exit(1);
        }
    }
}

fn display(
    result: (KeyPair, Option<String>), mode: DisplayMode,
    network_id: Option<&str>,
) -> String {
    let keypair = result.0;
    let hex_address = format!("{:x}", keypair.address());
    let network = match network_id {
        Some("main") => Network::Main,
        Some("test") => Network::Test,
        Some(id) => Network::Id(id.parse().unwrap_or(0)),
        None => Network::Main,
    };
    let base32_address = mazze_addr_encode(
        &hex::decode(&hex_address).unwrap(),
        network,
        EncodingOptions::Simple,
    )
    .unwrap();

    match mode {
        DisplayMode::KeyPair => {
            let keypair_info = format!("{}", keypair);
            let address_info = format!(
                "Hex address: {}\nBase32 address: {}",
                hex_address, base32_address
            );
            match result.1 {
                Some(extra_data) => format!(
                    "{}\n{}\n{}",
                    extra_data, keypair_info, address_info
                ),
                None => format!("{}\n{}", keypair_info, address_info),
            }
        }
        DisplayMode::Secret => format!("{:x}", keypair.secret()),
        DisplayMode::Public => format!("{:x}", keypair.public()),
        DisplayMode::Address => {
            format!(
                "Hex address: {}\nBase32 address: {}",
                hex_address, base32_address
            )
        }
    }
}

fn execute<S, I>(command: I) -> Result<String, Error>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let args: Args =
        Docopt::new(USAGE).and_then(|d| d.argv(command).deserialize())?;

    if args.cmd_info {
        let display_mode = DisplayMode::new(&args);
        let network_id = args.flag_network.as_deref();

        let result = if args.flag_brain {
            let phrase = args.arg_secret_or_phrase;
            let phrase_info = validate_phrase(&phrase);
            let keypair = Brain::new(phrase)
                .generate()
                .expect("Brain wallet generator is infallible; qed");
            (keypair, Some(phrase_info))
        } else {
            let secret = args
                .arg_secret_or_phrase
                .parse()
                .map_err(|_| EthkeyError::InvalidSecret)?;
            (KeyPair::from_secret(secret)?, None)
        };
        Ok(display(result, display_mode, network_id))
    } else if args.cmd_generate {
        let display_mode = DisplayMode::new(&args);
        let network_id = args.flag_network.as_deref();

        let result = if args.cmd_random {
            if args.flag_brain {
                let mut brain =
                    BrainPrefix::new(vec![0], usize::max_value(), BRAIN_WORDS);
                let keypair = brain.generate()?;
                let phrase = format!("recovery phrase: {}", brain.phrase());
                (keypair, Some(phrase))
            } else {
                (Random.generate()?, None)
            }
        } else if args.cmd_prefix {
            let prefix: Vec<u8> = args.arg_prefix.from_hex()?;
            let brain = args.flag_brain;
            in_threads(move || {
                let iterations = 1024;
                let prefix = prefix.clone();
                move || {
                    let prefix = prefix.clone();
                    let res = if brain {
                        let mut brain =
                            BrainPrefix::new(prefix, iterations, BRAIN_WORDS);
                        let result = brain.generate();
                        let phrase =
                            format!("recovery phrase: {}", brain.phrase());
                        result.map(|keypair| (keypair, Some(phrase)))
                    } else {
                        let result = Prefix::new(prefix, iterations).generate();
                        result.map(|res| (res, None))
                    };

                    Ok(res.map(Some).unwrap_or(None))
                }
            })?
        } else {
            return Ok(USAGE.to_string());
        };
        Ok(display(result, display_mode, network_id))
    } else if args.cmd_sign {
        let secret = args
            .arg_secret
            .parse()
            .map_err(|_| EthkeyError::InvalidSecret)?;
        let message = args
            .arg_message
            .parse()
            .map_err(|_| EthkeyError::InvalidMessage)?;
        let signature = sign(&secret, &message)?;
        Ok(format!("{}", signature))
    } else if args.cmd_verify {
        let signature = args
            .arg_signature
            .parse()
            .map_err(|_| EthkeyError::InvalidSignature)?;
        let message = args
            .arg_message
            .parse()
            .map_err(|_| EthkeyError::InvalidMessage)?;
        let ok = if args.cmd_public {
            let public = args
                .arg_public
                .parse()
                .map_err(|_| EthkeyError::InvalidPublic)?;
            verify_public(&public, &signature, &message)?
        } else if args.cmd_address {
            let address = args
                .arg_address
                .parse()
                .map_err(|_| EthkeyError::InvalidAddress)?;
            verify_address(&address, &signature, &message)?
        } else {
            return Ok(USAGE.to_string());
        };
        Ok(format!("{}", ok))
    } else if args.cmd_recover {
        let display_mode = DisplayMode::new(&args);
        let network_id = args.flag_network.as_deref();

        let known_phrase = args.arg_known_phrase;
        let address = args
            .arg_address
            .parse()
            .map_err(|_| EthkeyError::InvalidAddress)?;
        let (phrase, keypair) = in_threads(move || {
            let mut it = brain_recover::PhrasesIterator::from_known_phrase(
                &known_phrase,
                BRAIN_WORDS,
            )
            .enumerate();
            move || {
                for (i, phrase) in &mut it {
                    let keypair =
                        Brain::new(phrase.clone()).generate().unwrap();
                    if keypair.address() == address {
                        return Ok(Some((phrase, keypair)));
                    }

                    if i >= 1024 {
                        return Ok(None);
                    }
                }

                Err(EthkeyError::Custom("Couldn't find any results.".into()))
            }
        })?;
        Ok(display((keypair, Some(phrase)), display_mode, network_id))
    } else {
        Ok(USAGE.to_string())
    }
}

const BRAIN_WORDS: usize = 12;

fn validate_phrase(phrase: &str) -> String {
    match Brain::validate_phrase(phrase, BRAIN_WORDS) {
        Ok(()) => "The recovery phrase looks correct.\n".to_string(),
        Err(err) => {
            format!("The recover phrase was not generated by Mazze: {}", err)
        }
    }
}

fn in_threads<F, X, O>(prepare: F) -> Result<O, EthkeyError>
where
    O: Send + 'static,
    X: Send + 'static,
    F: Fn() -> X,
    X: FnMut() -> Result<Option<O>, EthkeyError>,
{
    let pool = threadpool::Builder::new().build();

    let (tx, rx) = sync::mpsc::sync_channel(1);
    let is_done = sync::Arc::new(sync::atomic::AtomicBool::default());

    for _ in 0..pool.max_count() {
        let is_done = is_done.clone();
        let tx = tx.clone();
        let mut task = prepare();
        pool.execute(move || {
            loop {
                if is_done.load(sync::atomic::Ordering::SeqCst) {
                    return;
                }

                let res = match task() {
                    Ok(None) => continue,
                    Ok(Some(v)) => Ok(v),
                    Err(err) => Err(err),
                };

                // We are interested only in the first response.
                let _ = tx.send(res);
            }
        });
    }

    if let Ok(solution) = rx.recv() {
        is_done.store(true, sync::atomic::Ordering::SeqCst);
        return solution;
    }

    Err(EthkeyError::Custom("No results found.".into()))
}

#[cfg(test)]
mod tests {
    use super::execute;

    #[test]
    fn info() {
        let command = vec![
            "mazzekey",
            "info",
            "17d08f5fe8c77af811caa0c9a187e668ce3b74a99acc3f6d976f075fa8e0be55",
        ]
        .into_iter()
        .map(Into::into)
        .collect::<Vec<String>>();

        let expected =
            "secret:  17d08f5fe8c77af811caa0c9a187e668ce3b74a99acc3f6d976f075fa8e0be55
public:  689268c0ff57a20cd299fa60d3fb374862aff565b20b5f1767906a99e6e09f3ff04ca2b2a5cd22f62941db103c0356df1a8ed20ce322cab2483db67685afd124
address: 16d1ec50b4e62c1d1a40d16e7cacc6a6580757d5".to_owned();
        assert_eq!(execute(command).unwrap(), expected);
    }

    #[test]
    fn brain() {
        let command = vec!["mazzekey", "info", "--brain", "this is sparta"]
            .into_iter()
            .map(Into::into)
            .collect::<Vec<String>>();

        let expected =
            "The recover phrase was not generated by Mazze: The word 'this' does not come from the dictionary.

secret:  a6bb621db2721ee05c44de651dde50ef85feefc5e91ae23bedcae69b874a22e7
public:  756cb3f7ad1516b7c0ee34bd5e8b3a519922d3737192a58115e47df57ff3270873360a61de523ce08c0ebd7d3801bcb1d03c0364431d2b8633067f3d79e1fb25
address: 10a33d9f95b22fe53024331c036db6e824a25bab".to_owned();
        assert_eq!(execute(command).unwrap(), expected);
    }

    #[test]
    fn secret() {
        let command =
            vec!["mazzekey", "info", "--brain", "this is sparta", "--secret"]
                .into_iter()
                .map(Into::into)
                .collect::<Vec<String>>();

        let expected =
            "a6bb621db2721ee05c44de651dde50ef85feefc5e91ae23bedcae69b874a22e7"
                .to_owned();
        assert_eq!(execute(command).unwrap(), expected);
    }

    #[test]
    fn public() {
        let command =
            vec!["mazzekey", "info", "--brain", "this is sparta", "--public"]
                .into_iter()
                .map(Into::into)
                .collect::<Vec<String>>();

        let expected = "756cb3f7ad1516b7c0ee34bd5e8b3a519922d3737192a58115e47df57ff3270873360a61de523ce08c0ebd7d3801bcb1d03c0364431d2b8633067f3d79e1fb25".to_owned();
        assert_eq!(execute(command).unwrap(), expected);
    }

    #[test]
    fn address() {
        let command =
            vec!["mazzekey", "info", "-b", "this is sparta", "--address"]
                .into_iter()
                .map(Into::into)
                .collect::<Vec<String>>();

        let expected = "10a33d9f95b22fe53024331c036db6e824a25bab".to_owned();
        assert_eq!(execute(command).unwrap(), expected);
    }

    #[test]
    fn sign() {
        let command = vec![
            "mazzekey",
            "sign",
            "17d08f5fe8c77af811caa0c9a187e668ce3b74a99acc3f6d976f075fa8e0be55",
            "bd50b7370c3f96733b31744c6c45079e7ae6c8d299613246d28ebcef507ec987",
        ]
        .into_iter()
        .map(Into::into)
        .collect::<Vec<String>>();

        let expected = "c1878cf60417151c766a712653d26ef350c8c75393458b7a9be715f053215af63dfd3b02c2ae65a8677917a8efa3172acb71cb90196e42106953ea0363c5aaf200".to_owned();
        assert_eq!(execute(command).unwrap(), expected);
    }

    #[test]
    fn verify_valid_public() {
        let command = vec!["mazzekey", "verify", "public", "689268c0ff57a20cd299fa60d3fb374862aff565b20b5f1767906a99e6e09f3ff04ca2b2a5cd22f62941db103c0356df1a8ed20ce322cab2483db67685afd124", "c1878cf60417151c766a712653d26ef350c8c75393458b7a9be715f053215af63dfd3b02c2ae65a8677917a8efa3172acb71cb90196e42106953ea0363c5aaf200", "bd50b7370c3f96733b31744c6c45079e7ae6c8d299613246d28ebcef507ec987"]
            .into_iter()
            .map(Into::into)
            .collect::<Vec<String>>();

        let expected = "true".to_owned();
        assert_eq!(execute(command).unwrap(), expected);
    }

    #[test]
    fn verify_valid_address() {
        let command = vec!["mazzekey", "verify", "address", "16d1ec50b4e62c1d1a40d16e7cacc6a6580757d5", "c1878cf60417151c766a712653d26ef350c8c75393458b7a9be715f053215af63dfd3b02c2ae65a8677917a8efa3172acb71cb90196e42106953ea0363c5aaf200", "bd50b7370c3f96733b31744c6c45079e7ae6c8d299613246d28ebcef507ec987"]
            .into_iter()
            .map(Into::into)
            .collect::<Vec<String>>();

        let expected = "true".to_owned();
        assert_eq!(execute(command).unwrap(), expected);
    }

    #[test]
    fn verify_invalid() {
        let command = vec!["mazzekey", "verify", "public", "689268c0ff57a20cd299fa60d3fb374862aff565b20b5f1767906a99e6e09f3ff04ca2b2a5cd22f62941db103c0356df1a8ed20ce322cab2483db67685afd124", "c1878cf60417151c766a712653d26ef350c8c75393458b7a9be715f053215af63dfd3b02c2ae65a8677917a8efa3172acb71cb90196e42106953ea0363c5aaf200", "bd50b7370c3f96733b31744c6c45079e7ae6c8d299613246d28ebcef507ec986"]
            .into_iter()
            .map(Into::into)
            .collect::<Vec<String>>();

        let expected = "false".to_owned();
        assert_eq!(execute(command).unwrap(), expected);
    }
}