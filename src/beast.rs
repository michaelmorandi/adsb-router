//! Beast binary protocol decoder
//!
//! Implements decoding of the Mode-S Beast binary format used by dump1090-fa
//! on port 30005. The format uses 0x1a as escape character.
//!
//! Frame format:
//! - 0x1a 0x31: Mode-AC (2 byte message)
//! - 0x1a 0x32: Mode-S short (7 byte message)
//! - 0x1a 0x33: Mode-S long (14 byte message)
//!
//! Each frame contains:
//! - 6 bytes MLAT timestamp (big-endian 12MHz counter)
//! - 1 byte signal level (RSSI)
//! - N bytes message data
//!
//! Escape sequences: 0x1a 0x1a represents a literal 0x1a in the data

use std::fmt;

/// Frame type indicator from Beast protocol
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameType {
    /// Mode A/C frame (2 byte message)
    ModeAC,
    /// Mode-S short frame (7 byte message)
    ModeSShort,
    /// Mode-S long frame (14 byte message) - contains ADS-B
    ModeSLong,
}

impl FrameType {
    /// Returns the message length for this frame type
    pub fn message_len(&self) -> usize {
        match self {
            FrameType::ModeAC => 2,
            FrameType::ModeSShort => 7,
            FrameType::ModeSLong => 14,
        }
    }

    /// Parse frame type from type byte
    fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0x31 => Some(FrameType::ModeAC),
            0x32 => Some(FrameType::ModeSShort),
            0x33 => Some(FrameType::ModeSLong),
            _ => None,
        }
    }
}

/// MLAT magic timestamp value (FF 00 "MLAT")
const MLAT_MAGIC_TIMESTAMP: u64 = 0xFF004D4C4154;

/// A decoded Beast frame
#[derive(Clone)]
pub struct BeastFrame {
    /// Type of frame
    pub frame_type: FrameType,
    /// MLAT timestamp (12MHz counter, 6 bytes big-endian)
    /// Special value 0xFF004D4C4154 indicates MLAT-derived position
    pub timestamp: u64,
    /// Signal level (RSSI), 0xFF means unavailable
    pub signal_level: u8,
    /// Raw message bytes
    pub message: Vec<u8>,
}

impl BeastFrame {
    /// Check if this frame has the MLAT magic timestamp
    pub fn is_mlat(&self) -> bool {
        self.timestamp == MLAT_MAGIC_TIMESTAMP
    }
}

impl fmt::Debug for BeastFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BeastFrame")
            .field("frame_type", &self.frame_type)
            .field("timestamp", &self.timestamp)
            .field("signal_level", &self.signal_level)
            .field("message", &hex::encode(&self.message))
            .finish()
    }
}

/// Simple hex encoding (avoiding external dependency)
mod hex {
    pub fn encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{:02x}", b)).collect()
    }
}

/// State machine for Beast protocol decoding
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecoderState {
    /// Waiting for escape byte (0x1a)
    WaitingForEscape,
    /// Got escape, waiting for frame type
    WaitingForType,
    /// Reading timestamp bytes
    ReadingTimestamp,
    /// Reading signal level
    ReadingSignal,
    /// Reading message bytes
    ReadingMessage,
}

/// Beast protocol decoder with streaming support
///
/// Handles escape sequences and can process partial data across multiple
/// calls to `decode()`.
pub struct BeastDecoder {
    state: DecoderState,
    frame_type: Option<FrameType>,
    timestamp_bytes: Vec<u8>,
    signal_level: u8,
    message_bytes: Vec<u8>,
    escape_pending: bool,
}

impl Default for BeastDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl BeastDecoder {
    /// Create a new decoder
    pub fn new() -> Self {
        Self {
            state: DecoderState::WaitingForEscape,
            frame_type: None,
            timestamp_bytes: Vec::with_capacity(6),
            signal_level: 0,
            message_bytes: Vec::with_capacity(14),
            escape_pending: false,
        }
    }

    /// Reset the decoder state
    fn reset(&mut self) {
        self.state = DecoderState::WaitingForEscape;
        self.frame_type = None;
        self.timestamp_bytes.clear();
        self.signal_level = 0;
        self.message_bytes.clear();
        self.escape_pending = false;
    }

    /// Decode bytes from the input buffer
    ///
    /// Returns a vector of complete frames found in the input.
    /// Incomplete frames are buffered internally for the next call.
    pub fn decode(&mut self, data: &[u8]) -> Vec<BeastFrame> {
        let mut frames = Vec::new();
        let mut i = 0;

        while i < data.len() {
            let byte = data[i];
            i += 1;

            // Handle escape sequences in data-reading states
            if self.escape_pending {
                self.escape_pending = false;
                if byte == 0x1a {
                    // Escaped 0x1a - this is a literal 0x1a in the data
                    if !self.process_data_byte(0x1a, &mut frames) {
                        self.reset();
                    }
                    continue;
                } else if let Some(frame_type) = FrameType::from_byte(byte) {
                    // This is a new frame start - abort current frame and start new one
                    self.reset();
                    self.frame_type = Some(frame_type);
                    self.state = DecoderState::ReadingTimestamp;
                    continue;
                } else {
                    // Invalid sequence after escape - reset
                    self.reset();
                    continue;
                }
            }

            match self.state {
                DecoderState::WaitingForEscape => {
                    if byte == 0x1a {
                        self.state = DecoderState::WaitingForType;
                    }
                    // Ignore non-escape bytes while waiting
                }
                DecoderState::WaitingForType => {
                    if byte == 0x1a {
                        // Double escape at frame start - stay in WaitingForType
                        // (unlikely but handle it)
                        continue;
                    }
                    if let Some(frame_type) = FrameType::from_byte(byte) {
                        self.frame_type = Some(frame_type);
                        self.state = DecoderState::ReadingTimestamp;
                    } else {
                        // Unknown frame type - go back to waiting
                        self.state = DecoderState::WaitingForEscape;
                    }
                }
                DecoderState::ReadingTimestamp
                | DecoderState::ReadingSignal
                | DecoderState::ReadingMessage => {
                    if byte == 0x1a {
                        // Potential escape sequence
                        self.escape_pending = true;
                    } else if !self.process_data_byte(byte, &mut frames) {
                        self.reset();
                    }
                }
            }
        }

        frames
    }

    /// Process a data byte (after escape handling)
    /// Returns false if state should be reset
    fn process_data_byte(&mut self, byte: u8, frames: &mut Vec<BeastFrame>) -> bool {
        match self.state {
            DecoderState::ReadingTimestamp => {
                self.timestamp_bytes.push(byte);
                if self.timestamp_bytes.len() == 6 {
                    self.state = DecoderState::ReadingSignal;
                }
                true
            }
            DecoderState::ReadingSignal => {
                self.signal_level = byte;
                self.state = DecoderState::ReadingMessage;
                true
            }
            DecoderState::ReadingMessage => {
                let expected_len = self.frame_type.map(|t| t.message_len()).unwrap_or(0);
                self.message_bytes.push(byte);

                if self.message_bytes.len() == expected_len {
                    // Frame complete
                    let timestamp = self
                        .timestamp_bytes
                        .iter()
                        .fold(0u64, |acc, &b| (acc << 8) | b as u64);

                    frames.push(BeastFrame {
                        frame_type: self.frame_type.unwrap(),
                        timestamp,
                        signal_level: self.signal_level,
                        message: std::mem::take(&mut self.message_bytes),
                    });

                    // Reset for next frame
                    self.state = DecoderState::WaitingForEscape;
                    self.frame_type = None;
                    self.timestamp_bytes.clear();
                    self.signal_level = 0;
                }
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_modes_long_frame() {
        let mut decoder = BeastDecoder::new();

        // Example Mode-S long frame (0x1a 0x33 + 6 byte timestamp + 1 byte signal + 14 byte message)
        // No escape sequences in this simple test
        let data: Vec<u8> = vec![
            0x1a, 0x33, // Escape + Mode-S long
            0x01, 0x71, 0x03, 0x40, 0x2d, 0xe6, // Timestamp
            0x3f, // Signal level
            0xa0, 0x00, 0x15, 0xb8, 0x90, 0x00, 0x2d, // Message part 1 (7 bytes)
            0x32, 0x20, 0x0c, 0x00, 0x94, 0xf4, 0xab, // Message part 2 (7 bytes)
        ];

        let frames = decoder.decode(&data);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].frame_type, FrameType::ModeSLong);
        assert_eq!(frames[0].message.len(), 14);
    }

    #[test]
    fn test_decode_multiple_frames() {
        let mut decoder = BeastDecoder::new();

        // Two frames back to back
        let data: Vec<u8> = vec![
            // Frame 1: Mode-S short
            0x1a, 0x32, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // Escape + type + timestamp
            0x50, // Signal
            0x8d, 0x40, 0x62, 0x1d, 0x58, 0xc3, 0x86, // 7-byte message
            // Frame 2: Mode-S long
            0x1a, 0x33, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, // Escape + type + timestamp
            0x60, // Signal
            0x8d, 0x48, 0x40, 0xd6, 0x20, 0x2c, 0xc3, 0x71, 0xc3, 0x2c, 0xe0, 0x57, 0x60,
            0x98, // 14-byte message
        ];

        let frames = decoder.decode(&data);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].frame_type, FrameType::ModeSShort);
        assert_eq!(frames[0].message.len(), 7);
        assert_eq!(frames[1].frame_type, FrameType::ModeSLong);
        assert_eq!(frames[1].message.len(), 14);
    }

    #[test]
    fn test_partial_frame_handling() {
        let mut decoder = BeastDecoder::new();

        // First part of a frame
        let part1: Vec<u8> = vec![
            0x1a, 0x33, // Escape + Mode-S long
            0x00, 0x00, 0x00, // Partial timestamp
        ];

        let frames1 = decoder.decode(&part1);
        assert!(frames1.is_empty());

        // Rest of the frame
        let part2: Vec<u8> = vec![
            0x00, 0x00, 0x00, // Rest of timestamp
            0x40, // Signal
            0x8d, 0x48, 0x40, 0xd6, 0x20, 0x2c, 0xc3, 0x71, 0xc3, 0x2c, 0xe0, 0x57, 0x60,
            0x98, // Message
        ];

        let frames2 = decoder.decode(&part2);
        assert_eq!(frames2.len(), 1);
        assert_eq!(frames2[0].frame_type, FrameType::ModeSLong);
    }

    #[test]
    fn test_escape_in_data() {
        let mut decoder = BeastDecoder::new();

        // Frame with 0x1a in the message data (escaped as 0x1a 0x1a)
        // Wire format: 8 bytes for message (including escape), decodes to 7 bytes
        let data: Vec<u8> = vec![
            0x1a, 0x32, // Mode-S short
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // Timestamp
            0x50, // Signal
            0x8d, 0x40, 0x1a, 0x1a, 0x1d, 0x58, 0xc3, 0x86, // Message with escaped 0x1a (8 bytes wire, 7 decoded)
        ];

        let frames = decoder.decode(&data);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].message.len(), 7);
        assert_eq!(frames[0].message[2], 0x1a); // The escaped byte
    }
}
