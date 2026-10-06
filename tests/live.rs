//! Live audio proof-of-concept (Step 2 of the plan).
//! Ignored by default (`cargo test` stays offline); run with:
//!   cargo test --test live -- --ignored --nocapture
//! Connects to a real verified stream, buffers, and decodes.

use lofi::stream::verify_station_stream;

#[tokio::test]
#[ignore]
async fn live_groove_salad_decodes() {
    let (codec, rate, channels) = verify_station_stream(
        "https://ice1.somafm.com/groovesalad-128-mp3",
        64 * 1024,
    )
    .await
    .expect("live stream should connect and decode");
    println!("codec={codec} rate={rate} channels={channels}");
    assert!(rate > 0);
    assert!(channels > 0);
}

/// End-to-end: worker streams, decodes continuously, and emits spectrum
/// levels with real energy (exercises pipe → Symphonia → FFT → events).
#[tokio::test]
#[ignore]
async fn live_levels_flow_with_energy() {
    use lofi::audio::{AudioCommand, AudioEvent, AudioWorker};
    use lofi::stream::StreamSource;
    use std::time::Duration;

    let mut worker = AudioWorker::spawn();
    worker.send(AudioCommand::Play(StreamSource::new(
        "https://ice1.somafm.com/groovesalad-128-mp3",
    )));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut got_levels = false;
    let mut max_band = 0.0f32;
    while tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(200)).await;
        for ev in worker.drain_events() {
            if let AudioEvent::Levels { bands, wave } = ev {
                assert_eq!(bands.len(), 32);
                assert_eq!(wave.len(), 48);
                got_levels = true;
                max_band = max_band.max(bands.iter().cloned().fold(0.0, f32::max));
            }
        }
        if got_levels && max_band > 0.2 {
            break;
        }
    }
    worker.send(AudioCommand::Stop);
    assert!(got_levels, "expected spectrum levels from live stream");
    assert!(
        max_band > 0.2,
        "bands should show energy, max={max_band}"
    );
}
