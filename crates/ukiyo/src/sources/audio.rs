//! `audio.*`: what's playing, from the default sink's **monitor** (never a
//! microphone). A child process (`pw-record`, else `parec`) streams raw f32
//! mono at 48 kHz; a reader thread runs an FFT over 1024-sample windows
//! and publishes the levels into shared state the frame loop reads.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustfft::{Fft, FftPlanner, num_complex::Complex};

pub const RATE: f32 = 48_000.0;
pub const WINDOW: usize = 1024;
pub const HOP: usize = 512;

/// Band edges in Hz.
pub const BASS: (f32, f32) = (40.0, 200.0);
pub const MID: (f32, f32) = (200.0, 2_000.0);
pub const TREBLE: (f32, f32) = (2_000.0, 12_000.0);

/// How long `audio.beat` reads 1 after an onset (so a frame sees it).
const BEAT_HOLD: f32 = 0.1;
/// No two beats closer than this.
const BEAT_REFRACTORY: f32 = 0.2;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Levels {
    pub level: f32,
    pub bass: f32,
    pub mid: f32,
    pub treble: f32,
    pub beat: f32,
}

/// dB (re full scale) to 0..1 over `floor..ceil`.
fn db01(power: f32, floor: f32, ceil: f32) -> f32 {
    let db = 10.0 * power.max(1e-12).log10();
    ((db - floor) / (ceil - floor)).clamp(0.0, 1.0)
}

pub struct Analyzer {
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    buf: Vec<Complex<f32>>,
    /// Samples not yet analysed.
    pending: Vec<f32>,
    prev_bass: Vec<f32>,
    flux_hist: std::collections::VecDeque<f32>,
    /// Seconds of audio processed.
    clock: f32,
    last_beat: f32,
    pub levels: Levels,
}

impl Default for Analyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer {
    pub fn new() -> Analyzer {
        let fft = FftPlanner::new().plan_fft_forward(WINDOW);
        let window = (0..WINDOW)
            .map(|i| {
                0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (WINDOW - 1) as f32).cos()
            })
            .collect();
        Analyzer {
            fft,
            window,
            buf: vec![Complex::default(); WINDOW],
            pending: Vec::with_capacity(WINDOW * 2),
            prev_bass: Vec::new(),
            flux_hist: Default::default(),
            clock: 0.0,
            last_beat: -1.0,
            levels: Levels::default(),
        }
    }

    fn bin(hz: f32) -> usize {
        ((hz * WINDOW as f32 / RATE).round() as usize).clamp(1, WINDOW / 2)
    }

    /// Feed samples; analyses every full hop. Returns whether the levels
    /// were updated.
    pub fn push(&mut self, samples: &[f32]) -> bool {
        self.pending.extend_from_slice(samples);
        let mut updated = false;
        while self.pending.len() >= WINDOW {
            self.analyse();
            self.pending.drain(..HOP);
            self.clock += HOP as f32 / RATE;
            updated = true;
        }
        updated
    }

    fn analyse(&mut self) {
        let x = &self.pending[..WINDOW];
        let ms = x.iter().map(|v| v * v).sum::<f32>() / WINDOW as f32;
        // Full-scale sine: mean square 0.5 → -3 dB.
        let level = db01(ms * 2.0, -50.0, -6.0);

        for ((b, v), w) in self.buf.iter_mut().zip(x).zip(&self.window) {
            *b = Complex::new(v * w, 0.0);
        }
        self.fft.process(&mut self.buf);
        // Amplitude per bin: a full-scale sine reads 1 at its bin (Hann
        // window has a coherent gain of 0.5).
        let norm = 2.0 / (WINDOW as f32 * 0.5);
        let mag: Vec<f32> = self.buf[..=WINDOW / 2]
            .iter()
            .map(|c| c.norm() * norm)
            .collect();
        // Band power: the strongest bin (a tone and broadband music both
        // read sensibly, no matter how wide the band).
        // Bands don't share bins: each starts above the previous one's top.
        let band = |(lo, hi): (f32, f32)| -> f32 {
            let first = if lo <= BASS.0 {
                Self::bin(lo)
            } else {
                Self::bin(lo) + 1
            };
            mag[first..=Self::bin(hi)]
                .iter()
                .fold(0.0f32, |m, v| m.max(*v))
                .powi(2)
        };
        let bass = db01(band(BASS), -50.0, -6.0);
        let mid = db01(band(MID), -55.0, -10.0);
        let treble = db01(band(TREBLE), -60.0, -15.0);

        // Spectral flux on the bass bins, against a running mean.
        let bins = &mag[Self::bin(BASS.0)..=Self::bin(BASS.1)];
        let flux: f32 = if self.prev_bass.len() == bins.len() {
            bins.iter()
                .zip(&self.prev_bass)
                .map(|(a, b)| (a - b).max(0.0))
                .sum()
        } else {
            0.0
        };
        self.prev_bass.clear();
        self.prev_bass.extend_from_slice(bins);
        let mean = if self.flux_hist.is_empty() {
            0.0
        } else {
            self.flux_hist.iter().sum::<f32>() / self.flux_hist.len() as f32
        };
        // About a second of history.
        self.flux_hist.push_back(flux);
        if self.flux_hist.len() > (RATE / HOP as f32) as usize {
            self.flux_hist.pop_front();
        }
        if flux > mean * 1.8 + 0.02 && self.clock - self.last_beat >= BEAT_REFRACTORY {
            self.last_beat = self.clock;
        }
        let beat = if self.clock - self.last_beat < BEAT_HOLD {
            1.0
        } else {
            0.0
        };
        self.levels = Levels {
            level,
            bass,
            mid,
            treble,
            beat,
        };
    }
}

/// The capture command lines to try, in order.
pub fn commands(device: &str) -> Vec<Vec<String>> {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let mut pw = s(&[
        "pw-record",
        "--raw",
        "--format",
        "f32",
        "--rate",
        "48000",
        "--channels",
        "1",
        "--latency",
        "10ms",
        "-P",
        "{ stream.capture.sink = true node.name = ukiyo-audio node.description = \"ukiyo (sink monitor)\" }",
    ]);
    let mut pa = s(&[
        "parec",
        "--format=float32le",
        "--channels=1",
        "--rate=48000",
        "--raw",
        "--latency-msec=20",
        "--client-name=ukiyo",
    ]);
    if device.is_empty() {
        pa.push("--device=@DEFAULT_MONITOR@".into());
    } else {
        // Always the sink, so always its monitor.
        pw.extend(["--target".into(), device.to_string()]);
        pa.push(format!("--device={device}.monitor"));
    }
    pw.push("-".into());
    vec![pw, pa]
}

/// A running capture: the child and the reader thread feeding `out`.
pub struct Capture {
    child: Child,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Capture {
    /// Start the first capture command that runs. `out` gets the levels.
    pub fn start(device: &str, out: Arc<Mutex<Levels>>) -> Option<(Capture, String)> {
        for cmd in commands(device) {
            let child = Command::new(&cmd[0])
                .args(&cmd[1..])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn();
            let Ok(mut child) = child else { continue };
            let Some(mut stdout) = child.stdout.take() else {
                let _ = child.kill();
                let _ = child.wait();
                continue;
            };
            let out2 = out.clone();
            let thread = std::thread::Builder::new()
                .name("ukiyo-audio".into())
                .spawn(move || {
                    let mut a = Analyzer::new();
                    let mut bytes = vec![0u8; HOP * 4];
                    let mut have = 0;
                    let mut samples = Vec::with_capacity(HOP);
                    loop {
                        match stdout.read(&mut bytes[have..]) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => have += n,
                        }
                        let whole = have / 4 * 4;
                        samples.clear();
                        samples.extend(
                            bytes[..whole]
                                .as_chunks::<4>()
                                .0
                                .iter()
                                .map(|c| f32::from_le_bytes(*c)),
                        );
                        bytes.copy_within(whole..have, 0);
                        have -= whole;
                        if a.push(&samples)
                            && let Ok(mut l) = out2.lock()
                        {
                            *l = a.levels;
                        }
                    }
                    if let Ok(mut l) = out2.lock() {
                        *l = Levels::default();
                    }
                })
                .ok();
            // A command that exits at once (no PipeWire) isn't a capture.
            let t0 = Instant::now();
            while t0.elapsed() < Duration::from_millis(150) {
                if let Ok(Some(_)) = child.try_wait() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            if let Ok(Some(_)) = child.try_wait() {
                if let Some(t) = thread {
                    let _ = t.join();
                }
                continue;
            }
            return Some((Capture { child, thread }, cmd[0].clone()));
        }
        None
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(hz: f32, amp: f32, n: usize, phase0: usize) -> Vec<f32> {
        (0..n)
            .map(|i| amp * (2.0 * std::f32::consts::PI * hz * (i + phase0) as f32 / RATE).sin())
            .collect()
    }

    fn run(hz: f32, amp: f32) -> Levels {
        let mut a = Analyzer::new();
        a.push(&sine(hz, amp, WINDOW * 4, 0));
        a.levels
    }

    #[test]
    fn bands_follow_the_tone() {
        let l = run(100.0, 0.5);
        assert!(l.bass > 0.8 && l.mid < 0.3 && l.treble < 0.1, "{l:?}");
        let l = run(1000.0, 0.5);
        assert!(l.mid > 0.8 && l.bass < 0.1 && l.treble < 0.1, "{l:?}");
        let l = run(5000.0, 0.5);
        assert!(l.treble > 0.8 && l.bass < 0.1 && l.mid < 0.3, "{l:?}");
    }

    #[test]
    fn level_is_perceptual() {
        let silent = run(440.0, 0.0);
        assert_eq!(silent, Levels::default());
        let quiet = run(440.0, 0.01).level;
        let loud = run(440.0, 0.5).level;
        let full = run(440.0, 1.0).level;
        assert!(quiet > 0.05 && quiet < 0.3, "{quiet}");
        assert!(loud > 0.7 && loud < full && full == 1.0, "{loud} {full}");
    }

    #[test]
    fn beats_on_kicks_only() {
        let mut a = Analyzer::new();
        let mut beats = 0;
        let mut was = 0.0;
        // Two seconds of a steady tone: no beats after the start.
        let steady = sine(60.0, 0.3, 48_000 * 2, 0);
        for c in steady.chunks(HOP) {
            a.push(c);
        }
        let mut t = 48_000 * 2;
        // Then 4 kicks a second for two seconds: 60 Hz bursts over silence.
        for k in 0..8 {
            let mut burst = vec![0.0; 12_000];
            burst[..3000].copy_from_slice(&sine(60.0, 0.9, 3000, t)[..]);
            t += 12_000;
            for c in burst.chunks(HOP) {
                a.push(c);
                if a.levels.beat > 0.5 && was < 0.5 {
                    beats += 1;
                }
                was = a.levels.beat;
            }
            let _ = k;
        }
        assert!((7..=9).contains(&beats), "{beats} beats");
    }

    #[test]
    fn capture_commands_use_the_monitor() {
        let c = commands("");
        assert!(
            c[0].iter()
                .any(|a| a.contains("stream.capture.sink = true"))
        );
        assert_eq!(c[0].last().unwrap(), "-");
        assert!(c[1].contains(&"--device=@DEFAULT_MONITOR@".to_string()));
        let c = commands("alsa_output.usb");
        assert!(
            c[0].windows(2)
                .any(|w| w == ["--target", "alsa_output.usb"])
        );
        assert!(c[1].contains(&"--device=alsa_output.usb.monitor".to_string()));
    }
}
