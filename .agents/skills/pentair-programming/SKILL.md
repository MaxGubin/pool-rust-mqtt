---
name: pentair-programming
description: Understand, encode, decode, and implement reliable communication protocols for Pentair pool automation controllers (RS-485 serial).
---

# Pentair Programming Protocol Skill

This skill guides the agent in understanding, writing, and testing code that communicates with Pentair pool automation controllers via RS-485 serial interfaces.

---

## 1. Packet Framing & Layout

All Pentair automation packets on the serial bus follow a strict byte-level framing layout.

| Section | Size (Bytes) | Description | Values |
|---|---|---|---|
| **Preamble/Header** | 4 | Identifies the start of a Pentair packet | `0xFF 0x00 0xFF 0xA5` |
| **Protocol** | 1 | Protocol type identifier | `0x01` (standard) or `0x00` (pump status) |
| **Destination** | 1 | Destination device address | e.g. `0x10` (Main Controller), `0x0F` (Broadcast) |
| **Source** | 1 | Source device address | e.g. `0x10` (Main Controller), `0x48` (Remote Panel) |
| **Command** | 1 | Command or packet function code | e.g. `0x02` (Status), `0x86` (CircuitChange) |
| **Data Length** | 1 | Length of the subsequent payload data | `0x00` to `0xFF` |
| **Payload Data** | Variable | The message payload | Length specified by Data Length field |
| **Checksum** | 2 | Sum of all bytes starting from `0xA5` | 16-bit Big-Endian integer |

---

## 2. Checksum Calculation

The checksum is calculated by summing every byte in the packet starting from the `0xA5` byte (index 3 in the raw buffer) up to the end of the payload data. It is represented as a 16-bit unsigned integer in Big-Endian format.

### Rust Implementation Example:
```rust
fn calculate_checksum(packet: &[u8]) -> u16 {
    // Sum all bytes starting from index 3 (0xA5)
    let sum: u32 = packet[3..].iter().map(|&b| b as u32).sum();
    sum as u16
}
```

---

## 3. Supported Message Types & Commands

| Command Code | Name | Payload Layout & Descriptions |
|---|---|---|
| **`0x02`** | `Status` | **Length**: `29` bytes.<br>- `payload[2]`: Base switches mask (`0x20` Pool, `0x01` Spa, `0x02` Aux1, `0x04` Aux2, `0x08` Aux3, `0x10` Feature1, `0x40` Feature2, `0x80` Feature3).<br>- `payload[3]`: Additional switches mask (`0x01` Feature4).<br>- `payload[14]`: Water Temp (u8).<br>- `payload[18]`: Air Temp (u8).<br>- `payload[19]`: Solar Temp (u8). |
| **`0x86`** | `CircuitChange` | **Length**: `2` bytes.<br>- `payload[0]`: Circuit index code (e.g. `0x06` Pool, `0x01` Spa, `0x02` Aux1).<br>- `payload[1]`: Desired state (`0x01` for ON, `0x00` for OFF). |
| **`0x01`** | `CircuitChangeResponse` | **Length**: `1` byte (`0x86`). Sent as an acknowledgment (ACK) by the main controller when a `CircuitChange` is processed. |
| **`0xE1`** | `RemoteLayoutRequest` | **Length**: `1` byte (`0x01`). Query sent by remote panels to retrieve screen assignment. |
| **`0x21`** | `RemoteLayoutResponse`| **Length**: `4` bytes. Specifies the circuits bound to buttons/rows 1 through 4. |
| **`0x05`** | `ClockBroadcast` | **Length**: `8` bytes. Calendar broadcast: Hour, Minute, DayOfWeek, DayOfMonth, Month, Year, ClockAdjust, DST. |
| **`0x07`** | `PumpStatus` | **Length**: `15` to `18` bytes.<br>- `payload[0]`: Running state (`0x0a` running, `0x04` stopped).<br>- `payload[3..5]`: Power Watts (u16 Big-Endian).<br>- `payload[5..7]`: Speed RPM (u16 Big-Endian).<br>- `payload[7]`: Flow GPM (u8). |

---

## 4. Parser & Decoder Strategy (Robust Stream Processing)

When reading raw bytes from a serial interface (like RS-485 UART), packets may arrive fragmented or with corrupted bytes. A robust parsing loop must:
1. Append raw read bytes to a growing persistent dynamic buffer.
2. Scan the buffer linearly for the start byte `0xA5` (relative to the expected `0xFF 0x00 0xFF` preamble).
3. If `0xA5` is found, verify if enough bytes exist to read the Data Length field (`dlen = buffer[idx + 5]`).
4. Wait until the entire expected packet length (`6 + dlen + 2` bytes from `0xA5`) is fully received.
5. Extract the slice, calculate the checksum, and compare with the received checksum bytes at the end.
6. **On Match**: Decode the packet, process it, drain the consumed bytes from the buffer, and loop to check for subsequent packets.
7. **On Mismatch/Corruption**: Prune old bytes or step `idx` forward to resume scanning from the next potential header.

---

## 5. Reliable Transmission & Retries

Commands sent to a Pentair controller (specifically `CircuitChange`) must be acknowledged by the controller returning a `CircuitChangeResponse` (`0x01`).
Implement a reliable state-machine with:
- **`Idle`**: Accept outgoing messages. If `CircuitChange` is popped, transmit and enter `AwaitingAck`.
- **`AwaitingAck`**: Monitor incoming messages.
  - If `CircuitChangeResponse` is decoded, transition back to `Idle`.
  - If a timeout of `500ms` is reached, re-transmit the command and increment the retry count.
  - Limit retries to `10` attempts. If exceeded, log a warning, discard the command, and return to `Idle`.

## 6. UART parameters
The controller uses speed 9600 bod, databits: 8, parity: none, stopbits: 1

## 7. Rust style
- Use ideomatic Rust
- Define types to control parameters for function calls, avoid situations where parameters can be confused
- add tests to every module
