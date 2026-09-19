use esp_idf_svc::{
  hal::uart::UartDriver,
  mqtt::client::{EspMqttClient, QoS},
  sys::{EspError, ESP_ERR_TIMEOUT},
};
use log::*;
use std::{
  collections::VecDeque,
  sync::{Arc, Mutex},
  time::{Duration, Instant},
};
use thiserror::Error;

use crate::pentair::{self, PentairMessage, WireDecoder};

// --- Pentair RS-485 Protocol & Framing Constants ---

/// The standard magic sequence starting a valid Pentair packet.
const HEADER: [u8; 4] = [0xFF, 0x00, 0xFF, 0xA5];

// --- Hardware & UART Driver Constants ---

/// Timeout in FreeRTOS ticks for blocking UART read calls (approx. 50ms at 100Hz tick rate).
const UART_READ_TIMEOUT_TICKS: u32 = 5;

/// How many consecutive read timeouts we tolerate in the middle of a packet before giving up
/// on it (~2s at `UART_READ_TIMEOUT_TICKS` per attempt). Guards against a sender that stops
/// transmitting partway through a frame.
const MAX_MID_PACKET_RETRIES: u32 = 40;

// --- Outgoing Reliable Message State Machine Constants ---

/// Timeout before triggering command re-transmission when awaiting an ACK.
const ACK_RETRY_TIMEOUT: Duration = Duration::from_millis(500);

/// Maximum number of transmission retries before aborting command execution.
const MAX_RETRY_COUNT: u32 = 10;

/// Period to sleep during each iteration of the main loop.
const WORKER_LOOP_SLEEP: Duration = Duration::from_millis(10);

/// Errors that can occur while reading and decoding Pentair protocol traffic off the UART.
#[derive(Debug, Error)]
pub enum PentairError {
  #[error("invalid Pentair message")]
  InvalidMessage,
  #[error("checksum mismatch")]
  CrcError,
  #[error("timed out waiting for more bytes mid-packet")]
  Timeout,
  #[error("UART error: {0}")]
  Uart(#[from] EspError),
}

impl From<pentair::Error> for PentairError {
  fn from(err: pentair::Error) -> Self {
    match err {
      pentair::Error::InvalidMessage => PentairError::InvalidMessage,
      pentair::Error::CheckSumError => PentairError::CrcError,
    }
  }
}

/// A thread-safe queue of outgoing Pentair messages.
#[derive(Clone)]
pub struct MessageQueue {
  queue: Arc<Mutex<VecDeque<PentairMessage>>>,
}

impl MessageQueue {
  pub fn new() -> Self {
    Self { queue: Arc::new(Mutex::new(VecDeque::new())) }
  }

  pub fn push(&self, msg: PentairMessage) {
    let mut q = self.queue.lock().unwrap();
    q.push_back(msg);
  }

  pub fn pop(&self) -> Option<PentairMessage> {
    let mut q = self.queue.lock().unwrap();
    q.pop_front()
  }
}

/// Status from processing serial input.
#[derive(PartialEq)]
enum HeaderScan {
  BusAvailable,
  GoodHeader,
}

/// Reads a single byte from the UART with a short timeout.
///
/// Returns `Ok(None)` when nothing arrived within `UART_READ_TIMEOUT_TICKS` (not an error - the
/// bus is simply idle), and `Err` only for a genuine UART driver failure.
fn read_byte(uart: &UartDriver<'static>) -> Result<Option<u8>, PentairError> {
  let mut byte = [0u8; 1];
  match uart.read(&mut byte, UART_READ_TIMEOUT_TICKS) {
    Ok(0) => Ok(None),
    Ok(_) => Ok(Some(byte[0])),
    Err(err) if err.code() == ESP_ERR_TIMEOUT => Ok(None),
    Err(err) => Err(err.into()),
  }
}

/// Like `read_byte`, but for use once we're committed to a packet: a timeout here means the
/// sender stalled mid-frame, so we retry a bounded number of times before giving up.
fn read_byte_required(uart: &UartDriver<'static>) -> Result<u8, PentairError> {
  for _ in 0..MAX_MID_PACKET_RETRIES {
    if let Some(byte) = read_byte(uart)? {
      return Ok(byte);
    }
  }
  Err(PentairError::Timeout)
}

/// Reads from the UART until a well-formed header is found, or reports that the bus is idle.
fn scan_for_header(uart: &UartDriver<'static>) -> Result<HeaderScan, PentairError> {
  let mut buffer = Vec::with_capacity(HEADER.len());
  loop {
    let byte = match read_byte(uart)? {
      Some(byte) => byte,
      None if buffer.is_empty() => return Ok(HeaderScan::BusAvailable),
      None => continue, // Mid-header: keep waiting for the rest of it.
    };
    debug!("Read byte {:?} from the port", byte);
    buffer.push(byte);
    if buffer.len() == HEADER.len() {
      if buffer == HEADER {
        return Ok(HeaderScan::GoodHeader);
      }
      buffer.remove(0);
    }
  }
}

/// Reads the remainder of a packet (past the header) and validates its checksum.
fn read_packet(uart: &UartDriver<'static>) -> Result<Vec<u8>, PentairError> {
  const USUAL_PACKET_SIZE: usize = 32;
  let mut buffer: Vec<u8> = Vec::with_capacity(USUAL_PACKET_SIZE);

  // Protocol, destination, source, command, length.
  for _ in 0..5 {
    buffer.push(read_byte_required(uart)?);
  }
  let to_read_len = buffer[4] as usize;
  for _ in 0..to_read_len {
    buffer.push(read_byte_required(uart)?);
  }

  // Checksum: two big-endian bytes following the payload.
  let checksum_hi = read_byte_required(uart)? as u16;
  let checksum_lo = read_byte_required(uart)? as u16;
  let mut checksum = (checksum_hi << 8) | checksum_lo;
  checksum = checksum.wrapping_sub(0xa5); // The header's trailing 0xA5 byte counts toward the sender's checksum.
  for &b in buffer.iter() {
    checksum = checksum.wrapping_sub(b as u16);
  }
  debug!("Checksum remainder: {}", checksum);
  if checksum != 0 {
    return Err(PentairError::CrcError);
  }

  Ok(buffer)
}

fn process_package(buffer: &[u8]) -> Result<PentairMessage, PentairError> {
  Ok(PentairMessage::decode(buffer)?)
}

/// Handles a successfully decoded incoming message: publishes state to MQTT and advances the
/// outgoing ACK state machine.
fn handle_message(
  msg: &PentairMessage,
  mqtt_client: &Arc<Mutex<EspMqttClient<'static>>>,
  state: &mut UartState,
) {
  match msg {
    PentairMessage::Status(system_state) => {
      let payload = format!(
        r#"{{"pool_on":{},"spa_on":{},"water_temp":{},"air_temp":{},"solar_temp":{}}}"#,
        system_state.pool_on,
        system_state.spa_on,
        system_state.water_temp,
        system_state.air_temp,
        system_state.solar_temp
      );
      info!("Publishing state update to MQTT: {}", payload);
      if let Err(err) =
        mqtt_client.lock().unwrap().publish("pool/state", QoS::AtMostOnce, false, payload.as_bytes())
      {
        error!("Failed to publish pool state to MQTT: {:?}", err);
      }
    }
    PentairMessage::CircuitChangeResponse => {
      info!("Received CircuitChangeResponse acknowledgment!");
      if let UartState::AwaitingAck { .. } = state {
        *state = UartState::Idle;
      }
    }
    _ => {
      debug!("Received other Pentair message: {:?}", msg);
    }
  }
}

/// Sends the next outgoing message when idle, or manages ACK retries/timeouts.
fn manage_outgoing(uart: &UartDriver<'static>, outgoing_queue: &MessageQueue, state: &mut UartState) {
  match state {
    UartState::Idle => {
      if let Some(msg) = outgoing_queue.pop() {
        info!("Sending message to UART: {:?}", msg);
        let bytes = msg.to_bytes();
        if let Err(err) = uart.write(&bytes) {
          error!("Failed to write to UART: {:?}", err);
          return;
        }

        if let PentairMessage::CircuitChange(..) = msg {
          *state = UartState::AwaitingAck { message: msg, last_sent: Instant::now(), retry_count: 0 };
        }
      }
    }
    UartState::AwaitingAck { message, last_sent, retry_count } => {
      if last_sent.elapsed() > ACK_RETRY_TIMEOUT {
        if *retry_count >= MAX_RETRY_COUNT {
          warn!("Exceeded {} retries for command {:?}. Aborting.", MAX_RETRY_COUNT, message);
          *state = UartState::Idle;
        } else {
          *retry_count += 1;
          *last_sent = Instant::now();
          warn!("No ACK received. Re-transmitting command (attempt {}): {:?}", retry_count, message);
          let bytes = message.to_bytes();
          if let Err(err) = uart.write(&bytes) {
            error!("Failed to write retry to UART: {:?}", err);
          }
        }
      }
    }
  }
}

pub fn spawn_pool_worker(
  uart: UartDriver<'static>,
  mqtt_client: Arc<Mutex<EspMqttClient<'static>>>,
  outgoing_queue: MessageQueue,
) {
  std::thread::spawn(move || {
    debug!("Starting reading thread...");
    let mut state = UartState::Idle;
    loop {
      match scan_for_header(&uart) {
        Ok(HeaderScan::GoodHeader) => match read_packet(&uart) {
          Ok(buffer) => match process_package(&buffer) {
            Ok(msg) => {
              info!("Successfully decoded message from UART: {:?}", msg);
              handle_message(&msg, &mqtt_client, &mut state);
            }
            Err(err) => {
              warn!("Failed to decode Pentair message: {:?}. Discarding.", err);
            }
          },
          Err(err) => error!("Error reading package: {:?}", err),
        },
        Ok(HeaderScan::BusAvailable) => {
          manage_outgoing(&uart, &outgoing_queue, &mut state);
        }
        Err(err) => {
          warn!("UART error while scanning for header: {:?}", err);
        }
      }

      std::thread::sleep(WORKER_LOOP_SLEEP);
    }
  });
}

enum UartState {
  Idle,
  AwaitingAck { message: PentairMessage, last_sent: Instant, retry_count: u32 },
}
