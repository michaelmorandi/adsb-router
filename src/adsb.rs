//! ADS-B and Mode-S message decoding
//!
//! This module handles decoding of ADS-B (DF17) and Mode-S messages from Beast frames,
//! extracting aircraft information including callsign, position, altitude, velocity,
//! squawk codes, and emergency status.

use adsb_deku::adsb::{StatusForGroundTrack, TypeCoding};
use adsb_deku::deku::DekuContainerRead;
use adsb_deku::{adsb, CPRFormat, Frame, Sign, DF};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::beast::{BeastFrame, FrameType};

/// Emergency/priority status codes (matches ADS-B spec)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EmergencyStatus {
    #[default]
    None,
    General,      // 7700
    Medical,
    MinFuel,
    NoComm,       // 7600
    Hijack,       // 7500
    Downed,
    Reserved,
}

/// Aircraft category from identification message
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AircraftCategory {
    #[default]
    Unknown,
    // Category Set A (TC=4)
    NoInfo,
    Light,           // < 15500 lbs
    Medium1,         // 15500 to 75000 lbs
    Medium2,         // 75000 to 300000 lbs
    HighVortexLarge,
    Heavy,           // > 300000 lbs
    HighPerformance, // > 5g acceleration and > 400 kts
    Rotorcraft,
    // Category Set B (TC=3)
    Glider,
    LighterThanAir,
    Parachutist,
    Ultralight,
    UAV,
    Space,
    // Category Set C (TC=2)
    SurfaceEmergency,
    SurfaceService,
    PointObstacle,
    ClusterObstacle,
    LineObstacle,
    // Reserved
    Reserved,
}

/// Decoded update from a Mode-S/ADS-B message
#[derive(Debug, Clone)]
pub struct AdsbUpdate {
    /// ICAO 24-bit address
    pub icao: u32,
    /// Update payload
    pub payload: UpdatePayload,
    /// Signal strength from receiver
    pub signal_level: u8,
    /// Timestamp when received
    pub received_at: u64,
}

/// Types of updates that can be extracted from Mode-S/ADS-B messages
#[derive(Debug, Clone)]
pub enum UpdatePayload {
    /// Aircraft identification (callsign and category)
    Identification {
        callsign: String,
        category: AircraftCategory,
    },
    /// Airborne position with CPR-encoded coordinates (barometric altitude)
    AirbornePositionBaro {
        altitude: Option<i32>,
        cpr_lat: u32,
        cpr_lon: u32,
        cpr_format: CprFormat,
    },
    /// Airborne position with GNSS altitude
    AirbornePositionGnss {
        altitude: Option<i32>,
        cpr_lat: u32,
        cpr_lon: u32,
        cpr_format: CprFormat,
    },
    /// Surface position (aircraft on ground)
    SurfacePosition {
        ground_speed: Option<f64>,
        ground_track: Option<f64>,
        cpr_lat: u32,
        cpr_lon: u32,
        cpr_format: CprFormat,
    },
    /// Airborne velocity
    AirborneVelocity {
        ground_speed: Option<f64>,
        heading: Option<f64>,
        vertical_rate: Option<i32>,
    },
    /// Aircraft status (emergency/priority)
    AircraftStatus {
        emergency: EmergencyStatus,
        squawk: Option<u16>,
    },
    /// Squawk code from surveillance reply (DF 4/5/20/21)
    Squawk {
        squawk: u16,
        altitude: Option<i32>,
    },
    /// Mode-S only detection (DF 11 All-Call)
    ModeSDetection,
}

/// CPR frame format (odd or even)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CprFormat {
    Even,
    Odd,
}

impl From<CPRFormat> for CprFormat {
    fn from(fmt: CPRFormat) -> Self {
        match fmt {
            CPRFormat::Even => CprFormat::Even,
            CPRFormat::Odd => CprFormat::Odd,
        }
    }
}

/// Convert ICAO address bytes to u32
fn icao_to_u32(icao: &[u8; 3]) -> u32 {
    ((icao[0] as u32) << 16) | ((icao[1] as u32) << 8) | (icao[2] as u32)
}

/// Decode a Beast frame into an update (if applicable)
pub fn decode_beast_frame(frame: &BeastFrame) -> Option<AdsbUpdate> {
    let received_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    match frame.frame_type {
        FrameType::ModeSLong => decode_long_frame(&frame.message, frame.signal_level, received_at),
        FrameType::ModeSShort => decode_short_frame(&frame.message, frame.signal_level, received_at),
        FrameType::ModeAC => None, // Mode A/C doesn't have ICAO address
    }
}

/// Decode a Mode-S long frame (112 bits / 14 bytes)
fn decode_long_frame(message: &[u8], signal_level: u8, received_at: u64) -> Option<AdsbUpdate> {
    let adsb_frame = Frame::from_bytes((message, 0)).ok()?.1;

    match &adsb_frame.df {
        DF::ADSB(adsb_msg) => {
            let icao = icao_to_u32(&adsb_msg.icao.0);
            let payload = decode_adsb_message(&adsb_msg.me)?;

            Some(AdsbUpdate {
                icao,
                payload,
                signal_level,
                received_at,
            })
        }
        DF::TisB { .. } => None,
        // DF 20/21 (Comm-B replies) have ICAO encoded in parity field,
        // which requires knowing the original interrogator - not handled here
        _ => None,
    }
}

/// Decode a Mode-S short frame (56 bits / 7 bytes)
fn decode_short_frame(message: &[u8], signal_level: u8, received_at: u64) -> Option<AdsbUpdate> {
    if message.len() < 7 {
        return None;
    }

    let df = (message[0] >> 3) & 0x1f;

    match df {
        11 => {
            // DF 11 - All-Call Reply
            // ICAO is in bytes 1-3
            let icao = ((message[1] as u32) << 16) | ((message[2] as u32) << 8) | (message[3] as u32);
            Some(AdsbUpdate {
                icao,
                payload: UpdatePayload::ModeSDetection,
                signal_level,
                received_at,
            })
        }
        4 => {
            // DF 4 - Surveillance Altitude Reply
            // Note: ICAO is encoded in parity, we'd need to check against known aircraft
            // For now, we can't reliably extract ICAO without additional context
            None
        }
        5 => {
            // DF 5 - Surveillance Identity Reply
            // Similar to DF 4, ICAO in parity
            None
        }
        _ => None,
    }
}

/// Decode the ME (Message Extension) field of an ADS-B message
fn decode_adsb_message(me: &adsb::ME) -> Option<UpdatePayload> {
    use adsb::ME;

    match me {
        ME::AircraftIdentification(id) => {
            let callsign = id.cn.to_string().trim().to_string();
            if callsign.is_empty() {
                return None;
            }
            let category = decode_aircraft_category(id.tc, id.ca);
            Some(UpdatePayload::Identification { callsign, category })
        }

        ME::AirbornePositionBaroAltitude(pos) => {
            let altitude = pos.alt.map(|a| a as i32);
            Some(UpdatePayload::AirbornePositionBaro {
                altitude,
                cpr_lat: pos.lat_cpr,
                cpr_lon: pos.lon_cpr,
                cpr_format: pos.odd_flag.into(),
            })
        }

        ME::AirbornePositionGNSSAltitude(pos) => {
            let altitude = pos.alt.map(|a| a as i32);
            Some(UpdatePayload::AirbornePositionGnss {
                altitude,
                cpr_lat: pos.lat_cpr,
                cpr_lon: pos.lon_cpr,
                cpr_format: pos.odd_flag.into(),
            })
        }

        ME::SurfacePosition(pos) => {
            let ground_speed = decode_surface_speed(pos.mov);
            let ground_track = if pos.s == StatusForGroundTrack::Valid {
                Some((pos.trk as f64) * 360.0 / 128.0)
            } else {
                None
            };

            Some(UpdatePayload::SurfacePosition {
                ground_speed,
                ground_track,
                cpr_lat: pos.lat_cpr,
                cpr_lon: pos.lon_cpr,
                cpr_format: pos.f.into(),
            })
        }

        ME::AirborneVelocity(vel) => {
            let (ground_speed, heading, vertical_rate) = extract_velocity(vel);
            Some(UpdatePayload::AirborneVelocity {
                ground_speed,
                heading,
                vertical_rate,
            })
        }

        ME::AircraftStatus(status) => {
            let (emergency, squawk) = decode_aircraft_status(status);
            Some(UpdatePayload::AircraftStatus { emergency, squawk })
        }

        _ => None,
    }
}

/// Decode aircraft category from TypeCoding and CA fields
fn decode_aircraft_category(tc: TypeCoding, ca: u8) -> AircraftCategory {
    // TypeCoding: D=1, C=2, B=3, A=4
    match tc {
        TypeCoding::A => match ca {
            0 => AircraftCategory::NoInfo,
            1 => AircraftCategory::Light,
            2 => AircraftCategory::Medium1,
            3 => AircraftCategory::Medium2,
            4 => AircraftCategory::HighVortexLarge,
            5 => AircraftCategory::Heavy,
            6 => AircraftCategory::HighPerformance,
            7 => AircraftCategory::Rotorcraft,
            _ => AircraftCategory::Unknown,
        },
        TypeCoding::B => match ca {
            0 => AircraftCategory::NoInfo,
            1 => AircraftCategory::Glider,
            2 => AircraftCategory::LighterThanAir,
            3 => AircraftCategory::Parachutist,
            4 => AircraftCategory::Ultralight,
            5 => AircraftCategory::Reserved,
            6 => AircraftCategory::UAV,
            7 => AircraftCategory::Space,
            _ => AircraftCategory::Unknown,
        },
        TypeCoding::C => match ca {
            0 => AircraftCategory::NoInfo,
            1 => AircraftCategory::SurfaceEmergency,
            2 => AircraftCategory::SurfaceService,
            3 => AircraftCategory::PointObstacle,
            4 => AircraftCategory::ClusterObstacle,
            5 => AircraftCategory::LineObstacle,
            _ => AircraftCategory::Unknown,
        },
        TypeCoding::D => AircraftCategory::Reserved,
    }
}

/// Decode surface speed from movement field
fn decode_surface_speed(mov: u8) -> Option<f64> {
    match mov {
        0 => None,                                        // No info
        1 => Some(0.0),                                   // Stopped
        2..=8 => Some((mov - 1) as f64 * 0.125),         // 0.125 to 1 kt
        9..=12 => Some(1.0 + (mov - 9) as f64 * 0.25),   // 1 to 2 kt
        13..=38 => Some(2.0 + (mov - 13) as f64 * 0.5),  // 2 to 15 kt
        39..=93 => Some(15.0 + (mov - 39) as f64),       // 15 to 70 kt
        94..=108 => Some(70.0 + (mov - 94) as f64 * 2.0), // 70 to 100 kt
        109..=123 => Some(100.0 + (mov - 109) as f64 * 5.0), // 100 to 175 kt
        124 => Some(175.0),                               // >= 175 kt
        _ => None,
    }
}

/// Decode aircraft status message (TC 28)
fn decode_aircraft_status(status: &adsb::AircraftStatus) -> (EmergencyStatus, Option<u16>) {
    use adsb::EmergencyState;

    // Emergency state is directly on the AircraftStatus struct
    let emergency = match status.emergency_state {
        EmergencyState::None => EmergencyStatus::None,
        EmergencyState::General => EmergencyStatus::General,
        EmergencyState::Lifeguard => EmergencyStatus::Medical, // Lifeguard = Medical
        EmergencyState::MinimumFuel => EmergencyStatus::MinFuel,
        EmergencyState::NoCommunication => EmergencyStatus::NoComm,
        EmergencyState::UnlawfulInterference => EmergencyStatus::Hijack,
        EmergencyState::DownedAircraft => EmergencyStatus::Downed,
        EmergencyState::Reserved2 => EmergencyStatus::Reserved,
    };

    // Squawk is also directly on the struct (u32, but represents 4-digit octal)
    let squawk = Some(status.squawk as u16);

    (emergency, squawk)
}

/// Extract velocity components from an AirborneVelocity message
fn extract_velocity(vel: &adsb::AirborneVelocity) -> (Option<f64>, Option<f64>, Option<i32>) {
    use adsb::AirborneVelocitySubType::*;

    let vertical_rate = if vel.vrate_value > 0 {
        let rate = ((vel.vrate_value as i32) - 1) * 64;
        let signed_rate = match vel.vrate_sign {
            Sign::Positive => rate,
            Sign::Negative => -rate,
        };
        Some(signed_rate)
    } else {
        None
    };

    match &vel.sub_type {
        GroundSpeedDecoding(gs) => {
            let ew_vel = gs.ew_vel as f64 - 1.0;
            let ns_vel = gs.ns_vel as f64 - 1.0;

            let ew_sign = match gs.ew_sign {
                Sign::Positive => 1.0,
                Sign::Negative => -1.0,
            };
            let ns_sign = match gs.ns_sign {
                Sign::Positive => 1.0,
                Sign::Negative => -1.0,
            };

            let vew = ew_vel * ew_sign;
            let vns = ns_vel * ns_sign;

            let ground_speed = (vew * vew + vns * vns).sqrt();
            let heading = vew.atan2(vns).to_degrees();
            let heading = if heading < 0.0 {
                heading + 360.0
            } else {
                heading
            };

            (Some(ground_speed), Some(heading), vertical_rate)
        }
        AirspeedDecoding(asp) => {
            let airspeed = if asp.airspeed > 0 {
                Some(asp.airspeed as f64 - 1.0)
            } else {
                None
            };

            let heading = if asp.status_heading == 1 {
                Some(asp.mag_heading as f64 * 360.0 / 1024.0)
            } else {
                None
            };

            (airspeed, heading, vertical_rate)
        }
        Reserved0(_) | Reserved1(_) => (None, None, vertical_rate),
    }
}

/// Format squawk code as 4-digit octal string
pub fn format_squawk(squawk: u16) -> String {
    // Squawk is stored as a decimal value representing octal digits
    // e.g., squawk value 4032 (decimal) = 7700 (octal)
    format!("{:04o}", squawk)
}

/// Check if squawk indicates emergency
pub fn is_emergency_squawk(squawk: u16) -> Option<EmergencyStatus> {
    let octal = format_squawk(squawk);
    match octal.as_str() {
        "7500" => Some(EmergencyStatus::Hijack),
        "7600" => Some(EmergencyStatus::NoComm),
        "7700" => Some(EmergencyStatus::General),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_identification() {
        let bytes: Vec<u8> = vec![
            0x8d, 0x48, 0x40, 0xd6, 0x20, 0x2c, 0xc3, 0x71, 0xc3, 0x2c, 0xe0, 0x57, 0x60, 0x98,
        ];

        let frame = BeastFrame {
            frame_type: FrameType::ModeSLong,
            timestamp: 0,
            signal_level: 0x50,
            message: bytes,
        };

        let update = decode_beast_frame(&frame);
        assert!(update.is_some());

        let update = update.unwrap();
        assert_eq!(update.icao, 0x4840d6);

        if let UpdatePayload::Identification { callsign, .. } = update.payload {
            assert_eq!(callsign, "KLM1023");
        } else {
            panic!("Expected Identification payload");
        }
    }

    #[test]
    fn test_decode_position() {
        let bytes: Vec<u8> = vec![
            0x8d, 0xa2, 0xc1, 0xbd, 0x58, 0x7b, 0xa2, 0xad, 0xb3, 0x17, 0x99, 0xcb, 0x80, 0x2b,
        ];

        let frame = BeastFrame {
            frame_type: FrameType::ModeSLong,
            timestamp: 0,
            signal_level: 0x50,
            message: bytes,
        };

        let update = decode_beast_frame(&frame);
        assert!(update.is_some());

        let update = update.unwrap();
        assert_eq!(update.icao, 0xa2c1bd);

        match update.payload {
            UpdatePayload::AirbornePositionBaro { altitude, .. } => {
                assert!(altitude.is_some());
            }
            _ => panic!("Expected AirbornePositionBaro payload"),
        }
    }

    #[test]
    fn test_format_squawk() {
        assert_eq!(format_squawk(0o7700), "7700");
        assert_eq!(format_squawk(0o7500), "7500");
        assert_eq!(format_squawk(0o7600), "7600");
        assert_eq!(format_squawk(0o1200), "1200");
    }

    #[test]
    fn test_emergency_squawk() {
        assert_eq!(is_emergency_squawk(0o7700), Some(EmergencyStatus::General));
        assert_eq!(is_emergency_squawk(0o7500), Some(EmergencyStatus::Hijack));
        assert_eq!(is_emergency_squawk(0o7600), Some(EmergencyStatus::NoComm));
        assert_eq!(is_emergency_squawk(0o1200), None);
    }

    #[test]
    fn test_df11_detection() {
        // DF 11 All-Call Reply: 5D + ICAO + parity
        let bytes: Vec<u8> = vec![0x5d, 0xa2, 0xc1, 0xbd, 0x00, 0x00, 0x00];

        let frame = BeastFrame {
            frame_type: FrameType::ModeSShort,
            timestamp: 0,
            signal_level: 0x50,
            message: bytes,
        };

        let update = decode_beast_frame(&frame);
        assert!(update.is_some());

        let update = update.unwrap();
        assert_eq!(update.icao, 0xa2c1bd);

        match update.payload {
            UpdatePayload::ModeSDetection => {}
            _ => panic!("Expected ModeSDetection payload"),
        }
    }
}
