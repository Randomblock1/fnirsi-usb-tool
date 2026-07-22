//! A/B benchmark: data-packet decode throughput.
//! Run with: `cargo test -p fnirsi-protocol --release --test ab_decode -- --ignored --nocapture`

use fnirsi_protocol::crc;
use fnirsi_protocol::protocol::{self, decode_data_packet};

fn make_packet() -> [u8; 64] {
    let mut pkt = [0u8; 64];
    pkt[0] = 0xAA;
    pkt[1] = protocol::packet_type::DATA;
    for i in 0..4 {
        let off = 2 + i * 15;
        pkt[off..off + 4].copy_from_slice(&500_000_u32.to_le_bytes());
        pkt[off + 4..off + 8].copy_from_slice(&100_000_u32.to_le_bytes());
        pkt[off + 8..off + 10].copy_from_slice(&2900_u16.to_le_bytes());
        pkt[off + 10..off + 12].copy_from_slice(&100_u16.to_le_bytes());
        pkt[off + 12] = 0x01;
        pkt[off + 13..off + 15].copy_from_slice(&250_u16.to_le_bytes());
    }
    pkt[63] = crc::data_crc(&pkt[1..63]);
    pkt
}

#[test]
#[ignore = "A/B benchmark"]
fn ab_decode_1m() {
    let pkt = make_packet();
    let iters = 1_000_000_u32;
    let mut acc = 0.0_f32;
    let start = std::time::Instant::now();
    for _ in 0..iters {
        let samples = decode_data_packet(std::hint::black_box(&pkt), true).unwrap();
        acc += samples[0].voltage_v;
    }
    let d = start.elapsed();
    std::hint::black_box(acc);
    println!(
        "AB_RESULT decode_1m: {:.1} ns/packet ({:.2} M samples/s)",
        d.as_secs_f64() * 1e9 / f64::from(iters),
        f64::from(iters) * 4.0 / d.as_secs_f64() / 1e6
    );
}
