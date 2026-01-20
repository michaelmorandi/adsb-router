# ADS-B Router

ADS-B Plane Tracking Service - Connects to multiple dump1090-fa instances, consolidates aircraft data, and exposes it via gRPC.

## Features

- **Multi-source support**: Connect to multiple dump1090-fa receivers simultaneously
- **Message deduplication**: Intelligently deduplicates messages across sources
- **Automatic cleanup**: Removes stale aircraft (3 minute timeout)
- **gRPC streaming API**: Real-time updates via gRPC
- **MLAT detection**: Automatically detects and flags MLAT positions
- **CPR position decoding**: Full support for global and local CPR position decoding
- **Hex code filtering**: Configure aircraft to ignore by ICAO hex code
- **Test mode**: Built-in synthetic data generator for testing

## Usage

### Basic Usage

```bash
# Connect to a single dump1090-fa instance
adsb-router --source 192.168.1.100:30005

# Connect to multiple instances
adsb-router --source 192.168.1.100 --source 192.168.1.101

# Use a config file
adsb-router --config config.toml
```

### Test Mode

Test mode generates synthetic aircraft data for testing and demonstration purposes without requiring actual ADS-B receivers.

```bash
# Run in test mode
adsb-router --test-mode

# Run in test mode with custom gRPC port
adsb-router --test-mode --grpc-addr 0.0.0.0:50052
```

#### Synthetic Aircraft Details

Test mode generates 16 aircraft of various types flying circular patterns over Switzerland:

**Category Set A (Surface/Airborne):**

| Category | Count | Callsigns | Circle Diameter | Speed | Altitude Range | Climb Rate |
|----------|-------|-----------|-----------------|-------|----------------|------------|
| Heavy | 2 | SWR100, SWR200 | 100 km | 450-460 kts | 28,000-38,000 ft | 1400-1500 fpm |
| Medium2 | 2 | EZY101, EZY202 | 60 km | 380-390 kts | 20,000-30,000 ft | 1700-1800 fpm |
| Medium1 | 1 | BEE101 | 40 km | 280 kts | 15,000-22,000 ft | 1200 fpm |
| Light | 2 | HB001, HB002 | 20 km | 160-180 kts | 4,000-12,000 ft | 700-800 fpm |
| High Performance | 1 | EAGLE1 | 50 km | 520 kts | 25,000-40,000 ft | 3000 fpm |
| Rotorcraft | 1 | HELI1 | 10 km | 120 kts | 1,000-5,000 ft | 500 fpm |

**Category Set B (Special Use):**

| Category | Count | Callsigns | Circle Diameter | Speed | Altitude Range | Climb Rate |
|----------|-------|-----------|-----------------|-------|----------------|------------|
| Glider | 1 | GLIDE1 | 30 km | 90 kts | 3,000-15,000 ft | 300 fpm |
| UAV | 1 | DRONE1 | 10 km | 50 kts | 500-3,000 ft | 400 fpm |
| Ultralight | 1 | UL001 | 15 km | 70 kts | 2,000-8,000 ft | 500 fpm |
| LighterThanAir | 2 | BALN1, BALN2 | 3-4 km | 12-15 kts | 800-4,000 ft | 180-200 fpm |
| Parachutist | 1 | JUMP1 | 2 km | 25 kts | 1,500-8,000 ft | 600 fpm |
| Space | 1 | SPACE1 | 185 km | 800 kts | 50,000-80,000 ft | 5000 fpm |

All aircraft follow this altitude cycle:
1. **Climb** from min to max altitude at specified climb rate
2. **Cruise** at max altitude for 60 seconds
3. **Descend** to min altitude at same rate
4. **Cruise** at min altitude for 60 seconds
5. Repeat

The aircraft are distributed across Switzerland and their circular paths may overlap, simulating real-world traffic patterns.

### Configuration

Create a `config.toml` file based on `config.example.toml`:

```toml
# gRPC server configuration
[grpc]
listen_addr = "0.0.0.0:50051"
update_interval_ms = 500

# Logging configuration
[logging]
level = "info"

# Filter configuration
[filters]
# Ignore specific aircraft by ICAO hex code
ignore_hex_codes = ["445566", "AABBCC"]

# ADS-B data sources
[[sources]]
host = "192.168.1.100"
port = 30005
name = "Primary Receiver"

[[sources]]
host = "192.168.1.101"
port = 30005
name = "Secondary Receiver"
```

### Command Line Options

```
Options:
  -c, --config <CONFIG>              Path to TOML configuration file
  -s, --source <SOURCES>             ADS-B sources to connect to (format: host:port or host for default port 30005)
  -g, --grpc-addr <GRPC_ADDR>        gRPC server listen address [default: 0.0.0.0:50051]
  -l, --log-level <LOG_LEVEL>        Log level (trace, debug, info, warn, error) [default: info]
      --test-mode                    Enable test mode with synthetic aircraft data
  -h, --help                         Print help
  -V, --version                      Print version
```

## Building

```bash
# Debug build
cargo build

# Release build
cargo build --release

# Run tests
cargo test
```

## gRPC API

The service exposes a gRPC API for accessing aircraft data. See `proto/adsb.proto` for the full API definition.

### Example: Streaming Updates

```bash
# Using grpcurl
grpcurl -plaintext localhost:50051 adsb.AdsbService/StreamUpdates
```

## Architecture

- **Beast Protocol Decoder** (`beast.rs`): Decodes Beast binary protocol from dump1090-fa
- **ADS-B Message Decoder** (`adsb.rs`): Extracts aircraft information from ADS-B messages
- **State Manager** (`state.rs`): Maintains consolidated aircraft state with CPR position decoding
- **Connection Manager** (`connection.rs`): Manages TCP connections to multiple receivers with auto-reconnect
- **gRPC Service** (`grpc.rs`): Exposes aircraft data via gRPC
- **Synthetic Generator** (`synthetic.rs`): Generates realistic test data

## License

See LICENSE file for details.
