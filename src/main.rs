//! ADS-B Plane Tracking Service
//!
//! This service connects to multiple dump1090-fa instances to receive raw ADS-B messages,
//! builds an internal representation of aircraft with position, altitude, speed, heading,
//! and callsign, and exposes this data via gRPC.
//!
//! Features:
//! - Multi-source support with automatic reconnection
//! - Message deduplication across sources
//! - Automatic cleanup of stale aircraft (3 minute timeout)
//! - gRPC streaming API for real-time updates
//!
//! # Usage
//!
//! ```bash
//! # Connect to a single dump1090-fa instance
//! adsb-router --source 192.168.1.100:30005
//!
//! # Connect to multiple instances
//! adsb-router --source 192.168.1.100 --source 192.168.1.101
//!
//! # Use a config file
//! adsb-router --config config.toml
//! ```

mod adsb;
mod beast;
mod config;
mod connection;
mod grpc;
mod state;
mod synthetic;

use std::time::Duration;

use anyhow::Result;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use config::Config;
use connection::ConnectionManager;
use grpc::start_grpc_server;
use state::PlaneStateManager;

/// Cleanup interval for removing stale aircraft
const CLEANUP_INTERVAL: Duration = Duration::from_secs(30);

#[tokio::main]
async fn main() -> Result<()> {
    // Load configuration
    let config = Config::load()?;

    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&config.log_level)),
        )
        .init();

    info!("ADS-B Plane Tracking Service starting");

    if config.test_mode {
        info!("TEST MODE ENABLED - Generating synthetic aircraft data");
    } else {
        // Check for sources
        if config.sources.is_empty() {
            warn!("No ADS-B sources configured. Use --source or config file to add sources.");
            warn!("Example: adsb-router --source 192.168.1.100:30005");
        }
    }

    // Log ignored hex codes
    if !config.ignore_hex_codes.is_empty() {
        let hex_codes: Vec<String> = config
            .ignore_hex_codes
            .iter()
            .map(|icao| format!("{:06X}", icao))
            .collect();
        info!("Ignoring {} aircraft: {}", hex_codes.len(), hex_codes.join(", "));
    }

    // Create state manager
    let state_manager = PlaneStateManager::new(config.ignore_hex_codes);

    // Create connection manager
    let connection_manager = ConnectionManager::new(state_manager.clone());

    if config.test_mode {
        // Start synthetic data generator
        let synthetic_state = state_manager.clone();
        tokio::spawn(async move {
            synthetic::start_synthetic_generator(synthetic_state).await;
        });
    } else {
        // Connect to all configured sources
        for source in &config.sources {
            info!(address = %source, "Adding ADS-B source");
            connection_manager.add_source(*source).await;
        }
    }

    // Start cleanup task
    let cleanup_state = state_manager.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(CLEANUP_INTERVAL);
        loop {
            interval.tick().await;
            cleanup_state.cleanup_stale();
        }
    });

    // Start gRPC server
    info!(address = %config.grpc_addr, "gRPC server listening");
    if config.update_interval_ms == 0 {
        info!("Update throttling disabled - all updates will be sent immediately");
    } else {
        info!(interval_ms = config.update_interval_ms, "Update throttling enabled");
    }

    start_grpc_server(
        config.grpc_addr,
        config.update_interval_ms,
        state_manager,
        connection_manager,
    )
    .await?;

    Ok(())
}
