#![allow(clippy::use_self)]

extern crate clap;
extern crate env_logger;
extern crate ruma;

use clap::{App, AppSettings, Arg, SubCommand};

use ruma::config::Config;
use ruma::crypto::generate_macaroon_secret_key;
use ruma::server::Server;

fn main() {
    if let Err(error) = env_logger::try_init() {
        eprintln!("Failed to initialize logger: {}", error);
    }

    let matches = App::new("ruma-client-server")
        .version(env!("CARGO_PKG_VERSION"))
        .about("Client-server APIs for Ruma.")
        .setting(AppSettings::GlobalVersion)
        .setting(AppSettings::SubcommandRequiredElseHelp)
        .subcommand(
            SubCommand::with_name("run").about("Runs the server").arg(
                Arg::with_name("config")
                    .short("c")
                    .long("config")
                    .value_name("PATH")
                    .help("Path to a configuration file")
                    .takes_value(true),
            ),
        )
        .subcommand(
            SubCommand::with_name("secret")
                .about("Generates a random value to be used as a macaroon secret key"),
        )
        .get_matches();

    match matches.subcommand() {
        ("run", Some(submatches)) => {
            let config = match Config::from_file(submatches.value_of("config")) {
                Ok(config) => config,
                Err(error) => {
                    eprintln!("Failed to load configuration file: {}", error);

                    return;
                }
            };

            match Server::new(&config).mount_client() {
                Ok(server) => {
                    if let Err(error) = server.run() {
                        eprintln!("Server failed: {}", error);
                    }
                }
                Err(error) => {
                    eprintln!("Failed to create server: {}", error);
                }
            }
        }
        ("secret", Some(_)) => match generate_macaroon_secret_key() {
            Ok(key) => println!("{}", key),
            Err(error) => println!("Failed to generate macaroon secret key: {}", error),
        },
        _ => println!("{}", matches.usage()),
    };
}
