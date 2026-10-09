//! The "clip saved" sound: a short two-note chime synthesised here (no sound file to
//! license), played on the default output on a thread of its own.

use std::f32::consts::TAU;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// Notes (Hz, seconds): E6 then A6.
const NOTES: [(f32, f32); 2] = [(1318.5, 0.075), (1760.0, 0.14)];
const VOLUME: f32 = 0.22;

/// Sample `n` of the chime at `rate` (`None` after the end).
pub fn sample(n: u64, rate: u32) -> Option<f32> {
    #[allow(clippy::cast_precision_loss)] // sample index and rate are far below 2^24
    let t = n as f32 / rate as f32;
    let mut start = 0.0;
    for (freq, len) in NOTES {
        if t < start + len {
            let local = t - start;
            // 4 ms attack, exponential release: no clicks.
            let attack = (local / 0.004).min(1.0);
            let release = (-(local / len) * 4.0).exp();
            return Some((TAU * freq * local).sin() * attack * release * VOLUME);
        }
        start += len;
    }
    None
}

/// Play the chime (returns at once; failures are logged, never shown).
pub fn play() {
    let spawned = std::thread::Builder::new()
        .name("klip-cue".into())
        .spawn(|| {
            if let Err(e) = run() {
                tracing::debug!(error = %e, "clip sound");
            }
        });
    if let Err(e) = spawned {
        tracing::debug!(error = %e, "no thread for the clip sound");
    }
}

fn run() -> Result<(), String> {
    let device = cpal::default_host()
        .default_output_device()
        .ok_or("no audio output")?;
    let supported = device.default_output_config().map_err(|e| e.to_string())?;
    if supported.sample_format() != cpal::SampleFormat::F32 {
        return Err(format!(
            "output is {:?}, not f32",
            supported.sample_format()
        ));
    }
    let config = supported.config();
    let rate = config.sample_rate;
    let channels = usize::from(config.channels).max(1);
    let mut n: u64 = 0;
    let stream = device
        .build_output_stream::<f32, _, _>(
            config,
            move |data, _| {
                for frame in data.chunks_mut(channels) {
                    frame.fill(sample(n, rate).unwrap_or(0.0));
                    n = n.saturating_add(1);
                }
            },
            |e| tracing::debug!(error = %e, "clip sound stream"),
            None,
        )
        .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    // The chime is 215 ms; leave room for the device's latency.
    std::thread::sleep(Duration::from_millis(450));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chime_is_short_quiet_and_click_free() {
        let rate = 48_000;
        let all: Vec<f32> = (0..).map_while(|n| sample(n, rate)).collect();
        let secs = all.len() as f32 / rate as f32;
        assert!((0.2..0.25).contains(&secs), "{secs}");
        assert!(all.iter().all(|s| s.abs() <= VOLUME + 1e-6));
        assert!(all[0].abs() < 1e-3, "starts at silence");
        assert!(all.iter().any(|s| s.abs() > VOLUME * 0.5), "audible");
        assert!(all.last().unwrap().abs() < VOLUME * 0.05, "ends quietly");
    }
}
