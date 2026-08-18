use std::{
  fs::File,
  io::{BufRead, BufReader, Write},
  path::Path,
};

#[path = "../pentair.rs"]
mod pentair;

use pentair::{PentairMessage, WireDecoder};

fn main() -> Result<(), Box<dyn std::error::Error>> {
  let log_path = "pool.log";
  if !Path::new(log_path).exists() {
    eprintln!("Error: log file '{}' not found.", log_path);
    std::process::exit(1);
  }

  println!("Reading and parsing '{}'...", log_path);
  let file = File::open(log_path)?;
  let reader = BufReader::new(file);

  // Open output files
  let mut bin_output = File::create("extracted_packets.bin")?;
  let mut txt_output = File::create("extracted_packets.txt")?;

  let mut packet_count = 0;
  let mut decoded_count = 0;

  for line_result in reader.lines() {
    let line = line_result?;
    if let Some(pos) = line.find("Processing packet [") {
      let start = pos + "Processing packet [".len();
      if let Some(end) = line[start..].find(']') {
        let array_str = &line[start..start + end];

        // Parse comma-separated list of bytes
        let packet_body: Result<Vec<u8>, _> =
          array_str.split(',').map(|s| s.trim().parse::<u8>()).collect();

        if let Ok(body) = packet_body {
          if body.is_empty() {
            continue;
          }
          packet_count += 1;

          // Reconstruct the full packet with preamble and checksum
          let mut full_packet = vec![0xFF, 0x00, 0xFF, 0xA5];
          full_packet.extend_from_slice(&body);

          // Calculate correct checksum
          let mut sum: u32 = 0;
          for &b in &full_packet[3..] {
            sum += b as u32;
          }
          let checksum = sum as u16;
          full_packet.push((checksum >> 8) as u8);
          full_packet.push((checksum & 0xFF) as u8);

          // Write raw binary representation to .bin file
          bin_output.write_all(&full_packet)?;

          // Format full packet as uppercase hex string
          let hex_str: String =
            full_packet.iter().map(|b| format!("{:02X}", b)).collect::<Vec<String>>().join(" ");

          // Write plain-text hex string to .txt file
          writeln!(txt_output, "{}", hex_str)?;

          print!("Packet #{:04}: {}", packet_count, hex_str);

          // Attempt to decode using Pentair parser
          match PentairMessage::decode(&full_packet) {
            Ok((msg, _)) => {
              decoded_count += 1;
              println!("  => Decoded: {:?}", msg);
            }
            Err(_) => {
              println!("  => [Unknown/Unrecognized Pentair command]");
            }
          }
        }
      }
    }
  }

  println!("\nSummary:");
  println!("Total packets extracted: {}", packet_count);
  println!("Successfully decoded:    {} / {}", decoded_count, packet_count);
  println!("Raw binary output saved to:   'extracted_packets.bin'");
  println!("Text hex output saved to:     'extracted_packets.txt'");

  Ok(())
}
