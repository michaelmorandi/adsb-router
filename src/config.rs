//! Configuration handling for the ADS-B router service
//!
//! Supports configuration via command-line arguments and TOML config file.

use clap::Parser;
use serde::Deserialize;
use std::net::SocketAddr;
use std::path::PathBuf;

/// ADS-B Plane Tracking Service
///
/// Connects to multiple dump1090-fa instances, consolidates aircraft data,
/// and exposes it via gRPC.
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// Path to TOML configuration file
    #[arg(short, long)]
    pub config: Option<PathBuf>,

    /// ADS-B sources to connect to (format: host:port or host for default port 30005)
    /// Can be specified multiple times
    #[arg(short, long = "source")]
    pub sources: Vec<String>,

    /// gRPC server listen address
    #[arg(short, long, default_value = "0.0.0.0:50051")]
    pub grpc_addr: SocketAddr,

    /// Log level (trace, debug, info, warn, error)
    #[arg(short, long, default_value = "info")]
    pub log_level: String,
}

/// TOML configuration file structure
#[derive(Debug, Deserialize, Default)]
pub struct ConfigFile {
    /// gRPC server configuration
    #[serde(default)]
    pub grpc: GrpcConfig,

    /// ADS-B source configuration
    #[serde(default)]
    pub sources: Vec<SourceConfig>,

    /// Logging configuration
    #[serde(default)]
    pub logging: LoggingConfig,
}

#[derive(Debug, Deserialize)]
pub struct GrpcConfig {
    /// Listen address for gRPC server
    #[serde(default = "default_grpc_addr")]
    pub listen_addr: SocketAddr,

    /// Minimum interval between updates per aircraft in milliseconds (default: 500ms)
    /// Updates for the same aircraft will be throttled to this rate in StreamUpdates
    /// Set to 0 to disable throttling and receive all updates immediately
    #[serde(default = "default_update_interval_ms")]
    pub update_interval_ms: u64,
}

impl Default for GrpcConfig {
    fn default() -> Self {
        Self {
            listen_addr: default_grpc_addr(),
            update_interval_ms: default_update_interval_ms(),
        }
    }
}

fn default_grpc_addr() -> SocketAddr {
    "0.0.0.0:50051".parse().unwrap()
}

fn default_update_interval_ms() -> u64 {
    500
}

#[derive(Debug, Deserialize)]
pub struct SourceConfig {
    /// Hostname or IP address
    pub host: String,

    /// Port number (default: 30005)
    #[serde(default = "default_port")]
    pub port: u16,

    /// Optional name for this source
    pub name: Option<String>,
}

fn default_port() -> u16 {
    30005
}

#[derive(Debug, Deserialize)]
pub struct LoggingConfig {
    /// Log level
    #[serde(default = "default_log_level")]
    pub level: String,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
        }
    }
}

fn default_log_level() -> String {
    "info".to_string()
}

/// Combined configuration from CLI args and config file
#[derive(Debug)]
pub struct Config {
    pub grpc_addr: SocketAddr,
    pub sources: Vec<SocketAddr>,
    pub log_level: String,
    pub update_interval_ms: u64,
}

impl Config {
    /// Load configuration from CLI args and optional config file
    pub fn load() -> anyhow::Result<Self> {
        let args = Args::parse();

        // Try to load config file
        let file_config = if let Some(config_path) = &args.config {
            let content = std::fs::read_to_string(config_path)?;
            toml::from_str(&content)?
        } else {
            ConfigFile::default()
        };

        // Merge sources from CLI and config file
        let mut sources: Vec<SocketAddr> = Vec::new();

        // Add sources from CLI
        for source in &args.sources {
            sources.push(parse_source_addr(source)?);
        }

        // Add sources from config file
        for source in &file_config.sources {
            let addr: SocketAddr = format!("{}:{}", source.host, source.port).parse()?;
            if !sources.contains(&addr) {
                sources.push(addr);
            }
        }

        // CLI overrides file for grpc_addr if specified
        let grpc_addr = if args.sources.is_empty() && args.config.is_some() {
            file_config.grpc.listen_addr
        } else {
            args.grpc_addr
        };

        // CLI overrides file for log_level
        let log_level = if args.log_level != "info" {
            args.log_level
        } else {
            file_config.logging.level
        };

        Ok(Config {
            grpc_addr,
            sources,
            log_level,
            update_interval_ms: file_config.grpc.update_interval_ms,
        })
    }
}

/// Parse a source address string (host:port or just host)
fn parse_source_addr(s: &str) -> anyhow::Result<SocketAddr> {
    if s.contains(':') {
        Ok(s.parse()?)
    } else {
        Ok(format!("{}:30005", s).parse()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_source_addr_with_port() {
        let addr = parse_source_addr("192.168.1.100:30005").unwrap();
        assert_eq!(addr.port(), 30005);
    }

    #[test]
    fn test_parse_source_addr_without_port() {
        let addr = parse_source_addr("192.168.1.100").unwrap();
        assert_eq!(addr.port(), 30005);
    }
}
