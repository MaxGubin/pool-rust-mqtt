//! Pentair system interface.
//!

use std::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
  InvalidMessage,
  CheckSumError,
}

/// A trait that must be implemented by all structures that can decode a messages,
/// sent ower serial interface.
pub trait WireDecoder: Sized {
  fn decode(data: &[u8]) -> Result<Self, Error>;
}

/// A trait that must be implemented for all types that can be serialized to serial interface.
pub trait WireEncoder {
  fn encode(&self) -> &[u8];
}

/// The decoded package with the system state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemState {
  // Different switches, usually in the state on/off
  pub pool_on: bool,
  pub spa_on: bool,
  pub aux_circuits: Vec<bool>,
  pub feature_circuits: Vec<bool>,

  // Block of temperatures
  pub water_temp: u32,
  pub air_temp: u32,
  pub solar_temp: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Circuit {
  Spa = 0x01,
  Aux1 = 0x02,
  Aux2 = 0x03,
  Aux3 = 0x04,
  Feature1 = 0x05,
  Pool = 0x06,
  Feature2 = 0x07,
  Feature3 = 0x08,
  Feature4 = 0x09,
  HeatBoost = 0x85,
}

impl Circuit {
  pub fn from_u8(val: u8) -> Option<Self> {
    match val {
      0x01 => Some(Self::Spa),
      0x02 => Some(Self::Aux1),
      0x03 => Some(Self::Aux2),
      0x04 => Some(Self::Aux3),
      0x05 => Some(Self::Feature1),
      0x06 => Some(Self::Pool),
      0x07 => Some(Self::Feature2),
      0x08 => Some(Self::Feature3),
      0x09 => Some(Self::Feature4),
      0x85 => Some(Self::HeatBoost),
      _ => None,
    }
  }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockCalendar {
  pub hour: u8,
  pub minute: u8,
  pub day_of_week: u8,
  pub day_of_month: u8,
  pub month: u8,
  pub year: u8,
  pub clock_adjust: u8,
  pub dst: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PumpStatus {
  pub running: bool,
  pub mode: u8,
  pub state: u8,
  pub power_watts: u32,
  pub speed_rpm: u32,
  pub flow_gpm: u8,
  pub filter_percent_used: u8,
  pub error_code: u8,
  pub time_remaining_hours: u8,
  pub time_remaining_minutes: u8,
  pub clock_hours: u8,
  pub clock_minutes: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PentairMessage {
  Status(SystemState),
  CircuitChange(Circuit, bool),
  CircuitChangeResponse,
  RemoteLayoutRequest,
  RemoteLayoutResponse { row1: Circuit, row2: Circuit, row3: Circuit, row4: Circuit },
  ClockBroadcast(ClockCalendar),
  PumpStatusRequestOrProvide(PumpStatus),
}

/// Helper function to build a standard Pentair automation packet with header, lengths, and checksums.
pub fn encode_pentair_packet(
  protocol: u8,
  dest: u8,
  source: u8,
  command: u8,
  data: &[u8],
) -> Vec<u8> {
  let mut packet = Vec::with_capacity(4 + 1 + 1 + 1 + 1 + 1 + data.len() + 2);
  // Standard Preamble/Header (0xFF 0x00 0xFF 0xA5)
  packet.extend_from_slice(&[0xFF, 0x00, 0xFF, 0xA5]);
  packet.push(protocol);
  packet.push(dest);
  packet.push(source);
  packet.push(command);
  packet.push(data.len() as u8);
  packet.extend_from_slice(data);

  // Checksum is calculated by summing all bytes starting from the 0xA5 byte (index 3)
  let mut sum: u32 = 0;
  for &b in &packet[3..] {
    sum += b as u32;
  }
  let checksum = sum as u16;
  packet.push((checksum >> 8) as u8);
  packet.push((checksum & 0xFF) as u8);
  packet
}

impl PentairMessage {
  pub fn to_bytes(&self) -> Vec<u8> {
    match self {
      PentairMessage::Status(state) => {
        let mut data = vec![0u8; 29];
        // Time defaults
        data[0] = 12;
        data[1] = 0;

        // Base switches bitmask
        let mut mask = 0u8;
        if state.pool_on {
          mask |= 0x20;
        }
        if state.spa_on {
          mask |= 0x01;
        }
        if state.aux_circuits.len() > 0 && state.aux_circuits[0] {
          mask |= 0x02;
        }
        if state.aux_circuits.len() > 1 && state.aux_circuits[1] {
          mask |= 0x04;
        }
        if state.aux_circuits.len() > 2 && state.aux_circuits[2] {
          mask |= 0x08;
        }
        if state.feature_circuits.len() > 0 && state.feature_circuits[0] {
          mask |= 0x10;
        }
        if state.feature_circuits.len() > 1 && state.feature_circuits[1] {
          mask |= 0x40;
        }
        if state.feature_circuits.len() > 2 && state.feature_circuits[2] {
          mask |= 0x80;
        }
        data[2] = mask;

        // Additional switches bitmask
        if state.feature_circuits.len() > 3 && state.feature_circuits[3] {
          data[3] |= 0x01;
        }

        // Temperatures
        data[14] = state.water_temp as u8;
        data[18] = state.air_temp as u8;
        data[19] = state.solar_temp as u8;

        // Version info (e.g. 2.070 FW version default)
        data[16] = 0x02;
        data[17] = 70;

        encode_pentair_packet(0x01, 0x0f, 0x10, 0x02, &data)
      }
      PentairMessage::CircuitChange(circuit, state) => {
        let data = vec![*circuit as u8, if *state { 0x01 } else { 0x00 }];
        encode_pentair_packet(0x01, 0x10, 0x48, 0x86, &data)
      }
      PentairMessage::CircuitChangeResponse => {
        let data = vec![0x86];
        encode_pentair_packet(0x01, 0x48, 0x10, 0x01, &data)
      }
      PentairMessage::RemoteLayoutRequest => {
        let data = vec![0x01];
        encode_pentair_packet(0x01, 0x10, 0x48, 0xE1, &data)
      }
      PentairMessage::RemoteLayoutResponse { row1, row2, row3, row4 } => {
        let data = vec![*row1 as u8, *row2 as u8, *row3 as u8, *row4 as u8];
        encode_pentair_packet(0x01, 0x0f, 0x10, 0x21, &data)
      }
      PentairMessage::ClockBroadcast(clock) => {
        let data = vec![
          clock.hour,
          clock.minute,
          clock.day_of_week,
          clock.day_of_month,
          clock.month,
          clock.year,
          clock.clock_adjust,
          clock.dst,
        ];
        encode_pentair_packet(0x01, 0x0f, 0x10, 0x05, &data)
      }
      PentairMessage::PumpStatusRequestOrProvide(pump) => {
        let power_high = (pump.power_watts >> 8) as u8;
        let power_low = (pump.power_watts & 0xFF) as u8;
        let speed_high = (pump.speed_rpm >> 8) as u8;
        let speed_low = (pump.speed_rpm & 0xFF) as u8;
        let data = vec![
          if pump.running { 0x0a } else { 0x04 },
          pump.mode,
          pump.state,
          power_high,
          power_low,
          speed_high,
          speed_low,
          pump.flow_gpm,
          pump.filter_percent_used,
          0,
          pump.error_code,
          pump.time_remaining_hours,
          pump.time_remaining_minutes,
          pump.clock_hours,
          pump.clock_minutes,
        ];
        encode_pentair_packet(0x00, 0x10, 0x60, 0x07, &data)
      }
    }
  }
}

pub struct PentairMessageFrame {
  pub message: PentairMessage,
  encoded: Vec<u8>,
}

impl PentairMessageFrame {
  pub fn new(message: PentairMessage) -> Self {
    let encoded = message.to_bytes();
    Self { message, encoded }
  }
}

impl WireEncoder for PentairMessageFrame {
  fn encode(&self) -> &[u8] {
    &self.encoded
  }
}

pub struct SystemStateFrame {
  pub state: SystemState,
  encoded: Vec<u8>,
}

impl SystemStateFrame {
  pub fn new(state: SystemState) -> Self {
    let encoded = PentairMessage::Status(state.clone()).to_bytes();
    Self { state, encoded }
  }
}

impl WireEncoder for SystemStateFrame {
  fn encode(&self) -> &[u8] {
    &self.encoded
  }
}

impl WireDecoder for SystemState {
  fn decode(packet: &[u8]) -> Result<Self, Error> {
    if packet.len() < 8 {
      return Err(Error::InvalidMessage);
    }

    let mut state = Self {
      pool_on: false,
      spa_on: false,
      aux_circuits: Vec::new(),
      feature_circuits: Vec::new(),
      water_temp: 0,
      air_temp: 0,
      solar_temp: 0,
    };

    const MASK_IDX: usize = 7;
    const SPA_MASK: u8 = 0x01;
    const AUX1_MASK: u8 = 0x02;
    const AUX2_MASK: u8 = 0x04;
    const AUX3_MASK: u8 = 0x08;
    const POOL_MASK: u8 = 0x20;
    const FEATURE1_MASK: u8 = 0x10;
    const FEATURE2_MASK: u8 = 0x40;
    const FEATURE3_MASK: u8 = 0x80;

    {
      state.pool_on = (packet[MASK_IDX] & POOL_MASK) != 0;
      state.spa_on = (packet[MASK_IDX] & SPA_MASK) != 0;
      state.aux_circuits.push((packet[MASK_IDX] & AUX1_MASK) != 0);
      state.aux_circuits.push((packet[MASK_IDX] & AUX2_MASK) != 0);
      state.aux_circuits.push((packet[MASK_IDX] & AUX3_MASK) != 0);
      state.feature_circuits.push((packet[MASK_IDX] & FEATURE1_MASK) != 0);
      state.feature_circuits.push((packet[MASK_IDX] & FEATURE2_MASK) != 0);
      state.feature_circuits.push((packet[MASK_IDX] & FEATURE3_MASK) != 0);
    }

    // Additional switches: FEATURE4 is at bit 0 of DATA[3] (index 8 of packet)
    if packet.len() >= 9 {
      state.feature_circuits.push((packet[8] & 0x01) != 0);
    }

    // Temperatures
    if packet.len() >= 25 {
      state.water_temp = packet[19] as u32;
      state.air_temp = packet[23] as u32;
      state.solar_temp = packet[24] as u32;
    }

    Ok(state)
  }
}

impl WireDecoder for PentairMessage {
  // Decodes a message ffrom the buffer:
  fn decode(data: &[u8]) -> Result<Self, Error> {
    let mut idx = 0;
    while idx < data.len() {
      // Skip until start og the package.
      if data[idx] == 0xA5 {
        if idx + 6 <= data.len() {
          let dlen = data[idx + 5] as usize;
          let total_len = 6 + dlen + 2;
          if idx + total_len <= data.len() {
            let packet_slice = &data[idx..idx + total_len];
            let mut sum: u32 = 0;
            for &b in &packet_slice[..6 + dlen] {
              sum += b as u32;
            }
            let expected_chk =
              ((packet_slice[6 + dlen] as u16) << 8) | (packet_slice[6 + dlen + 1] as u16);
            if sum as u16 == expected_chk {
              let command = packet_slice[4];
              let payload = &packet_slice[6..6 + dlen];

              let msg = match command {
                0x02 => {
                  let state = SystemState::decode(&packet_slice[1..6 + dlen])?;
                  PentairMessage::Status(state)
                }
                0x86 => {
                  if dlen < 2 {
                    return Err(Error::InvalidMessage);
                  }
                  let circuit = Circuit::from_u8(payload[0]).ok_or(Error::InvalidMessage)?;
                  let state = payload[1] != 0;
                  PentairMessage::CircuitChange(circuit, state)
                }
                0x01 => PentairMessage::CircuitChangeResponse,
                0xE1 => PentairMessage::RemoteLayoutRequest,
                0x21 => {
                  if dlen < 4 {
                    return Err(Error::InvalidMessage);
                  }
                  let row1 = Circuit::from_u8(payload[0]).ok_or(Error::InvalidMessage)?;
                  let row2 = Circuit::from_u8(payload[1]).ok_or(Error::InvalidMessage)?;
                  let row3 = Circuit::from_u8(payload[2]).ok_or(Error::InvalidMessage)?;
                  let row4 = Circuit::from_u8(payload[3]).ok_or(Error::InvalidMessage)?;
                  PentairMessage::RemoteLayoutResponse { row1, row2, row3, row4 }
                }
                0x05 => {
                  if dlen < 8 {
                    return Err(Error::InvalidMessage);
                  }
                  PentairMessage::ClockBroadcast(ClockCalendar {
                    hour: payload[0],
                    minute: payload[1],
                    day_of_week: payload[2],
                    day_of_month: payload[3],
                    month: payload[4],
                    year: payload[5],
                    clock_adjust: payload[6],
                    dst: payload[7],
                  })
                }
                0x07 => {
                  if dlen < 15 {
                    return Err(Error::InvalidMessage);
                  }
                  PentairMessage::PumpStatusRequestOrProvide(PumpStatus {
                    running: payload[0] == 0x0a,
                    mode: payload[1],
                    state: payload[2],
                    power_watts: ((payload[3] as u32) << 8) | (payload[4] as u32),
                    speed_rpm: ((payload[5] as u32) << 8) | (payload[6] as u32),
                    flow_gpm: payload[7],
                    filter_percent_used: payload[8],
                    error_code: payload[10],
                    time_remaining_hours: payload[11],
                    time_remaining_minutes: payload[12],
                    clock_hours: payload[13],
                    clock_minutes: payload[14],
                  })
                }
                _ => {
                  return Err(Error::InvalidMessage);
                }
              };

              return Ok(msg);
            }
          }
        }
      }
      idx += 1;
    }
    Err(Error::InvalidMessage)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_system_state_from_packet() {
    let packet = vec![
      0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x2a, 0x00, 0x00, 0x00, 0x00, 0x3C, 0x00, 0x00,
      0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x45, 0x00, 0x00, 0x00, 0x00, 0x00,
      0x00, 0x00, 0x53, 0x00, 0x00, 0x00, 0x64, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
      0x00, 0x00, 0x00, 0x54, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x5F, 0x00, 0x00, 0x00,
      0x3C, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x0b, 0x00, 0x00, 0x00, 0xf4, 0x01, 0x00,
      0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xf5, 0x01, 0x00, 0x00, 0x00, 0x00,
      0x00, 0x00, 0x00, 0x00, 0x00,
    ];

    let state = SystemState::decode(&packet).unwrap().0;
    assert!(state.pool_on);
    assert!(!state.spa_on);
    assert!(state.aux_circuits[0]);
    assert!(!state.aux_circuits[1]);
    assert!(state.aux_circuits[2]);
    assert!(!state.feature_circuits[0]);
    assert!(!state.feature_circuits[1]);
    assert!(!state.feature_circuits[2]);
  }

  #[test]
  fn test_circuit_change_roundtrip() {
    let msg = PentairMessage::CircuitChange(Circuit::Pool, true);
    let frame = PentairMessageFrame::new(msg.clone());
    let bytes = frame.encode();

    // Verify preamble is present
    assert_eq!(&bytes[..4], &[0xFF, 0x00, 0xFF, 0xA5]);

    // Decode message
    let (decoded_msg, remaining) = PentairMessage::decode(bytes).unwrap();
    assert_eq!(msg, decoded_msg);
    assert_eq!(remaining.len(), 0);
  }

  #[test]
  fn test_clock_broadcast_roundtrip() {
    let clock = ClockCalendar {
      hour: 14,
      minute: 35,
      day_of_week: 4, // Tue
      day_of_month: 5,
      month: 8,
      year: 26,
      clock_adjust: 0,
      dst: 1,
    };
    let msg = PentairMessage::ClockBroadcast(clock);
    let frame = PentairMessageFrame::new(msg.clone());
    let bytes = frame.encode();

    let (decoded_msg, remaining) = PentairMessage::decode(bytes).unwrap();
    assert_eq!(msg, decoded_msg);
    assert_eq!(remaining.len(), 0);
  }

  #[test]
  fn test_pump_status_roundtrip() {
    let pump = PumpStatus {
      running: true,
      mode: 0,
      state: 2,
      power_watts: 320,
      speed_rpm: 1550,
      flow_gpm: 45,
      filter_percent_used: 12,
      error_code: 0,
      time_remaining_hours: 3,
      time_remaining_minutes: 15,
      clock_hours: 14,
      clock_minutes: 35,
    };
    let msg = PentairMessage::PumpStatusRequestOrProvide(pump);
    let frame = PentairMessageFrame::new(msg.clone());
    let bytes = frame.encode();

    let (decoded_msg, remaining) = PentairMessage::decode(bytes).unwrap();
    assert_eq!(msg, decoded_msg);
    assert_eq!(remaining.len(), 0);
  }
}
