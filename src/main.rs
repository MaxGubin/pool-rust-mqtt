use esp_idf_svc::{
  eventloop::EspSystemEventLoop,
  hal::{
    peripherals::Peripherals,
    uart::{UartConfig, UartDriver},
  },
  mqtt::client::{EspMqttClient, MqttClientConfiguration, QoS},
  nvs::EspDefaultNvsPartition,
  wifi::{AuthMethod, ClientConfiguration, Configuration, EspWifi},
};
use log::*;
use std::time::Duration;

pub mod pentair;
pub mod pool_worker;

const WIFI_SSID: &str = env!("WIFI_SSID");
const WIFI_PW: &str = env!("WIFI_PW");
const HIVEMQ_HOST: &str = env!("HIVEMQ_HOST");
const HIVEMQ_USER: &str = env!("HIVEMQ_USER");
const HIVEMQ_PW: &str = env!("HIVEMQ_PW");

fn main() -> Result<(), Box<dyn std::error::Error>> {
  esp_idf_svc::sys::link_patches();
  esp_idf_svc::log::EspLogger::initialize_default();
  log::set_max_level(log::LevelFilter::Debug);
  unsafe {
    esp_idf_svc::sys::esp_log_level_set(b"*\0".as_ptr() as *const _, 4);
  }

  info!("Initializing Peripherals...");
  let peripherals = Peripherals::take()?;
  let sys_loop = EspSystemEventLoop::take()?;
  let nvs = EspDefaultNvsPartition::take()?;

  // 1. Initialize Wi-Fi
  info!("Connecting to Wi-Fi SSID: {}", WIFI_SSID);
  let mut wifi = EspWifi::new(peripherals.modem, sys_loop, Some(nvs))?;
  wifi.set_configuration(&Configuration::Client(ClientConfiguration {
    ssid: WIFI_SSID.try_into().unwrap(),
    password: WIFI_PW.try_into().unwrap(),
    auth_method: AuthMethod::WPA2Personal,
    ..Default::default()
  }))?;

  // Set DHCP hostname
  use esp_idf_svc::handle::RawHandle;
  let netif_handle = wifi.sta_netif().handle();
  unsafe {
    if let Ok(hostname) = std::ffi::CString::new("pool-controller") {
      let err = esp_idf_svc::sys::esp_netif_set_hostname(netif_handle, hostname.as_ptr());
      if err != 0 {
        warn!("Failed to set DHCP hostname: {}", err);
      } else {
        info!("DHCP Hostname set to 'pool-controller'");
      }
    }
  }

  wifi.start()?;
  wifi.connect()?;

  info!("Waiting for Wi-Fi DHCP IP...");
  while !wifi.is_connected()? || wifi.sta_netif().get_ip_info()?.ip.is_unspecified() {
    std::thread::sleep(Duration::from_millis(500));
  }
  info!("Wi-Fi Connected! IP: {:?}", wifi.sta_netif().get_ip_info()?);

  // Spawn a background thread to monitor Wi-Fi status and reconnect if needed
  std::thread::spawn(move || {
    loop {
      std::thread::sleep(Duration::from_secs(5));
      match wifi.is_connected() {
        Ok(false) => {
          warn!("Wi-Fi disconnected! Attempting to reconnect...");
          if let Err(err) = wifi.connect() {
            error!("Failed to trigger Wi-Fi reconnection: {:?}", err);
          }
        }
        Ok(true) => {}
        Err(err) => {
          error!("Error checking Wi-Fi status: {:?}", err);
        }
      }
    }
  });

  // 2. Initialize UART (Pins 16 and 17)
  info!("Initializing UART...");
  let config = UartConfig::new().baudrate(9600.into());
  let uart = UartDriver::new(
    peripherals.uart1,
    peripherals.pins.gpio17, // TX
    peripherals.pins.gpio16, // RX
    Option::<esp_idf_svc::hal::gpio::AnyIOPin>::None,
    Option::<esp_idf_svc::hal::gpio::AnyIOPin>::None,
    &config,
  )?;

  // 3. Initialize MQTT Client connected to HiveMQ Cloud
  info!("Connecting to HiveMQ Cloud: {}", HIVEMQ_HOST);
  let mqtt_config = MqttClientConfiguration {
    client_id: Some("esp32-pool-client"),
    username: Some(HIVEMQ_USER),
    password: Some(HIVEMQ_PW),
    crt_bundle_attach: Some(esp_idf_svc::sys::esp_crt_bundle_attach),
    ..Default::default()
  };

  let broker_url = format!("mqtts://{}", HIVEMQ_HOST);
  let (client, mut connection) = EspMqttClient::new(&broker_url, &mqtt_config)?;
  let client = std::sync::Arc::new(std::sync::Mutex::new(client));
  let client_clone = client.clone();

  // Initialize outgoing queue for Pentair messages
  let outgoing_queue = pool_worker::MessageQueue::new();
  let outgoing_queue_clone = outgoing_queue.clone();

  // Spawn the pool worker that handles UART reads, decoding, reliable sends and ACKs
  pool_worker::spawn_pool_worker(uart, client.clone(), outgoing_queue);

  // Spawn a background thread to process MQTT connection events and incoming messages
  std::thread::spawn(move || {
    let client = client_clone;
    loop {
      match connection.next() {
        Ok(event) => match event.payload() {
          esp_idf_svc::mqtt::client::EventPayload::Connected(_) => {
            info!("MQTT Connected successfully!");
            info!("Setting up subscriptions in a background thread...");
            let client_subscribe = client.clone();
            std::thread::spawn(move || {
              let mut guard = client_subscribe.lock().unwrap();
              if let Err(err) = guard.subscribe("pool/pump/set", QoS::AtMostOnce) {
                error!("Failed to subscribe to pool/pump/set: {:?}", err);
              }
              if let Err(err) = guard.subscribe("pool/light/set", QoS::AtMostOnce) {
                error!("Failed to subscribe to pool/light/set: {:?}", err);
              }
              info!("Subscriptions have been set up.");
            });
          }
          esp_idf_svc::mqtt::client::EventPayload::Disconnected => {
            warn!("MQTT Disconnected from broker!");
          }
          esp_idf_svc::mqtt::client::EventPayload::Subscribed(_) => {
            info!("MQTT Subscription confirmed by broker!");
          }
          esp_idf_svc::mqtt::client::EventPayload::Received { topic, data, .. } => {
            let topic = topic.unwrap_or("");
            let payload = data;
            info!("Received command on topic '{}': {:?}", topic, payload);

            if topic == "pool/pump/set" {
              let state = payload == b"ON";
              info!("Queuing Pentair Pool CircuitChange command (state: {})", state);
              outgoing_queue_clone
                .push(pentair::PentairMessage::CircuitChange(pentair::Circuit::Pool, state));
            } else if topic == "pool/light/set" {
              let state = payload == b"ON";
              info!("Queuing Pentair Aux1 (Light) CircuitChange command (state: {})", state);
              outgoing_queue_clone
                .push(pentair::PentairMessage::CircuitChange(pentair::Circuit::Aux1, state));
            }
          }
          _ => {}
        },
        Err(err) => {
          error!("MQTT connection next() returned error: {:?}. Retrying in 2 seconds...", err);
          std::thread::sleep(Duration::from_secs(2));
        }
      }
    }
  });

  // 4. Main reporting loop
  info!("Pool controller loop running...");
  loop {
    std::thread::sleep(Duration::from_secs(30));
    info!("Reporting current pool state...");
    // Publish pool temperature
    if let Err(err) = client.lock().unwrap().publish("pool/temp", QoS::AtMostOnce, false, b"78.5") {
      error!("Failed to publish pool temperature: {:?}", err);
    }
  }
}
