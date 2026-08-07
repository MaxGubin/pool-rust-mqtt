use esp_idf_svc::{
  hal::uart::UartDriver,
  mqtt::client::{EspMqttClient, QoS},
};
use log::*;
use std::{
  collections::VecDeque,
  sync::{Arc, Mutex},
  time::{Duration, Instant},
};

use crate::pentair::{PentairMessage, WireDecoder};

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

fn align_to_preamble(buf: &mut Vec<u8>) {
  if buf.len() < 4 {
    return;
  }
  for i in 0..=buf.len() - 4 {
    if buf[i..i + 4] == [0xFF, 0x00, 0xFF, 0xA5] {
      if i > 0 {
        buf.drain(0..i);
      }
      return;
    }
  }
  // If preamble is not found, but a suffix could be a partial preamble, keep it
  if buf.len() > 4 {
    if buf.ends_with(&[0xFF, 0x00, 0xFF]) {
      buf.drain(0..buf.len() - 3);
    } else if buf.ends_with(&[0xFF, 0x00]) || buf.ends_with(&[0x00, 0xFF]) {
      buf.drain(0..buf.len() - 2);
    } else if buf.ends_with(&[0xFF]) || buf.ends_with(&[0x00]) {
      buf.drain(0..buf.len() - 1);
    } else {
      buf.clear();
    }
  }
}

pub fn spawn_pool_worker(
  uart: UartDriver<'static>,
  mqtt_client: Arc<Mutex<EspMqttClient<'static>>>,
  outgoing_queue: MessageQueue,
) {
  std::thread::spawn(move || {
    let mut buf = Vec::new();
    let mut read_buf = [0u8; 128];
    let mut state = UartState::Idle;

    loop {
      // 1. Read from UART (timeout of 5 ticks = ~50ms)
      match uart.read(&mut read_buf, 5) {
        Ok(bytes_read) if bytes_read > 0 => {
          buf.extend_from_slice(&read_buf[..bytes_read]);
          debug!("Read {} bytes from UART. Buffer size: {}", bytes_read, buf.len());
        }
        Ok(_) => {}
        Err(err) => {
          if err.code() != esp_idf_svc::sys::ESP_ERR_TIMEOUT {
            error!("Error reading from UART: {:?}", err);
          }
        }
      }

      // 2. Align buffer and process all complete messages
      align_to_preamble(&mut buf);

      while buf.len() >= 9 {
        let dlen = buf[8] as usize;
        let total_len = 11 + dlen;

        if buf.len() < total_len {
          // Packet is incomplete, wait for more bytes
          break;
        }

        // We have a full packet candidate!
        let packet_candidate = buf[..total_len].to_vec();
        match PentairMessage::decode(&packet_candidate) {
          Ok((msg, _)) => {
            info!("Successfully decoded message from UART: {:?}", msg);
            buf.drain(0..total_len);

            match &msg {
              PentairMessage::Status(state) => {
                // Format system state as JSON string
                let payload = format!(
                  r#"{{"pool_on":{},"spa_on":{},"water_temp":{},"air_temp":{},"solar_temp":{}}}"#,
                  state.pool_on, state.spa_on, state.water_temp, state.air_temp, state.solar_temp
                );
                info!("Publishing state update to MQTT: {}", payload);
                if let Err(err) = mqtt_client.lock().unwrap().publish(
                  "pool/state",
                  QoS::AtMostOnce,
                  false,
                  payload.as_bytes(),
                ) {
                  error!("Failed to publish pool state to MQTT: {:?}", err);
                }
              }
              PentairMessage::CircuitChangeResponse => {
                info!("Received CircuitChangeResponse acknowledgment!");
                if let UartState::AwaitingAck { .. } = state {
                  state = UartState::Idle;
                }
              }
              _ => {
                debug!("Received other Pentair message: {:?}", msg);
              }
            }

            // Align buffer for the next message candidate
            align_to_preamble(&mut buf);
          }
          Err(err) => {
            warn!(
              "Failed to decode complete packet of length {} starting with preamble. Checksum invalid or corrupted: {:?}. Discarding corrupted preamble.",
              total_len, err
            );
            // Discard the corrupted preamble/header so we can search for the next valid one
            buf.drain(0..4);
            align_to_preamble(&mut buf);
          }
        }
      }

      // 3. Manage reliable outgoing state machine
      match state {
        UartState::Idle => {
          if let Some(msg) = outgoing_queue.pop() {
            info!("Sending message to UART: {:?}", msg);
            let bytes = msg.to_bytes();
            if let Err(err) = uart.write(&bytes) {
              error!("Failed to write to UART: {:?}", err);
            }

            if let PentairMessage::CircuitChange(..) = msg {
              state =
                UartState::AwaitingAck { message: msg, last_sent: Instant::now(), retry_count: 0 };
            }
          }
        }
        UartState::AwaitingAck { ref message, ref mut last_sent, ref mut retry_count } => {
          if last_sent.elapsed() > Duration::from_millis(500) {
            if *retry_count >= 10 {
              warn!("Exceeded 10 retries for command {:?}. Aborting.", message);
              state = UartState::Idle;
            } else {
              *retry_count += 1;
              *last_sent = Instant::now();
              warn!(
                "No ACK received. Re-transmitting command (attempt {}): {:?}",
                retry_count, message
              );
              let bytes = message.to_bytes();
              if let Err(err) = uart.write(&bytes) {
                error!("Failed to write retry to UART: {:?}", err);
              }
            }
          }
        }
      }

      std::thread::sleep(Duration::from_millis(10));
    }
  });
}

enum UartState {
  Idle,
  AwaitingAck { message: PentairMessage, last_sent: Instant, retry_count: u32 },
}
