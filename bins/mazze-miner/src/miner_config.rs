use clap::Parser;
use std::fs;
use std::path::PathBuf;
use toml;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct CliArgs {
    #[clap(long, value_parser)]
    config: PathBuf,

    #[clap(long)]
    stratum_address: Option<String>,

    #[clap(long, default_value = "4")]
    num_threads: usize,

    #[clap(long, default_value = "1")]
    worker_id: usize,
}

#[derive(Debug)]
pub struct MinerConfig {
    pub stratum_address: String,
    pub stratum_secret: String,
    pub num_threads: usize,
    pub worker_id: usize,
}

impl MinerConfig {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        // Parse command line arguments
        let cli_args = CliArgs::parse();

        // Read config file
        let config_content = fs::read_to_string(&cli_args.config)?;
        let config_toml: toml::Value = toml::from_str(&config_content)?;

        // Read stratum_secret from the config file
        let stratum_secret = config_toml["stratum_secret"]
            .as_str()
            .ok_or("stratum_secret not found in config file")?
            .to_string();

        // Read stratum_address from CLI args or config file
        let stratum_address = cli_args.stratum_address.unwrap_or_else(|| {
            let listen_address = config_toml["stratum_listen_address"]
                .as_str()
                .expect("stratum_address not found in config file and not provided as CLI argument")
                .to_string();
            let port = config_toml["stratum_port"]
                .as_integer()
                .expect("stratum_port not found in config file and not provided as CLI argument");
            format!("{}:{}", listen_address, port)
        });

        Ok(MinerConfig {
            stratum_address,
            stratum_secret,
            num_threads: cli_args.num_threads,
            worker_id: cli_args.worker_id,
        })
    }
}
