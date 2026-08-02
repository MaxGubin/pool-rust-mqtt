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

const WIFI_SSID: &str = env!("WIFI_SSID");
const WIFI_PW: &str = env!("WIFI_PW");
const HIVEMQ_HOST: &str = env!("HIVEMQ_HOST");
const HIVEMQ_USER: &str = env!("HIVEMQ_USER");
const HIVEMQ_PW: &str = env!("HIVEMQ_PW");

fn main() -> Result<(), Box<dyn std::error::Error>> {
  esp_idf_svc::sys::link_patches();
  esp_idf_svc::log::EspLogger::initialize_default();

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
  let config = UartConfig::new().baudrate(115200.into());
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
  let (mut client, mut connection) = EspMqttClient::new(&broker_url, &mqtt_config)?;
  loop {
    if let Ok(event) = connection.next() {
      if let esp_idf_svc::mqtt::client::EventPayload::Connected(_) = event.payload() {
        info!("Successfully connected");
        break;
      }
    }
    std::thread::sleep(Duration::from_millis(50));
  }

  // Spawn a background thread to process MQTT connection events and incoming messages
  std::thread::spawn(move || {
    while let Ok(event) = connection.next() {
      match event.payload() {
        esp_idf_svc::mqtt::client::EventPayload::Connected(_) => {
          info!("MQTT Connected successfully!");
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
            if payload == b"ON" {
              info!("Turning pump ON!");
              uart.write(b"PUMP:ON\r\n").unwrap();
            } else if payload == b"OFF" {
              info!("Turning pump OFF!");
              uart.write(b"PUMP:OFF\r\n").unwrap();
            }
          } else if topic == "pool/light/set" {
            if payload == b"ON" {
              info!("Turning light ON!");
              uart.write(b"LIGHT:ON\r\n").unwrap();
            } else if payload == b"OFF" {
              info!("Turning light OFF!");
              uart.write(b"LIGHT:OFF\r\n").unwrap();
            }
          }
        }
        _ => {}
      }
    }
  });

  // Subscribe to topics
  info!("Setting up subscriptions");
  client.subscribe("pool/pump/set", QoS::AtMostOnce)?;
  client.subscribe("pool/light/set", QoS::AtMostOnce)?;

  // 4. Main reporting loop
  info!("Pool controller loop running...");
  loop {
    std::thread::sleep(Duration::from_secs(30));
    info!("Reporting current pool state...");
    // Publish pool temperature
    if let Err(err) = client.publish("pool/temp", QoS::AtMostOnce, false, b"78.5") {
      error!("Failed to publish pool temperature: {:?}", err);
    }
  }
}
