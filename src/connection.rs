//! Multi-source connection manager
//!
//! Manages TCP connections to multiple dump1090-fa instances (Beast binary port 30005)
//! with automatic reconnection.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use crate::adsb::decode_beast_frame;
use crate::beast::BeastDecoder;
use crate::state::PlaneStateManager;

/// Default Beast binary port for dump1090-fa
pub const DEFAULT_BEAST_PORT: u16 = 30005;

/// Reconnect delay after connection failure
const RECONNECT_DELAY: Duration = Duration::from_secs(5);

/// Read buffer size
const BUFFER_SIZE: usize = 8192;

/// Status of a single source connection
#[derive(Debug, Clone)]
pub struct SourceStatus {
    /// Address of the source
    pub address: SocketAddr,
    /// Whether currently connected
    pub connected: bool,
    /// Total bytes received
    pub bytes_received: u64,
    /// Total messages decoded
    pub messages_decoded: u64,
}

/// Connection to a single dump1090-fa instance
struct SourceConnection {
    address: SocketAddr,
    connected: AtomicBool,
    bytes_received: AtomicU64,
    messages_decoded: AtomicU64,
    shutdown: AtomicBool,
}

impl SourceConnection {
    fn new(address: SocketAddr) -> Arc<Self> {
        Arc::new(Self {
            address,
            connected: AtomicBool::new(false),
            bytes_received: AtomicU64::new(0),
            messages_decoded: AtomicU64::new(0),
            shutdown: AtomicBool::new(false),
        })
    }

    fn status(&self) -> SourceStatus {
        SourceStatus {
            address: self.address,
            connected: self.connected.load(Ordering::Relaxed),
            bytes_received: self.bytes_received.load(Ordering::Relaxed),
            messages_decoded: self.messages_decoded.load(Ordering::Relaxed),
        }
    }
}

/// Manages connections to multiple ADS-B data sources
pub struct ConnectionManager {
    sources: RwLock<Vec<Arc<SourceConnection>>>,
    state_manager: Arc<PlaneStateManager>,
}

impl ConnectionManager {
    /// Create a new connection manager
    pub fn new(state_manager: Arc<PlaneStateManager>) -> Arc<Self> {
        Arc::new(Self {
            sources: RwLock::new(Vec::new()),
            state_manager,
        })
    }

    /// Add a new source and start connecting
    pub async fn add_source(self: &Arc<Self>, address: SocketAddr) {
        let source = SourceConnection::new(address);
        self.sources.write().await.push(source.clone());

        let manager = self.clone();
        tokio::spawn(async move {
            manager.run_source(source).await;
        });
    }

    /// Get status of all sources
    pub async fn get_status(&self) -> Vec<SourceStatus> {
        self.sources
            .read()
            .await
            .iter()
            .map(|s| s.status())
            .collect()
    }

    /// Get count of connected sources
    pub async fn connected_count(&self) -> usize {
        self.sources
            .read()
            .await
            .iter()
            .filter(|s| s.connected.load(Ordering::Relaxed))
            .count()
    }

    /// Shutdown all connections
    pub async fn shutdown(&self) {
        for source in self.sources.read().await.iter() {
            source.shutdown.store(true, Ordering::Relaxed);
        }
    }

    /// Run connection loop for a single source
    async fn run_source(self: &Arc<Self>, source: Arc<SourceConnection>) {
        info!(address = %source.address, "Starting connection to ADS-B source");

        loop {
            if source.shutdown.load(Ordering::Relaxed) {
                info!(address = %source.address, "Shutting down source connection");
                break;
            }

            match self.connect_and_read(&source).await {
                Ok(()) => {
                    // Clean disconnect
                    warn!(address = %source.address, "Connection closed cleanly");
                }
                Err(e) => {
                    error!(address = %source.address, error = %e, "Connection error");
                }
            }

            source.connected.store(false, Ordering::Relaxed);

            if !source.shutdown.load(Ordering::Relaxed) {
                debug!(
                    address = %source.address,
                    delay_secs = RECONNECT_DELAY.as_secs(),
                    "Reconnecting after delay"
                );
                tokio::time::sleep(RECONNECT_DELAY).await;
            }
        }
    }

    /// Connect to source and read data until disconnection
    async fn connect_and_read(&self, source: &SourceConnection) -> Result<(), std::io::Error> {
        let mut stream = TcpStream::connect(source.address).await?;
        source.connected.store(true, Ordering::Relaxed);

        info!(address = %source.address, "Connected to ADS-B source");

        let mut decoder = BeastDecoder::new();
        let mut buffer = vec![0u8; BUFFER_SIZE];

        loop {
            if source.shutdown.load(Ordering::Relaxed) {
                break;
            }

            let n = stream.read(&mut buffer).await?;
            if n == 0 {
                // Connection closed
                break;
            }

            source.bytes_received.fetch_add(n as u64, Ordering::Relaxed);

            // Decode Beast frames
            let frames = decoder.decode(&buffer[..n]);

            for frame in frames {
                // Try to decode as ADS-B
                if let Some(update) = decode_beast_frame(&frame) {
                    source.messages_decoded.fetch_add(1, Ordering::Relaxed);
                    self.state_manager.process_update(update);
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_connection_manager_creation() {
        let state_manager = PlaneStateManager::new();
        let conn_manager = ConnectionManager::new(state_manager);

        let status = conn_manager.get_status().await;
        assert!(status.is_empty());
    }
}
