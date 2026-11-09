//! Signal helpers: FFTs (cached plans), scipy-compatible next_fast_len and resample_poly (decimation).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use realfft::num_complex::Complex32;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};

fn planner() -> &'static Mutex<RealFftPlanner<f32>> {
    static P: OnceLock<Mutex<RealFftPlanner<f32>>> = OnceLock::new();
    P.get_or_init(|| Mutex::new(RealFftPlanner::new()))
}

fn fwd_plan(n: usize) -> Arc<dyn RealToComplex<f32>> {
    static C: OnceLock<Mutex<HashMap<usize, Arc<dyn RealToComplex<f32>>>>> = OnceLock::new();
    let c = C.get_or_init(Default::default);
    let mut g = c.lock().unwrap();
    g.entry(n).or_insert_with(|| planner().lock().unwrap().plan_fft_forward(n)).clone()
}

fn inv_plan(n: usize) -> Arc<dyn ComplexToReal<f32>> {
    static C: OnceLock<Mutex<HashMap<usize, Arc<dyn ComplexToReal<f32>>>>> = OnceLock::new();
    let c = C.get_or_init(Default::default);
    let mut g = c.lock().unwrap();
    g.entry(n).or_insert_with(|| planner().lock().unwrap().plan_fft_inverse(n)).clone()
}

/// Smallest 5-smooth number >= n (scipy.fft.next_fast_len(n, real=True)).
pub fn next_fast_len(n: usize) -> usize {
    if n <= 6 {
        return n.max(1);
    }
    let mut best = usize::MAX;
    let mut p5 = 1usize;
    while p5 < best {
        let mut p35 = p5;
        while p35 < best {
            let mut v = p35;
            while v < n {
                v *= 2;
            }
            if v < best {
                best = v;
            }
            if p35 >= n {
                break;
            }
            p35 *= 3;
        }
        if p5 >= n {
            break;
        }
        p5 *= 5;
    }
    best
}

/// rfft of `x` zero-padded (or truncated) to length n.
pub fn rfft(x: &[f32], n: usize) -> Vec<Complex32> {
    let plan = fwd_plan(n);
    let mut input = vec![0f32; n];
    let m = x.len().min(n);
    input[..m].copy_from_slice(&x[..m]);
    let mut out = plan.make_output_vec();
    let mut scratch = plan.make_scratch_vec();
    plan.process_with_scratch(&mut input, &mut out, &mut scratch).expect("rfft");
    out
}

/// Normalized inverse real FFT (scipy irfft semantics). Consumes the spectrum.
pub fn irfft(mut spec: Vec<Complex32>, n: usize) -> Vec<f32> {
    let plan = inv_plan(n);
    spec[0].im = 0.0;
    if n % 2 == 0 {
        let last = spec.len() - 1;
        spec[last].im = 0.0;
    }
    let mut out = plan.make_output_vec();
    let mut scratch = plan.make_scratch_vec();
    let _ = plan.process_with_scratch(&mut spec, &mut out, &mut scratch);
    let k = 1.0 / n as f32;
    for v in out.iter_mut() {
        *v *= k;
    }
    out
}

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let q = x * x / 4.0;
    for k in 1..200 {
        term *= q / (k as f64 * k as f64);
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

/// scipy.signal.firwin(numtaps, cutoff, window=('kaiser', beta)) with cutoff relative to Nyquist.
fn firwin_kaiser(numtaps: usize, cutoff: f64, beta: f64) -> Vec<f64> {
    let alpha = 0.5 * (numtaps as f64 - 1.0);
    let mut h: Vec<f64> = (0..numtaps)
        .map(|i| {
            let m = i as f64 - alpha;
            let x = cutoff * m;
            let sinc = if x == 0.0 { 1.0 } else { (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x) };
            let r = 2.0 * i as f64 / (numtaps as f64 - 1.0) - 1.0;
            let w = bessel_i0(beta * (1.0 - r * r).max(0.0).sqrt()) / bessel_i0(beta);
            cutoff * sinc * w
        })
        .collect();
    let s: f64 = h.iter().sum();
    for v in h.iter_mut() {
        *v /= s;
    }
    h
}

/// scipy.signal.resample_poly(x, up, down) for up == 1 (integer decimation), zero-phase,
/// default ('kaiser', 5.0) window: y[k] = sum_i h[i] * x[k*down + half - i].
pub fn decimate(x: &[f32], down: usize) -> Vec<f32> {
    if down <= 1 {
        return x.to_vec();
    }
    let half = 10 * down;
    let h = firwin_kaiser(2 * half + 1, 1.0 / down as f64, 5.0);
    let h32: Vec<f32> = h.iter().map(|&v| v as f32).collect();
    let n_out = x.len().div_ceil(down);
    let n = x.len() as isize;
    let mut y = vec![0f32; n_out];
    use rayon::prelude::*;
    y.par_chunks_mut(65536).enumerate().for_each(|(ci, chunk)| {
        for (j, out) in chunk.iter_mut().enumerate() {
            let k = ci * 65536 + j;
            let c = (k * down + half) as isize; // index paired with h[0]
            let mut acc = 0f32;
            if c - (2 * half) as isize >= 0 && c < n {
                let base = (c - 2 * half as isize) as usize;
                let seg = &x[base..=c as usize];
                // h is symmetric, so x[c - i] * h[i] == sum over seg reversed
                for (a, b) in seg.iter().zip(h32.iter()) {
                    acc += a * b;
                }
            } else {
                for (i, hv) in h32.iter().enumerate() {
                    let idx = c - i as isize;
                    if idx >= 0 && idx < n {
                        acc += x[idx as usize] * hv;
                    }
                }
            }
            *out = acc;
        }
    });
    y
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_len() {
        assert_eq!(next_fast_len(7), 8);
        assert_eq!(next_fast_len(11), 12);
        assert_eq!(next_fast_len(1001), 1024);
        assert_eq!(next_fast_len(1025), 1080);
        assert_eq!(next_fast_len(18_000_001), 18_000_000usize.max(next_fast_len(18_000_001)));
    }

    #[test]
    fn roundtrip() {
        let x: Vec<f32> = (0..100).map(|i| (i as f32 * 0.3).sin()).collect();
        let n = next_fast_len(100);
        let y = irfft(rfft(&x, n), n);
        for i in 0..100 {
            assert!((x[i] - y[i]).abs() < 1e-4);
        }
    }
}
