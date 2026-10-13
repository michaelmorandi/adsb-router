use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use anyhow::Result;
use tokio::sync::mpsc;
use tonic::transport::Channel;

/// Max samples retained per source.
/// Sized to hold up to 5 h of 1-min history + 5 min of 1-sec live data.
pub const HISTORY_LEN: usize = 600;

pub mod proto {
    tonic::include_proto!("adsb");
}

use proto::plane_tracking_service_client::PlaneTrackingServiceClient;
use proto::{MetricsSnapshot, StreamMetricsRequest};

// ── Messages flowing from the background gRPC task to the app ──────────────

#[derive(Debug, Clone)]
pub enum AppMessage {
    Snapshot(MetricsSnapshot),
    ConnectionError(String),
    Connected,
}

// ── Connection status displayed in the header ──────────────────────────────

pub enum ConnectionStatus {
    Connecting,
    Connected,
    Error(String),
}

// ── Per-source rolling history ─────────────────────────────────────────────

/// One data point in the fps timeseries
#[derive(Clone, Copy)]
pub struct Sample {
    /// Unix time of the sample (seconds, f64 for the Chart widget)
    pub time_secs: f64,
    pub fps: f64,
}

pub struct SourceHistory {
    pub address: String,
    /// Human-readable name from config, empty if not set
    pub name: String,
    pub connected: bool,
    pub frames_total: u64,
    pub current_fps: f64,
    /// Rolling window of HISTORY_LEN samples
    pub samples: VecDeque<Sample>,
}

impl SourceHistory {
    fn new(address: String, name: String) -> Self {
        Self {
            address,
            name,
            connected: false,
            frames_total: 0,
            current_fps: 0.0,
            samples: VecDeque::with_capacity(HISTORY_LEN + 1),
        }
    }

    /// Display label: name if set, otherwise address
    pub fn label(&self) -> &str {
        if self.name.is_empty() { &self.address } else { &self.name }
    }

    fn push_sample(&mut self, time_secs: f64, fps: f64) {
        if self.samples.len() >= HISTORY_LEN {
            self.samples.pop_front();
        }
        self.samples.push_back(Sample { time_secs, fps });
    }
}

// ── Main application state ─────────────────────────────────────────────────

pub struct App {
    pub addr: String,
    /// Sources in the order they were first seen
    pub source_order: Vec<String>,
    pub sources: HashMap<String, SourceHistory>,
    pub emergencies: Vec<proto::EmergencyEvent>,
    pub total_frames: u64,
    pub plane_count: u32,
    pub connection_status: ConnectionStatus,
    /// Router uptime in seconds (from the snapshot's uptime_seconds field)
    pub uptime_secs: u64,
}

impl App {
    pub fn new(addr: String) -> Self {
        Self {
            addr,
            source_order: Vec::new(),
            sources: HashMap::new(),
            emergencies: Vec::new(),
            total_frames: 0,
            plane_count: 0,
            connection_status: ConnectionStatus::Connecting,
            uptime_secs: 0,
        }
    }

    pub fn start_grpc_client(&self, tx: mpsc::Sender<AppMessage>) {
        let addr = self.addr.clone();
        tokio::spawn(async move {
            grpc_task(addr, tx).await;
        });
    }

    pub fn handle_message(&mut self, msg: AppMessage) {
        match msg {
            AppMessage::Snapshot(snap) => {
                let time_secs = snap.timestamp_ms as f64 / 1000.0;

                // Use the router's actual uptime from the snapshot
                self.uptime_secs = snap.uptime_seconds;

                self.total_frames = snap.total_frames;
                self.plane_count = snap.plane_count;

                for src in &snap.sources {
                    let history = self
                        .sources
                        .entry(src.address.clone())
                        .or_insert_with(|| {
                            self.source_order.push(src.address.clone());
                            SourceHistory::new(src.address.clone(), src.name.clone())
                        });
                    // Update the name in case it was empty on first snapshot
                    if history.name.is_empty() && !src.name.is_empty() {
                        history.name = src.name.clone();
                    }
                    history.connected = src.connected;
                    history.frames_total = src.frames_total;
                    history.current_fps = src.frames_per_sec;
                    history.push_sample(time_secs, src.frames_per_sec);
                }

                self.emergencies = snap.emergencies_72h;
            }
            AppMessage::Connected => {
                self.connection_status = ConnectionStatus::Connected;
            }
            AppMessage::ConnectionError(e) => {
                self.connection_status = ConnectionStatus::Error(e);
            }
        }
    }

    /// Max fps seen across all sources — used for chart y-axis scaling
    pub fn max_fps(&self) -> f64 {
        self.sources
            .values()
            .flat_map(|s| s.samples.iter().map(|p| p.fps))
            .fold(10.0_f64, f64::max) // floor at 10 so axis is never empty
    }
}

// ── Background gRPC task ───────────────────────────────────────────────────

async fn grpc_task(addr: String, tx: mpsc::Sender<AppMessage>) {
    loop {
        match connect_and_stream(addr.clone(), tx.clone()).await {
            Ok(_) => break,
            Err(e) => {
                let _ = tx.send(AppMessage::ConnectionError(format!("{e}"))).await;
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

async fn connect_and_stream(addr: String, tx: mpsc::Sender<AppMessage>) -> Result<()> {
    let channel = Channel::from_shared(addr)?.connect().await?;
    let _ = tx.send(AppMessage::Connected).await;

    let mut client = PlaneTrackingServiceClient::new(channel);
    let mut stream = client
        .stream_metrics(StreamMetricsRequest { interval_ms: Some(1000) })
        .await?
        .into_inner();

    while let Some(snap) = stream.message().await? {
        if tx.send(AppMessage::Snapshot(snap)).await.is_err() {
            break;
        }
    }

    Ok(())
}
