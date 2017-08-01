//! gRPC service implementation for plane tracking
//!
//! Provides gRPC endpoints for:
//! - GetAllPlanes: Snapshot of all tracked aircraft
//! - StreamUpdates: Continuous stream of updates
//! - GetStatus: Service status information

use std::pin::Pin;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use async_stream::try_stream;
use futures::Stream;
use tonic::{Request, Response, Status};
use tracing::{debug, info};

use crate::adsb::{format_squawk, AircraftCategory, EmergencyStatus};
use crate::connection::ConnectionManager;
use crate::state::{PlaneEvent, PlaneState as InternalPlaneState, PlaneStateManager};

// Include generated protobuf code
pub mod proto {
    tonic::include_proto!("adsb");
}

use proto::plane_tracking_service_server::PlaneTrackingService;
use proto::{
    GetAllPlanesRequest, GetAllPlanesResponse, GetStatusRequest, GetStatusResponse, PlaneState,
    PlaneUpdate, Position, StreamUpdatesRequest, UpdateType, Velocity,
};

/// gRPC service implementation
pub struct PlaneTrackingServiceImpl {
    state_manager: Arc<PlaneStateManager>,
    connection_manager: Arc<ConnectionManager>,
    start_time: Instant,
    update_interval_ms: u64,
}

impl PlaneTrackingServiceImpl {
    /// Create a new gRPC service
    pub fn new(
        state_manager: Arc<PlaneStateManager>,
        connection_manager: Arc<ConnectionManager>,
        update_interval_ms: u64,
    ) -> Self {
        Self {
            state_manager,
            connection_manager,
            start_time: Instant::now(),
            update_interval_ms,
        }
    }
}

/// Convert internal plane state to protobuf message
fn to_proto_plane(plane: &InternalPlaneState) -> PlaneState {
    PlaneState {
        icao_address: format!("{:06X}", plane.icao),
        callsign: plane.callsign.clone().unwrap_or_default(),
        position: plane.latitude.and_then(|lat| {
            plane.longitude.map(|lon| Position {
                latitude: lat,
                longitude: lon,
                on_ground: plane.on_ground,
            })
        }),
        altitude_feet: plane.altitude,
        velocity: if plane.ground_speed.is_some()
            || plane.heading.is_some()
            || plane.vertical_rate.is_some()
        {
            Some(Velocity {
                ground_speed_knots: plane.ground_speed.unwrap_or(0.0),
                heading_degrees: plane.heading.unwrap_or(0.0),
                vertical_rate_fpm: plane.vertical_rate.unwrap_or(0),
            })
        } else {
            None
        },
        last_seen_ms: plane.last_seen as i64,
        first_seen_ms: plane.first_seen as i64,
        message_count: plane.message_count,
        squawk: plane.squawk.map(format_squawk).unwrap_or_default(),
        emergency: emergency_to_proto(plane.emergency).into(),
        category: category_to_string(plane.category),
        adsb_capable: plane.adsb_capable,
        altitude_gnss_feet: plane.altitude_gnss,
    }
}

/// Convert internal EmergencyStatus to proto enum
fn emergency_to_proto(status: EmergencyStatus) -> proto::EmergencyStatus {
    match status {
        EmergencyStatus::None => proto::EmergencyStatus::None,
        EmergencyStatus::General => proto::EmergencyStatus::General,
        EmergencyStatus::Medical => proto::EmergencyStatus::Medical,
        EmergencyStatus::MinFuel => proto::EmergencyStatus::MinFuel,
        EmergencyStatus::NoComm => proto::EmergencyStatus::NoComm,
        EmergencyStatus::Hijack => proto::EmergencyStatus::Hijack,
        EmergencyStatus::Downed => proto::EmergencyStatus::Downed,
        EmergencyStatus::Reserved => proto::EmergencyStatus::Reserved,
    }
}

/// Convert AircraftCategory to display string
fn category_to_string(category: AircraftCategory) -> String {
    match category {
        AircraftCategory::Unknown => String::new(),
        AircraftCategory::NoInfo => String::new(),
        AircraftCategory::Light => "Light".to_string(),
        AircraftCategory::Medium1 => "Medium 1".to_string(),
        AircraftCategory::Medium2 => "Medium 2".to_string(),
        AircraftCategory::HighVortexLarge => "High Vortex Large".to_string(),
        AircraftCategory::Heavy => "Heavy".to_string(),
        AircraftCategory::HighPerformance => "High Performance".to_string(),
        AircraftCategory::Rotorcraft => "Rotorcraft".to_string(),
        AircraftCategory::Glider => "Glider".to_string(),
        AircraftCategory::LighterThanAir => "Lighter Than Air".to_string(),
        AircraftCategory::Parachutist => "Parachutist".to_string(),
        AircraftCategory::Ultralight => "Ultralight".to_string(),
        AircraftCategory::UAV => "UAV".to_string(),
        AircraftCategory::Space => "Space".to_string(),
        AircraftCategory::SurfaceEmergency => "Surface Emergency".to_string(),
        AircraftCategory::SurfaceService => "Surface Service".to_string(),
        AircraftCategory::PointObstacle => "Point Obstacle".to_string(),
        AircraftCategory::ClusterObstacle => "Cluster Obstacle".to_string(),
        AircraftCategory::LineObstacle => "Line Obstacle".to_string(),
        AircraftCategory::Reserved => String::new(),
    }
}

#[tonic::async_trait]
impl PlaneTrackingService for PlaneTrackingServiceImpl {
    /// Get snapshot of all currently tracked planes
    async fn get_all_planes(
        &self,
        _request: Request<GetAllPlanesRequest>,
    ) -> Result<Response<GetAllPlanesResponse>, Status> {
        let planes = self.state_manager.get_all_planes();
        let snapshot_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;

        debug!(plane_count = planes.len(), "GetAllPlanes request");

        let response = GetAllPlanesResponse {
            planes: planes.iter().map(to_proto_plane).collect(),
            snapshot_time_ms: snapshot_time,
        };

        Ok(Response::new(response))
    }

    type StreamUpdatesStream =
        Pin<Box<dyn Stream<Item = Result<PlaneUpdate, Status>> + Send + 'static>>;

    /// Stream continuous updates (additions, updates, removals)
    async fn stream_updates(
        &self,
        request: Request<StreamUpdatesRequest>,
    ) -> Result<Response<Self::StreamUpdatesStream>, Status> {
        let include_snapshot = request.into_inner().include_initial_snapshot;
        let state_manager = self.state_manager.clone();
        let update_interval_ms = self.update_interval_ms;

        info!(include_snapshot, update_interval_ms, "New StreamUpdates subscription");

        let output_stream = try_stream! {
            // Optionally send initial snapshot
            if include_snapshot {
                let planes = state_manager.get_all_planes();
                debug!(plane_count = planes.len(), "Sending initial snapshot");

                for plane in planes {
                    yield PlaneUpdate {
                        update_type: UpdateType::Add.into(),
                        plane: Some(to_proto_plane(&plane)),
                        removed_icao: None,
                    };
                }
            }

            // Subscribe to updates
            let mut receiver = state_manager.subscribe();

            // Rate limiting: track last sent time per ICAO
            use std::collections::HashMap;
            let mut last_sent: HashMap<u32, u64> = HashMap::new();

            loop {
                match receiver.recv().await {
                    Ok(event) => {
                        let should_send = if update_interval_ms == 0 {
                            // No throttling
                            true
                        } else {
                            match &event {
                                PlaneEvent::Added(_) | PlaneEvent::Removed(_) => {
                                    // Always send add/remove events immediately
                                    true
                                }
                                PlaneEvent::Updated(plane) => {
                                    // Check if enough time has passed since last update for this plane
                                    let now = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap()
                                        .as_millis() as u64;

                                    let last_time = last_sent.get(&plane.icao).copied().unwrap_or(0);
                                    let elapsed = now.saturating_sub(last_time);

                                    if elapsed >= update_interval_ms {
                                        last_sent.insert(plane.icao, now);
                                        debug!(icao = %format!("{:06X}", plane.icao), elapsed_ms = elapsed, "Sending throttled update");
                                        true
                                    } else {
                                        debug!(icao = %format!("{:06X}", plane.icao), elapsed_ms = elapsed, threshold_ms = update_interval_ms, "Throttling update");
                                        false
                                    }
                                }
                            }
                        };

                        if should_send {
                            let update = match event {
                                PlaneEvent::Added(plane) => PlaneUpdate {
                                    update_type: UpdateType::Add.into(),
                                    plane: Some(to_proto_plane(&plane)),
                                    removed_icao: None,
                                },
                                PlaneEvent::Updated(plane) => PlaneUpdate {
                                    update_type: UpdateType::Update.into(),
                                    plane: Some(to_proto_plane(&plane)),
                                    removed_icao: None,
                                },
                                PlaneEvent::Removed(icao) => {
                                    // Clean up tracking for removed plane
                                    last_sent.remove(&icao);
                                    PlaneUpdate {
                                        update_type: UpdateType::Remove.into(),
                                        plane: None,
                                        removed_icao: Some(format!("{:06X}", icao)),
                                    }
                                },
                            };
                            yield update;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        // Client fell behind, continue with new messages
                        debug!(skipped, "Stream subscriber lagged, skipped messages");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        // Channel closed, end stream
                        break;
                    }
                }
            }
        };

        Ok(Response::new(Box::pin(output_stream)))
    }

    /// Get service status
    async fn get_status(
        &self,
        _request: Request<GetStatusRequest>,
    ) -> Result<Response<GetStatusResponse>, Status> {
        let source_status = self.connection_manager.get_status().await;
        let connected_sources: Vec<String> = source_status
            .iter()
            .filter(|s| s.connected)
            .map(|s| s.address.to_string())
            .collect();

        let response = GetStatusResponse {
            plane_count: self.state_manager.plane_count() as u32,
            source_count: self.connection_manager.connected_count().await as u32,
            connected_sources,
            total_messages: self.state_manager.total_messages(),
            uptime_seconds: self.start_time.elapsed().as_secs(),
        };

        Ok(Response::new(response))
    }
}

/// Start the gRPC server
pub async fn start_grpc_server(
    addr: std::net::SocketAddr,
    update_interval_ms: u64,
    state_manager: Arc<PlaneStateManager>,
    connection_manager: Arc<ConnectionManager>,
) -> anyhow::Result<()> {
    use proto::plane_tracking_service_server::PlaneTrackingServiceServer;

    let service = PlaneTrackingServiceImpl::new(state_manager, connection_manager, update_interval_ms);

    info!(address = %addr, "Starting gRPC server");

    tonic::transport::Server::builder()
        .add_service(PlaneTrackingServiceServer::new(service))
        .serve(addr)
        .await?;

    Ok(())
}
