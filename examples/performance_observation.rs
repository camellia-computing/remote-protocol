use std::{
    env,
    fs::OpenOptions,
    hint::black_box,
    io::{BufWriter, Write},
    path::PathBuf,
    time::Instant,
};

use camellia_remote_protocol::{
    error,
    message_proto::{ErrorCode, ErrorEnvelope},
    protobuf::Message,
};
use serde_json::json;

const SCENARIO: &str = "protocol-error-envelope-v1";
const ITERATIONS: usize = 200_000;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = PathBuf::from(
        env::var_os("CAMELLIA_PERFORMANCE_OBSERVATION_FILE")
            .ok_or("CAMELLIA_PERFORMANCE_OBSERVATION_FILE is required")?,
    );
    if !output.is_absolute() {
        return Err("performance observation path must be absolute".into());
    }

    let envelope = error::with_details(
        error::for_code(ErrorCode::HTTP_PROXY_FAILED),
        "bounded-performance-detail",
    );
    let expected_bytes = envelope.write_to_bytes()?;
    let started = Instant::now();
    let mut total_bytes = 0usize;
    for _ in 0..ITERATIONS {
        let encoded = black_box(&envelope).write_to_bytes()?;
        let decoded = ErrorEnvelope::parse_from_bytes(black_box(&encoded))?;
        if !error::is_valid(&decoded) || decoded != envelope {
            return Err("error envelope round-trip changed semantics".into());
        }
        total_bytes = total_bytes
            .checked_add(encoded.len())
            .ok_or("byte counter overflow")?;
        black_box(decoded);
    }
    let elapsed_ns = started.elapsed().as_nanos();
    let roundtrip_ns = elapsed_ns as f64 / ITERATIONS as f64;
    if total_bytes != expected_bytes.len() * ITERATIONS {
        return Err("encoded size changed during one observation".into());
    }

    let document = json!({
        "metrics": {
            "encoded_bytes": {
                "direction": "lower",
                "unit": "bytes",
                "value": expected_bytes.len(),
            },
            "roundtrip_ns": {
                "direction": "lower",
                "unit": "ns",
                "value": roundtrip_ns,
            },
        },
        "scenario": SCENARIO,
        "schema": "camellia-performance-observation-v1",
    });
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output)?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer(&mut writer, &document)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    writer.get_ref().sync_all()?;
    Ok(())
}
