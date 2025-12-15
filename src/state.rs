//! Plane state management with deduplication and TTL
//!
//! This module maintains the internal representation of all tracked aircraft,
//! handles message deduplication from multiple sources, and removes stale entries.

use dashmap::DashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::broadcast;
use tracing::{debug, trace};

use crate::adsb::{
    is_emergency_squawk, AdsbUpdate, AircraftCategory, CprFormat, EmergencyStatus, UpdatePayload,
};

/// Time after which a plane is considered stale and removed (3 minutes)
const STALE_TIMEOUT: Duration = Duration::from_secs(180);

/// Number of latitude zones for CPR decoding
const NZ: f64 = 15.0;

/// Maximum distance in NM for local CPR decode
const LOCAL_CPR_MAX_RANGE: f64 = 180.0;

/// Internal state for a tracked aircraft
#[derive(Debug, Clone)]
pub struct PlaneState {
    /// ICAO 24-bit address
    pub icao: u32,
    /// Aircraft callsign
    pub callsign: Option<String>,
    /// Decoded latitude in degrees
    pub latitude: Option<f64>,
    /// Decoded longitude in degrees
    pub longitude: Option<f64>,
    /// Barometric altitude in feet
    pub altitude: Option<i32>,
    /// GNSS altitude in feet (more accurate)
    pub altitude_gnss: Option<i32>,
    /// Ground speed in knots
    pub ground_speed: Option<f64>,
    /// Heading in degrees
    pub heading: Option<f64>,
    /// Vertical rate in ft/min
    pub vertical_rate: Option<i32>,
    /// Squawk code (raw value)
    pub squawk: Option<u16>,
    /// Emergency status
    pub emergency: EmergencyStatus,
    /// Aircraft category
    pub category: AircraftCategory,
    /// Whether aircraft is on ground
    pub on_ground: bool,
    /// Whether aircraft has ADS-B (vs Mode-S only)
    pub adsb_capable: bool,
    /// Unix timestamp (ms) when first seen
    pub first_seen: u64,
    /// Unix timestamp (ms) when last updated
    pub last_seen: u64,
    /// Number of messages received
    pub message_count: u64,
    /// Last even CPR position for global decoding
    pub last_even_cpr: Option<CprPosition>,
    /// Last odd CPR position for global decoding
    pub last_odd_cpr: Option<CprPosition>,
}

/// CPR position data for global position decoding
#[derive(Debug, Clone)]
pub struct CprPosition {
    pub lat: u32,
    pub lon: u32,
    pub timestamp: u64,
    pub is_surface: bool,
}

/// Update event broadcast to subscribers
#[derive(Debug, Clone)]
pub enum PlaneEvent {
    /// New plane appeared
    Added(PlaneState),
    /// Existing plane was updated
    Updated(PlaneState),
    /// Plane was removed (timed out)
    Removed(u32), // ICAO address
}

/// Manages the state of all tracked aircraft
pub struct PlaneStateManager {
    /// Map of ICAO address to plane state
    planes: DashMap<u32, PlaneState>,
    /// Recent message hashes for deduplication
    recent_messages: DashMap<u64, u64>, // hash -> timestamp
    /// Broadcast channel for updates
    update_sender: broadcast::Sender<PlaneEvent>,
    /// Total messages processed
    total_messages: AtomicU64,
}

impl PlaneStateManager {
    /// Create a new plane state manager
    pub fn new() -> Arc<Self> {
        let (update_sender, _) = broadcast::channel(10000);

        Arc::new(Self {
            planes: DashMap::new(),
            recent_messages: DashMap::new(),
            update_sender,
            total_messages: AtomicU64::new(0),
        })
    }

    /// Subscribe to plane updates
    pub fn subscribe(&self) -> broadcast::Receiver<PlaneEvent> {
        self.update_sender.subscribe()
    }

    /// Get all current planes
    pub fn get_all_planes(&self) -> Vec<PlaneState> {
        self.planes.iter().map(|r| r.value().clone()).collect()
    }

    /// Get total message count
    pub fn total_messages(&self) -> u64 {
        self.total_messages.load(Ordering::Relaxed)
    }

    /// Get current plane count
    pub fn plane_count(&self) -> usize {
        self.planes.len()
    }

    /// Check if meaningful state fields have changed (excludes last_seen, message_count, CPR state)
    fn has_state_changed(before: &PlaneState, after: &PlaneState) -> bool {
        before.callsign != after.callsign
            || before.latitude != after.latitude
            || before.longitude != after.longitude
            || before.altitude != after.altitude
            || before.altitude_gnss != after.altitude_gnss
            || before.ground_speed != after.ground_speed
            || before.heading != after.heading
            || before.vertical_rate != after.vertical_rate
            || before.squawk != after.squawk
            || before.emergency != after.emergency
            || before.category != after.category
            || before.on_ground != after.on_ground
            || before.adsb_capable != after.adsb_capable
    }

    /// Process an ADS-B/Mode-S update
    pub fn process_update(&self, update: AdsbUpdate) {
        self.total_messages.fetch_add(1, Ordering::Relaxed);

        // Check for duplicate message
        let msg_hash = self.compute_message_hash(&update);
        if self.is_duplicate(msg_hash, update.received_at) {
            trace!(icao = %format!("{:06X}", update.icao), "Duplicate message ignored");
            return;
        }

        let now = update.received_at;
        let icao = update.icao;

        // Check if this is a new plane
        let is_new = !self.planes.contains_key(&icao);

        // Get or create plane state
        let mut plane = self.planes.entry(icao).or_insert_with(|| PlaneState {
            icao,
            callsign: None,
            latitude: None,
            longitude: None,
            altitude: None,
            altitude_gnss: None,
            ground_speed: None,
            heading: None,
            vertical_rate: None,
            squawk: None,
            emergency: EmergencyStatus::None,
            category: AircraftCategory::Unknown,
            on_ground: false,
            adsb_capable: false,
            first_seen: now,
            last_seen: now,
            message_count: 0,
            last_even_cpr: None,
            last_odd_cpr: None,
        });

        // Capture state before update for change detection
        let state_before = plane.clone();

        plane.last_seen = now;
        plane.message_count += 1;

        // Apply the update
        match update.payload {
            UpdatePayload::Identification { callsign, category } => {
                plane.callsign = Some(callsign);
                plane.category = category;
                plane.adsb_capable = true;
            }

            UpdatePayload::AirbornePositionBaro {
                altitude,
                cpr_lat,
                cpr_lon,
                cpr_format,
            } => {
                if let Some(alt) = altitude {
                    plane.altitude = Some(alt);
                }
                plane.on_ground = false;
                plane.adsb_capable = true;

                self.process_cpr_position(&mut plane, cpr_lat, cpr_lon, cpr_format, false, now);
            }

            UpdatePayload::AirbornePositionGnss {
                altitude,
                cpr_lat,
                cpr_lon,
                cpr_format,
            } => {
                if let Some(alt) = altitude {
                    plane.altitude_gnss = Some(alt);
                    // Also use as primary altitude if baro not available
                    if plane.altitude.is_none() {
                        plane.altitude = Some(alt);
                    }
                }
                plane.on_ground = false;
                plane.adsb_capable = true;

                self.process_cpr_position(&mut plane, cpr_lat, cpr_lon, cpr_format, false, now);
            }

            UpdatePayload::SurfacePosition {
                ground_speed,
                ground_track,
                cpr_lat,
                cpr_lon,
                cpr_format,
            } => {
                if let Some(gs) = ground_speed {
                    plane.ground_speed = Some(gs);
                }
                if let Some(trk) = ground_track {
                    plane.heading = Some(trk);
                }
                plane.on_ground = true;
                plane.altitude = Some(0); // Surface position implies ground level
                plane.adsb_capable = true;

                self.process_cpr_position(&mut plane, cpr_lat, cpr_lon, cpr_format, true, now);
            }

            UpdatePayload::AirborneVelocity {
                ground_speed,
                heading,
                vertical_rate,
            } => {
                if let Some(gs) = ground_speed {
                    plane.ground_speed = Some(gs);
                }
                if let Some(hdg) = heading {
                    plane.heading = Some(hdg);
                }
                if let Some(vr) = vertical_rate {
                    plane.vertical_rate = Some(vr);
                }
                plane.adsb_capable = true;
            }

            UpdatePayload::AircraftStatus { emergency, squawk } => {
                plane.emergency = emergency;
                if let Some(sq) = squawk {
                    plane.squawk = Some(sq);
                    // Check if squawk indicates emergency
                    if let Some(em) = is_emergency_squawk(sq) {
                        if plane.emergency == EmergencyStatus::None {
                            plane.emergency = em;
                        }
                    }
                }
                plane.adsb_capable = true;
            }

            UpdatePayload::Squawk { squawk, altitude } => {
                plane.squawk = Some(squawk);
                if let Some(alt) = altitude {
                    plane.altitude = Some(alt);
                }
                // Check if squawk indicates emergency
                if let Some(em) = is_emergency_squawk(squawk) {
                    if plane.emergency == EmergencyStatus::None {
                        plane.emergency = em;
                    }
                }
            }

            UpdatePayload::ModeSDetection => {
                // Just confirms aircraft exists via Mode-S, no additional data
            }
        }

        let plane_state = plane.clone();
        drop(plane);

        // Broadcast update only if state changed
        let event = if is_new {
            debug!(icao = %format!("{:06X}", icao), "New aircraft detected");
            Some(PlaneEvent::Added(plane_state))
        } else if Self::has_state_changed(&state_before, &plane_state) {
            trace!(icao = %format!("{:06X}", icao), "Aircraft state changed");
            Some(PlaneEvent::Updated(plane_state))
        } else {
            // No meaningful change, don't broadcast
            None
        };

        if let Some(event) = event {
            let _ = self.update_sender.send(event);
        }
    }

    /// Process CPR position data with global or local decoding
    fn process_cpr_position(
        &self,
        plane: &mut PlaneState,
        cpr_lat: u32,
        cpr_lon: u32,
        cpr_format: CprFormat,
        is_surface: bool,
        now: u64,
    ) {
        let cpr_pos = CprPosition {
            lat: cpr_lat,
            lon: cpr_lon,
            timestamp: now,
            is_surface,
        };

        // Store CPR data
        match cpr_format {
            CprFormat::Even => plane.last_even_cpr = Some(cpr_pos.clone()),
            CprFormat::Odd => plane.last_odd_cpr = Some(cpr_pos.clone()),
        }

        // Try local decode first if we have a recent position
        if let (Some(lat), Some(lon)) = (plane.latitude, plane.longitude) {
            if let Some((new_lat, new_lon)) =
                self.decode_cpr_local(lat, lon, &cpr_pos, cpr_format, is_surface)
            {
                plane.latitude = Some(new_lat);
                plane.longitude = Some(new_lon);
                return;
            }
        }

        // Fall back to global decode
        if let Some((lat, lon)) = self.decode_cpr_global(plane, is_surface) {
            plane.latitude = Some(lat);
            plane.longitude = Some(lon);
        }
    }

    /// Local CPR decode using reference position
    fn decode_cpr_local(
        &self,
        ref_lat: f64,
        ref_lon: f64,
        cpr: &CprPosition,
        cpr_format: CprFormat,
        is_surface: bool,
    ) -> Option<(f64, f64)> {
        let nz = if is_surface { 90.0 } else { NZ };
        let dlat = if cpr_format == CprFormat::Even {
            360.0 / (4.0 * nz)
        } else {
            360.0 / (4.0 * nz - 1.0)
        };

        let cpr_lat = cpr.lat as f64 / 131072.0;
        let cpr_lon = cpr.lon as f64 / 131072.0;

        // Calculate latitude
        let j = (ref_lat / dlat).floor() + ((mod_frac(ref_lat, dlat) / dlat - cpr_lat + 0.5).floor());
        let lat = dlat * (j + cpr_lat);

        // Check latitude is in valid range
        if lat < -90.0 || lat > 90.0 {
            return None;
        }

        // Calculate longitude zone count
        let nl_val = nl(lat) as f64;
        let nl_adj = if cpr_format == CprFormat::Even {
            nl_val
        } else {
            (nl_val - 1.0).max(1.0)
        };
        let dlon = 360.0 / nl_adj;

        // Calculate longitude
        let m =
            (ref_lon / dlon).floor() + ((mod_frac(ref_lon, dlon) / dlon - cpr_lon + 0.5).floor());
        let lon = dlon * (m + cpr_lon);

        // Normalize longitude
        let lon = normalize_longitude(lon);

        // Verify we're within reasonable range of reference
        let dist = approximate_distance(ref_lat, ref_lon, lat, lon);
        if dist > LOCAL_CPR_MAX_RANGE {
            return None;
        }

        Some((lat, lon))
    }

    /// Global CPR decode using even/odd pair
    fn decode_cpr_global(&self, plane: &PlaneState, is_surface: bool) -> Option<(f64, f64)> {
        let even = plane.last_even_cpr.as_ref()?;
        let odd = plane.last_odd_cpr.as_ref()?;

        // Both must be same type (surface or airborne)
        if even.is_surface != is_surface || odd.is_surface != is_surface {
            return None;
        }

        // Both positions must be recent (within 10 seconds of each other)
        let time_diff = if even.timestamp > odd.timestamp {
            even.timestamp - odd.timestamp
        } else {
            odd.timestamp - even.timestamp
        };

        if time_diff > 10_000 {
            return None;
        }

        let nz = if is_surface { 90.0 } else { NZ };

        // CPR global decoding algorithm
        let lat_even = even.lat as f64 / 131072.0;
        let lat_odd = odd.lat as f64 / 131072.0;
        let lon_even = even.lon as f64 / 131072.0;
        let lon_odd = odd.lon as f64 / 131072.0;

        // Calculate latitude index
        let j = ((59.0 * lat_even - 60.0 * lat_odd + 0.5).floor()) as i32;

        // Latitude zone sizes
        let dlat_even = 360.0 / (4.0 * nz);
        let dlat_odd = 360.0 / (4.0 * nz - 1.0);

        // Calculate latitudes
        let lat_even_result = dlat_even * (mod_func(j, 60) as f64 + lat_even);
        let lat_odd_result = dlat_odd * (mod_func(j, 59) as f64 + lat_odd);

        // Normalize to -90..90
        let lat_even_norm = normalize_latitude(lat_even_result);
        let lat_odd_norm = normalize_latitude(lat_odd_result);

        // Check if both positions are in the same longitude zone
        let nl_even = nl(lat_even_norm);
        let nl_odd = nl(lat_odd_norm);

        if nl_even != nl_odd {
            return None;
        }

        // Use the most recent position for latitude
        let lat = if even.timestamp > odd.timestamp {
            lat_even_norm
        } else {
            lat_odd_norm
        };

        // Calculate longitude
        // Always use NL from the even frame for the m calculation
        let nl = nl_even as f64;
        let m = ((lon_even * (nl - 1.0) - lon_odd * nl + 0.5).floor()) as i32;

        let lon = if even.timestamp > odd.timestamp {
            // Using even frame (most recent)
            let dlon = 360.0 / nl;
            dlon * (mod_func(m, nl as i32) as f64 + lon_even)
        } else {
            // Using odd frame (most recent)
            let dlon = 360.0 / (nl - 1.0).max(1.0);
            dlon * (mod_func(m, (nl - 1.0).max(1.0) as i32) as f64 + lon_odd)
        };

        let lon = normalize_longitude(lon);

        // Sanity check
        if lat < -90.0 || lat > 90.0 || lon < -180.0 || lon > 180.0 {
            return None;
        }

        Some((lat, lon))
    }

    /// Remove stale planes (not seen for 3 minutes)
    pub fn cleanup_stale(&self) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let stale_threshold = now - STALE_TIMEOUT.as_millis() as u64;

        let stale_icaos: Vec<u32> = self
            .planes
            .iter()
            .filter(|r| r.last_seen < stale_threshold)
            .map(|r| *r.key())
            .collect();

        for icao in stale_icaos {
            if self.planes.remove(&icao).is_some() {
                debug!(icao = %format!("{:06X}", icao), "Aircraft removed (stale)");
                let _ = self.update_sender.send(PlaneEvent::Removed(icao));
            }
        }

        // Cleanup old message hashes (keep last 10 seconds)
        let hash_threshold = now - 10_000;
        self.recent_messages.retain(|_, &mut ts| ts > hash_threshold);
    }

    /// Compute hash for deduplication
    fn compute_message_hash(&self, update: &AdsbUpdate) -> u64 {
        let mut hasher = DefaultHasher::new();
        update.icao.hash(&mut hasher);

        match &update.payload {
            UpdatePayload::Identification { callsign, category } => {
                0u8.hash(&mut hasher);
                callsign.hash(&mut hasher);
                (*category as u8).hash(&mut hasher);
            }
            UpdatePayload::AirbornePositionBaro {
                altitude,
                cpr_lat,
                cpr_lon,
                cpr_format,
            } => {
                1u8.hash(&mut hasher);
                altitude.hash(&mut hasher);
                cpr_lat.hash(&mut hasher);
                cpr_lon.hash(&mut hasher);
                (*cpr_format == CprFormat::Odd).hash(&mut hasher);
            }
            UpdatePayload::AirbornePositionGnss {
                altitude,
                cpr_lat,
                cpr_lon,
                cpr_format,
            } => {
                2u8.hash(&mut hasher);
                altitude.hash(&mut hasher);
                cpr_lat.hash(&mut hasher);
                cpr_lon.hash(&mut hasher);
                (*cpr_format == CprFormat::Odd).hash(&mut hasher);
            }
            UpdatePayload::SurfacePosition {
                ground_speed,
                ground_track,
                cpr_lat,
                cpr_lon,
                cpr_format,
            } => {
                3u8.hash(&mut hasher);
                ground_speed.map(|v| v.to_bits()).hash(&mut hasher);
                ground_track.map(|v| v.to_bits()).hash(&mut hasher);
                cpr_lat.hash(&mut hasher);
                cpr_lon.hash(&mut hasher);
                (*cpr_format == CprFormat::Odd).hash(&mut hasher);
            }
            UpdatePayload::AirborneVelocity {
                ground_speed,
                heading,
                vertical_rate,
            } => {
                4u8.hash(&mut hasher);
                ground_speed.map(|v| v.to_bits()).hash(&mut hasher);
                heading.map(|v| v.to_bits()).hash(&mut hasher);
                vertical_rate.hash(&mut hasher);
            }
            UpdatePayload::AircraftStatus { emergency, squawk } => {
                5u8.hash(&mut hasher);
                (*emergency as u8).hash(&mut hasher);
                squawk.hash(&mut hasher);
            }
            UpdatePayload::Squawk { squawk, altitude } => {
                6u8.hash(&mut hasher);
                squawk.hash(&mut hasher);
                altitude.hash(&mut hasher);
            }
            UpdatePayload::ModeSDetection => {
                7u8.hash(&mut hasher);
            }
        }

        hasher.finish()
    }

    /// Check if a message is a duplicate
    fn is_duplicate(&self, hash: u64, timestamp: u64) -> bool {
        if self.recent_messages.contains_key(&hash) {
            return true;
        }
        self.recent_messages.insert(hash, timestamp);
        false
    }
}

/// Modulo function that handles negative numbers correctly
fn mod_func(a: i32, b: i32) -> i32 {
    ((a % b) + b) % b
}

/// Fractional modulo for floating point
fn mod_frac(a: f64, b: f64) -> f64 {
    a - b * (a / b).floor()
}

/// NL function - number of longitude zones at a given latitude
fn nl(lat: f64) -> i32 {
    if lat.abs() >= 87.0 {
        return 1;
    }

    let lat_rad = lat.abs().to_radians();
    let nz = NZ;

    let a = 1.0 - (std::f64::consts::PI / (2.0 * nz)).cos();
    let b = lat_rad.cos().powi(2);
    let c = (1.0 - a / b).acos();
    let d = 2.0 * std::f64::consts::PI;

    (d / c).floor() as i32
}

/// Normalize latitude to -90..90
fn normalize_latitude(lat: f64) -> f64 {
    if lat > 270.0 {
        lat - 360.0
    } else if lat > 90.0 {
        180.0 - lat
    } else {
        lat
    }
}

/// Normalize longitude to -180..180
fn normalize_longitude(lon: f64) -> f64 {
    if lon > 180.0 {
        lon - 360.0
    } else if lon < -180.0 {
        lon + 360.0
    } else {
        lon
    }
}

/// Approximate distance in NM between two points (simple formula)
fn approximate_distance(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let dlat = (lat2 - lat1).to_radians();
    let dlon = (lon2 - lon1).to_radians();
    let lat1_rad = lat1.to_radians();
    let lat2_rad = lat2.to_radians();

    let a = (dlat / 2.0).sin().powi(2) + lat1_rad.cos() * lat2_rad.cos() * (dlon / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().asin();

    // Earth radius in NM
    c * 3440.065
}

impl Default for PlaneStateManager {
    fn default() -> Self {
        Arc::try_unwrap(Self::new()).unwrap_or_else(|arc| (*arc).clone())
    }
}

impl Clone for PlaneStateManager {
    fn clone(&self) -> Self {
        Self {
            planes: self.planes.clone(),
            recent_messages: self.recent_messages.clone(),
            update_sender: self.update_sender.clone(),
            total_messages: AtomicU64::new(self.total_messages.load(Ordering::Relaxed)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nl_function() {
        assert_eq!(nl(0.0), 59);
        assert!(nl(45.0) == 41 || nl(45.0) == 42);
        assert_eq!(nl(87.0), 1);
        assert_eq!(nl(-87.0), 1);
    }

    #[test]
    fn test_normalize_latitude() {
        assert!((normalize_latitude(45.0) - 45.0).abs() < 0.001);
        assert!((normalize_latitude(270.0) - (-90.0)).abs() < 0.001);
        assert!((normalize_latitude(359.0) - (-1.0)).abs() < 0.001);
    }

    #[test]
    fn test_normalize_longitude() {
        assert!((normalize_longitude(45.0) - 45.0).abs() < 0.001);
        assert!((normalize_longitude(200.0) - (-160.0)).abs() < 0.001);
        assert!((normalize_longitude(-200.0) - 160.0).abs() < 0.001);
    }

    #[test]
    fn test_approximate_distance() {
        // JFK to LAX is approximately 2145 NM
        let dist = approximate_distance(40.6413, -73.7781, 33.9425, -118.4081);
        assert!(dist > 2100.0 && dist < 2200.0);
    }

    #[test]
    fn test_deduplication() {
        let manager = PlaneStateManager::new();

        let update1 = AdsbUpdate {
            icao: 0x123456,
            payload: UpdatePayload::Identification {
                callsign: "TEST123".to_string(),
                category: AircraftCategory::Unknown,
            },
            signal_level: 50,
            received_at: 1000,
        };

        manager.process_update(update1.clone());
        assert_eq!(manager.plane_count(), 1);
        assert_eq!(manager.total_messages(), 1);

        manager.process_update(update1);
        assert_eq!(manager.plane_count(), 1);
        assert_eq!(manager.total_messages(), 2);
    }
}
