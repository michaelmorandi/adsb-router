//! Synthetic aircraft data generator for testing
//!
//! Generates synthetic ADS-B updates for various aircraft types flying in circular patterns
//! over Switzerland with altitude cycling (climb, hold, descend, hold).

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time;
use tracing::info;

use crate::adsb::{AdsbUpdate, AircraftCategory, CprFormat, UpdatePayload};
use crate::state::PlaneStateManager;

const SWITZERLAND_CENTER_LAT: f64 = 46.8;
const SWITZERLAND_CENTER_LON: f64 = 8.2;

/// Number of latitude zones for CPR encoding (same as decoder)
const NZ: f64 = 15.0;

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

/// Fractional modulo
fn mod_frac(a: f64, b: f64) -> f64 {
    a - b * (a / b).floor()
}

/// Configuration for a synthetic aircraft
#[derive(Debug, Clone)]
struct SyntheticAircraft {
    icao: u32,
    callsign: String,
    category: AircraftCategory,
    center_lat: f64,
    center_lon: f64,
    radius_nm: f64,     // Nautical miles
    speed_kts: f64,     // Knots
    min_altitude: i32,  // Feet
    max_altitude: i32,  // Feet
    climb_rate: i32,    // Feet per minute
}

/// Altitude phase in the cycle
#[derive(Debug, Clone, Copy)]
enum AltitudePhase {
    Climbing,
    CruiseHigh,
    Descending,
    CruiseLow,
}

/// State for a synthetic aircraft
struct AircraftState {
    config: SyntheticAircraft,
    angle: f64,             // Current angle in degrees
    altitude: i32,          // Current altitude in feet
    altitude_phase: AltitudePhase,
    phase_start_time: u64,  // Timestamp when phase started
}

impl AircraftState {
    fn new(config: SyntheticAircraft) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        Self {
            altitude: config.min_altitude,
            angle: 0.0,
            altitude_phase: AltitudePhase::Climbing,
            phase_start_time: now,
            config,
        }
    }

    /// Update position and altitude
    fn update(&mut self, delta_seconds: f64) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        // Update angle based on speed and radius for smooth circular motion
        // Angular velocity ω = v / r (radians per unit time)
        // Since speed is in knots (NM/hour) and radius in NM:
        //   circumference = 2π * radius (in NM)
        //   time for full circle = circumference / speed (in hours)
        //   degrees per second = 360° / (time_for_circle * 3600)
        // Simplified: degrees_per_second = (speed / circumference) * 360 / 3600
        let circumference_nm = 2.0 * std::f64::consts::PI * self.config.radius_nm;
        let hours_per_circle = circumference_nm / self.config.speed_kts;
        let degrees_per_second = 360.0 / (hours_per_circle * 3600.0);
        self.angle = (self.angle + degrees_per_second * delta_seconds) % 360.0;

        // Update altitude based on phase
        let phase_duration = (now - self.phase_start_time) as f64 / 1000.0; // seconds

        match self.altitude_phase {
            AltitudePhase::Climbing => {
                let altitude_change = (self.config.climb_rate as f64 * delta_seconds / 60.0) as i32;
                self.altitude += altitude_change;

                if self.altitude >= self.config.max_altitude {
                    self.altitude = self.config.max_altitude;
                    self.altitude_phase = AltitudePhase::CruiseHigh;
                    self.phase_start_time = now;
                }
            }
            AltitudePhase::CruiseHigh => {
                // Hold for 60 seconds
                if phase_duration >= 60.0 {
                    self.altitude_phase = AltitudePhase::Descending;
                    self.phase_start_time = now;
                }
            }
            AltitudePhase::Descending => {
                let altitude_change = (self.config.climb_rate as f64 * delta_seconds / 60.0) as i32;
                self.altitude -= altitude_change;

                if self.altitude <= self.config.min_altitude {
                    self.altitude = self.config.min_altitude;
                    self.altitude_phase = AltitudePhase::CruiseLow;
                    self.phase_start_time = now;
                }
            }
            AltitudePhase::CruiseLow => {
                // Hold for 60 seconds
                if phase_duration >= 60.0 {
                    self.altitude_phase = AltitudePhase::Climbing;
                    self.phase_start_time = now;
                }
            }
        }

    }

    /// Calculate current position on a perfect circle
    /// Uses parametric circle equations adjusted for Earth's coordinate system:
    /// - Latitude offset: radius * cos(angle)  (North at angle=0)
    /// - Longitude offset: radius * sin(angle) / cos(center_lat) (East at angle=90)
    /// The cos(center_lat) correction accounts for longitude convergence at higher latitudes
    fn get_position(&self) -> (f64, f64) {
        let angle_rad = self.angle.to_radians();
        let center_lat_rad = self.config.center_lat.to_radians();

        // 1 nautical mile = 1/60 degree of latitude
        let radius_deg = self.config.radius_nm / 60.0;

        // Parametric circle: angle=0 is North, angle=90 is East (clockwise from North)
        // x (East) = radius * sin(angle)
        // y (North) = radius * cos(angle)
        let lat_offset = radius_deg * angle_rad.cos();
        let lon_offset = radius_deg * angle_rad.sin() / center_lat_rad.cos();

        let lat = self.config.center_lat + lat_offset;
        let lon = self.config.center_lon + lon_offset;

        (lat, lon)
    }

    /// Get heading in degrees (true heading, clockwise from North)
    /// The heading is tangent to the circle at the current position.
    /// For clockwise motion: tangent direction = angle + 90°
    /// This gives the direction of travel along the circle.
    fn get_heading(&self) -> f64 {
        // For a circle where angle=0 is North and increases clockwise:
        // - At angle=0 (North point), heading should be 90° (East)
        // - At angle=90 (East point), heading should be 180° (South)
        // - At angle=180 (South point), heading should be 270° (West)
        // - At angle=270 (West point), heading should be 0° (North)
        // So heading = angle + 90° (for clockwise motion)
        let heading = self.angle + 90.0;
        if heading >= 360.0 {
            heading - 360.0
        } else {
            heading
        }
    }

    /// Encode position to CPR format
    /// CPR (Compact Position Reporting) encodes lat/lon into 17-bit values
    fn encode_cpr(&self, lat: f64, lon: f64, is_even: bool) -> (u32, u32) {
        const NZ: f64 = 15.0; // Number of latitude zones (same as decoder)
        const CPR_MAX: f64 = 131072.0; // 2^17

        // Calculate dlat based on even/odd
        let dlat = if is_even {
            360.0 / (4.0 * NZ)
        } else {
            360.0 / (4.0 * NZ - 1.0)
        };

        // Encode latitude
        // lat = dlat * (mod(j, nz) + cpr_lat/131072)
        // So: cpr_lat/131072 = (lat/dlat) - floor(lat/dlat) = frac(lat/dlat)
        let lat_cpr_frac = mod_frac(lat, dlat) / dlat;
        let cpr_lat = (lat_cpr_frac * CPR_MAX).floor() as u32 % (CPR_MAX as u32);

        // Calculate NL (number of longitude zones) at this latitude
        let nl_val = nl(lat) as f64;
        let nl_adj = if is_even {
            nl_val
        } else {
            (nl_val - 1.0).max(1.0)
        };
        let dlon = 360.0 / nl_adj;

        // Encode longitude
        // Similar to latitude encoding
        let lon_cpr_frac = mod_frac(lon, dlon) / dlon;
        let cpr_lon = (lon_cpr_frac * CPR_MAX).floor() as u32 % (CPR_MAX as u32);

        (cpr_lat, cpr_lon)
    }

    /// Generate AdsbUpdate messages for this aircraft
    fn generate_updates(&mut self) -> Vec<AdsbUpdate> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let (lat, lon) = self.get_position();
        // Balloons have no directional control, so they don't report heading
        let heading = (self.config.category != AircraftCategory::LighterThanAir)
            .then(|| self.get_heading());

        // Encode both even and odd CPR frames for the same position
        // Global CPR decode requires both frames to be from roughly the same location
        let (cpr_lat_even, cpr_lon_even) = self.encode_cpr(lat, lon, true);
        let (cpr_lat_odd, cpr_lon_odd) = self.encode_cpr(lat, lon, false);

        let mut updates = Vec::new();

        // Identification message (every 10th update cycle)
        if self.angle as u32 % 36 < 4 {
            updates.push(AdsbUpdate {
                icao: self.config.icao,
                payload: UpdatePayload::Identification {
                    callsign: self.config.callsign.clone(),
                    category: self.config.category,
                },
                signal_level: 200,
                received_at: now,
                is_mlat: false,
            });
        }

        // Send even position frame
        updates.push(AdsbUpdate {
            icao: self.config.icao,
            payload: UpdatePayload::AirbornePositionBaro {
                altitude: Some(self.altitude),
                cpr_lat: cpr_lat_even,
                cpr_lon: cpr_lon_even,
                cpr_format: CprFormat::Even,
            },
            signal_level: 200,
            received_at: now,
            is_mlat: false,
        });

        // Send odd position frame (slightly later timestamp for proper ordering)
        updates.push(AdsbUpdate {
            icao: self.config.icao,
            payload: UpdatePayload::AirbornePositionBaro {
                altitude: Some(self.altitude),
                cpr_lat: cpr_lat_odd,
                cpr_lon: cpr_lon_odd,
                cpr_format: CprFormat::Odd,
            },
            signal_level: 200,
            received_at: now + 1, // 1ms later
            is_mlat: false,
        });

        // Velocity message
        updates.push(AdsbUpdate {
            icao: self.config.icao,
            payload: UpdatePayload::AirborneVelocity {
                ground_speed: Some(self.config.speed_kts),
                heading,
                vertical_rate: Some(match self.altitude_phase {
                    AltitudePhase::Climbing => self.config.climb_rate,
                    AltitudePhase::Descending => -self.config.climb_rate,
                    _ => 0,
                }),
            },
            signal_level: 200,
            received_at: now,
            is_mlat: false,
        });

        updates
    }
}

/// Create synthetic aircraft configurations
fn create_synthetic_aircraft() -> Vec<SyntheticAircraft> {
    vec![
        // Heavy aircraft (A380, B747) - Large circle (100km diameter = 54 NM radius)
        SyntheticAircraft {
            icao: 0x4B1A01,
            callsign: "SWR100".to_string(),
            category: AircraftCategory::Heavy,
            center_lat: SWITZERLAND_CENTER_LAT + 0.5,
            center_lon: SWITZERLAND_CENTER_LON - 0.5,
            radius_nm: 27.0,
            speed_kts: 450.0,
            min_altitude: 30000,
            max_altitude: 38000,
            climb_rate: 1500,
        },
        // Another Heavy
        SyntheticAircraft {
            icao: 0x4B1A02,
            callsign: "SWR200".to_string(),
            category: AircraftCategory::Heavy,
            center_lat: SWITZERLAND_CENTER_LAT - 0.5,
            center_lon: SWITZERLAND_CENTER_LON + 0.5,
            radius_nm: 27.0,
            speed_kts: 460.0,
            min_altitude: 28000,
            max_altitude: 36000,
            climb_rate: 1400,
        },
        // Medium2 aircraft (B737, A320) - Medium circle (60km diameter = 32 NM radius)
        SyntheticAircraft {
            icao: 0x4B2A01,
            callsign: "EZY101".to_string(),
            category: AircraftCategory::Medium2,
            center_lat: SWITZERLAND_CENTER_LAT + 0.3,
            center_lon: SWITZERLAND_CENTER_LON + 0.3,
            radius_nm: 16.0,
            speed_kts: 380.0,
            min_altitude: 20000,
            max_altitude: 28000,
            climb_rate: 1800,
        },
        SyntheticAircraft {
            icao: 0x4B2A02,
            callsign: "EZY202".to_string(),
            category: AircraftCategory::Medium2,
            center_lat: SWITZERLAND_CENTER_LAT - 0.3,
            center_lon: SWITZERLAND_CENTER_LON - 0.3,
            radius_nm: 16.0,
            speed_kts: 390.0,
            min_altitude: 22000,
            max_altitude: 30000,
            climb_rate: 1700,
        },
        // Medium1 aircraft - Smaller circle (40km diameter = 22 NM radius)
        SyntheticAircraft {
            icao: 0x4B3A01,
            callsign: "BEE101".to_string(),
            category: AircraftCategory::Medium1,
            center_lat: SWITZERLAND_CENTER_LAT,
            center_lon: SWITZERLAND_CENTER_LON,
            radius_nm: 11.0,
            speed_kts: 280.0,
            min_altitude: 15000,
            max_altitude: 22000,
            climb_rate: 1200,
        },
        // Light aircraft - Small circle (20km diameter = 11 NM radius)
        // Cessna 172 style: cruise ~120 kts, Cirrus/Mooney style: ~160 kts
        SyntheticAircraft {
            icao: 0x4B4A01,
            callsign: "HB001".to_string(),
            category: AircraftCategory::Light,
            center_lat: SWITZERLAND_CENTER_LAT + 0.2,
            center_lon: SWITZERLAND_CENTER_LON - 0.2,
            radius_nm: 5.4,
            speed_kts: 120.0,  // Typical Cessna 172 cruise
            min_altitude: 5000,
            max_altitude: 10000,
            climb_rate: 700,
        },
        SyntheticAircraft {
            icao: 0x4B4A02,
            callsign: "HB002".to_string(),
            category: AircraftCategory::Light,
            center_lat: SWITZERLAND_CENTER_LAT - 0.2,
            center_lon: SWITZERLAND_CENTER_LON + 0.2,
            radius_nm: 5.4,
            speed_kts: 140.0,  // Faster light aircraft (Cirrus, Mooney)
            min_altitude: 4000,
            max_altitude: 9000,
            climb_rate: 800,
        },
        // High performance - Swiss Air Force F/A-18 Hornet
        SyntheticAircraft {
            icao: 0x4B7F8C,
            callsign: "EAGLE1".to_string(),
            category: AircraftCategory::HighPerformance,
            center_lat: SWITZERLAND_CENTER_LAT - 0.45,
            center_lon: SWITZERLAND_CENTER_LON + 0.5,
            radius_nm: 27.0, // ~50 km diameter
            speed_kts: 700.0,
            min_altitude: 30000,
            max_altitude: 50000,
            climb_rate: 5000,
        },
        // Rotorcraft - Very small circle (10km diameter = 5.4 NM radius)
        SyntheticAircraft {
            icao: 0x4B7FA7,
            callsign: "HELI1".to_string(),
            category: AircraftCategory::Rotorcraft,
            center_lat: SWITZERLAND_CENTER_LAT - 0.1,
            center_lon: SWITZERLAND_CENTER_LON,
            radius_nm: 2.7,
            speed_kts: 120.0,
            min_altitude: 1000,
            max_altitude: 5000,
            climb_rate: 500,
        },
        // Glider (Category Set B)
        SyntheticAircraft {
            icao: 0x4B7A01,
            callsign: "GLIDE1".to_string(),
            category: AircraftCategory::Glider,
            center_lat: SWITZERLAND_CENTER_LAT,
            center_lon: SWITZERLAND_CENTER_LON - 0.4,
            radius_nm: 8.1,
            speed_kts: 90.0,
            min_altitude: 3000,
            max_altitude: 15000,
            climb_rate: 300,
        },
        // UAV (Category Set B) - Unmanned Aerial Vehicle
        SyntheticAircraft {
            icao: 0x4B7B01,
            callsign: "DRONE1".to_string(),
            category: AircraftCategory::UAV,
            center_lat: SWITZERLAND_CENTER_LAT + 0.15,
            center_lon: SWITZERLAND_CENTER_LON + 0.15,
            radius_nm: 2.7,
            speed_kts: 50.0,
            min_altitude: 500,
            max_altitude: 3000,
            climb_rate: 400,
        },
        // Ultralight (Category Set B)
        SyntheticAircraft {
            icao: 0x4B7C01,
            callsign: "UL001".to_string(),
            category: AircraftCategory::Ultralight,
            center_lat: SWITZERLAND_CENTER_LAT - 0.15,
            center_lon: SWITZERLAND_CENTER_LON - 0.15,
            radius_nm: 4.0,
            speed_kts: 70.0,
            min_altitude: 2000,
            max_altitude: 8000,
            climb_rate: 500,
        },
        // LighterThanAir (Category Set B) - Hot air balloon #1
        SyntheticAircraft {
            icao: 0x4B17F9,
            callsign: "BALN1".to_string(),
            category: AircraftCategory::LighterThanAir,
            center_lat: SWITZERLAND_CENTER_LAT + 0.25,
            center_lon: SWITZERLAND_CENTER_LON - 0.25,
            radius_nm: 1.0,
            speed_kts: 15.0,
            min_altitude: 1000,
            max_altitude: 4000,
            climb_rate: 200,
        },
        // Rotorcraft (Category Set A) - Helicopter #2
        SyntheticAircraft {
            icao: 0x4B43D0,
            callsign: "HELI2".to_string(),
            category: AircraftCategory::Rotorcraft,
            center_lat: SWITZERLAND_CENTER_LAT - 0.35,
            center_lon: SWITZERLAND_CENTER_LON - 0.15,
            radius_nm: 0.8,
            speed_kts: 12.0,
            min_altitude: 800,
            max_altitude: 3500,
            climb_rate: 180,
        },
        // Parachutist (Category Set B) - Skydiver under canopy
        // Horizontal speed is low (10-20 kts), mostly drifting with wind
        SyntheticAircraft {
            icao: 0x4B7E01,
            callsign: "JUMP1".to_string(),
            category: AircraftCategory::Parachutist,
            center_lat: SWITZERLAND_CENTER_LAT - 0.25,
            center_lon: SWITZERLAND_CENTER_LON + 0.25,
            radius_nm: 0.3,
            speed_kts: 15.0,
            min_altitude: 2000,
            max_altitude: 12000,
            climb_rate: 1000, // Represents descent rate when "climbing" back up (jump plane)
        },
        // Space (Category Set B) - High-altitude reconnaissance aircraft (U-2 style)
        // U-2 cruises at 400+ kts at 70,000+ ft
        SyntheticAircraft {
            icao: 0x4B7F01,
            callsign: "RECON1".to_string(),
            category: AircraftCategory::Space, // Using Space category for high-altitude
            center_lat: SWITZERLAND_CENTER_LAT + 0.1,
            center_lon: SWITZERLAND_CENTER_LON - 0.1,
            radius_nm: 40.0,
            speed_kts: 410.0,
            min_altitude: 60000,
            max_altitude: 70000,
            climb_rate: 2000,
        },
    ]
}

/// Start synthetic data generator
pub async fn start_synthetic_generator(state_manager: Arc<PlaneStateManager>) {
    info!("Starting synthetic aircraft generator");

    let configs = create_synthetic_aircraft();
    let mut aircraft_states: Vec<AircraftState> = configs
        .into_iter()
        .map(AircraftState::new)
        .collect();

    info!("Generated {} synthetic aircraft", aircraft_states.len());

    // Update interval (1 second)
    let mut interval = time::interval(Duration::from_secs(1));
    let delta_seconds = 1.0;

    loop {
        interval.tick().await;

        for aircraft in aircraft_states.iter_mut() {
            // Update aircraft state
            aircraft.update(delta_seconds);

            // Generate and process updates
            let updates = aircraft.generate_updates();
            for update in updates {
                state_manager.process_update(update);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cpr_encode_switzerland() {
        // Test that CPR encoding produces valid values for Switzerland coordinates
        let config = SyntheticAircraft {
            icao: 0x4B1A01,
            callsign: "TEST".to_string(),
            category: AircraftCategory::Light,
            center_lat: SWITZERLAND_CENTER_LAT,
            center_lon: SWITZERLAND_CENTER_LON,
            radius_nm: 5.0,
            speed_kts: 100.0,
            min_altitude: 5000,
            max_altitude: 10000,
            climb_rate: 500,
        };

        let state = AircraftState::new(config);

        // Get position (should be near Switzerland center)
        let (lat, lon) = state.get_position();
        println!("Position: lat={}, lon={}", lat, lon);

        // Encode to CPR (both even and odd)
        let (cpr_lat_even, cpr_lon_even) = state.encode_cpr(lat, lon, true);
        let (cpr_lat_odd, cpr_lon_odd) = state.encode_cpr(lat, lon, false);

        println!("Even CPR: lat={}, lon={}", cpr_lat_even, cpr_lon_even);
        println!("Odd CPR: lat={}, lon={}", cpr_lat_odd, cpr_lon_odd);

        // Verify values are in valid 17-bit range
        assert!(cpr_lat_even < 131072, "Even CPR lat out of range");
        assert!(cpr_lon_even < 131072, "Even CPR lon out of range");
        assert!(cpr_lat_odd < 131072, "Odd CPR lat out of range");
        assert!(cpr_lon_odd < 131072, "Odd CPR lon out of range");

        // Now decode using the same algorithm as state.rs
        // This verifies the round-trip works
        let dlat_even = 360.0 / (4.0 * NZ);
        let dlat_odd = 360.0 / (4.0 * NZ - 1.0);

        let lat_even_frac = cpr_lat_even as f64 / 131072.0;
        let lat_odd_frac = cpr_lat_odd as f64 / 131072.0;

        // Global decode
        let j = ((59.0 * lat_even_frac - 60.0 * lat_odd_frac + 0.5).floor()) as i32;

        fn mod_func(a: i32, b: i32) -> i32 {
            ((a % b) + b) % b
        }

        let decoded_lat_even = dlat_even * (mod_func(j, 60) as f64 + lat_even_frac);
        let _decoded_lat_odd = dlat_odd * (mod_func(j, 59) as f64 + lat_odd_frac);

        // Normalize
        let decoded_lat_even = if decoded_lat_even > 270.0 {
            decoded_lat_even - 360.0
        } else if decoded_lat_even > 90.0 {
            180.0 - decoded_lat_even
        } else {
            decoded_lat_even
        };

        println!("Decoded lat (even): {}", decoded_lat_even);
        println!("Original lat: {}", lat);

        // The decoded latitude should be close to the original
        // (within a zone, since we need longitude to fully resolve)
        assert!(
            (decoded_lat_even - lat).abs() < 10.0,
            "Decoded latitude too far from original: {} vs {}",
            decoded_lat_even,
            lat
        );
    }
}
