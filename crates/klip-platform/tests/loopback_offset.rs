//! Diagnostic (hardware): process loopback vs system loopback timing for the same sound.
//! Start something that plays audio (e.g. `klip-testgame --beep`), then run with
//! `KLIP_DIAG_PID=<its pid> cargo test -p klip-platform --test loopback_offset -- --ignored --nocapture`.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::sync::{Arc, Mutex};

use klip_platform::{AudioCapture, AudioCaptureSink, AudioPacket, CaptureEndpoint};

type Packets = Arc<Mutex<Vec<(i64, u32, Vec<f32>)>>>;

struct Collect(Packets);
impl AudioCaptureSink for Collect {
    fn packet(&mut self, p: AudioPacket<'_>) {
        let mono = match p.data {
            Some(d) => d.chunks(2).map(|f| (f[0] + f[1]) / 2.0).collect(),
            None => vec![0.0; p.frames as usize],
        };
        self.0.lock().unwrap().push((p.qpc_ns, p.frames, mono));
    }
    fn notice(&mut self, m: String) {
        println!("notice: {m}");
    }
}

/// Place packets on a 48 kHz grid by their QPC time, starting at `t0`.
fn render(packets: &[(i64, u32, Vec<f32>)], t0: i64, len: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; len];
    for (t, _, d) in packets {
        let start = (t - t0) * 48_000 / 1_000_000_000;
        for (i, v) in d.iter().enumerate() {
            let k = start + i as i64;
            if k >= 0 && (k as usize) < len {
                out[k as usize] = *v;
            }
        }
    }
    out
}

#[test]
#[ignore = "diagnostic: needs KLIP_DIAG_PID of a process playing audio"]
fn diag_process_vs_system_loopback() {
    let pid: u32 = std::env::var("KLIP_DIAG_PID").unwrap().parse().unwrap();
    let sys: Packets = Arc::default();
    let pro: Packets = Arc::default();
    let a = AudioCapture::start(
        CaptureEndpoint::Loopback { device: None },
        48_000,
        2,
        Box::new(Collect(sys.clone())),
    )
    .unwrap();
    let b = AudioCapture::start(
        CaptureEndpoint::Process { pid, include: true },
        48_000,
        2,
        Box::new(Collect(pro.clone())),
    )
    .unwrap();
    std::thread::sleep(std::time::Duration::from_secs(6));
    a.stop();
    b.stop();
    let (sys, pro) = (sys.lock().unwrap(), pro.lock().unwrap());
    for (name, p) in [("system", &*sys), ("process", &*pro)] {
        let mut d: Vec<i64> = p
            .windows(2)
            .map(|w| (w[1].0 - w[0].0) - w[0].1 as i64 * 1_000_000_000 / 48_000)
            .collect();
        d.sort();
        let sizes: std::collections::BTreeSet<u32> = p.iter().map(|x| x.1).collect();
        println!(
            "{name}: {} packets, sizes {sizes:?}, timestamp step minus length: min {} p50 {} max {} us",
            p.len(),
            d.first().unwrap() / 1000,
            d[d.len() / 2] / 1000,
            d.last().unwrap() / 1000
        );
    }
    let t0 = sys[0].0.max(pro[0].0) + 200_000_000;
    let len = 48_000 * 5;
    let (x, y) = (render(&sys, t0, len), render(&pro, t0, len));
    // Lag (in samples) of the process stream behind the system stream, over ±60 ms.
    let mut best = (0i64, f64::MIN);
    for lag in -2880i64..=2880 {
        let mut dot = 0.0f64;
        for i in 3000..len - 3000 {
            dot += f64::from(x[i]) * f64::from(y[(i as i64 + lag) as usize]);
        }
        if dot > best.1 {
            best = (lag, dot);
        }
    }
    println!(
        "process loopback stamps are {:+.2} ms after system loopback for the same audio",
        best.0 as f64 / 48.0
    );
    // Per beep: onset in the process stream (only the game's audio), its timestamp
    // residual (stamp minus sample-count time), and the exact lag behind the system stream.
    let base = pro[0].0;
    let mut n = 0i64;
    let mut residual = Vec::new();
    for (t, f, _) in pro.iter() {
        residual.push((*t, t - (base + n * 1_000_000_000 / 48_000)));
        n += i64::from(*f);
    }
    let mut i = 3000;
    while i < len - 3000 {
        if y[i].abs() > 0.1 {
            // Onset of the same beep in the system stream: first sample over the threshold
            // within ±40 ms.
            let lo = i.saturating_sub(1920);
            let sys_onset = (lo..(i + 1920).min(len)).find(|&k| x[k].abs() > 0.1);
            let b3 = (sys_onset.map_or(i64::MIN, |k| i as i64 - k as i64), 0.0);
            let t = t0 + i as i64 * 1_000_000_000 / 48_000;
            let r = residual
                .iter()
                .rev()
                .find(|(pt, _)| *pt <= t)
                .map_or(0, |(_, r)| *r);
            println!(
                "  beep at {:.3} s: process {:+.2} ms after system; stamp residual {:+.2} ms",
                i as f64 / 48_000.0,
                b3.0 as f64 / 48.0,
                r as f64 / 1e6
            );
            i += 48_000 / 2;
        } else {
            i += 1;
        }
    }
    let mut rs: Vec<i64> = residual.iter().map(|r| r.1).collect();
    rs.sort();
    println!(
        "  residual min {:+.2} p50 {:+.2} max {:+.2} ms",
        rs[0] as f64 / 1e6,
        rs[rs.len() / 2] as f64 / 1e6,
        rs[rs.len() - 1] as f64 / 1e6
    );
}
