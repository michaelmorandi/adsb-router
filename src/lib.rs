//! ADS-B Plane Tracking Library
//!
//! This library provides components for tracking aircraft via ADS-B:
//!
//! - `beast`: Beast binary protocol decoder for dump1090-fa
//! - `adsb`: ADS-B message decoding using adsb_deku
//! - `state`: Thread-safe aircraft state management with deduplication
//! - `connection`: Multi-source connection management
//! - `grpc`: gRPC service for external access

pub mod adsb;
pub mod beast;
pub mod config;
pub mod connection;
pub mod grpc;
pub mod state;
pub mod synthetic;

// Re-export commonly used types
pub use adsb::{AdsbUpdate, CprFormat, UpdatePayload};
pub use beast::{BeastDecoder, BeastFrame, FrameType};
pub use connection::{ConnectionManager, SourceStatus};
pub use state::{PlaneEvent, PlaneState, PlaneStateManager};
