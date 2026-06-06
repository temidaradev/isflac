use claxon::FlacReader;
use rustfft::{num_complex::Complex, FftPlanner};

#[derive(Debug)]
pub enum FlacResult {
    NotFlac,
    TrueFlac {
        sample_rate: u32,
        bit_depth: u32,
        channels: u32,
    },
    FakeFlac {
        sample_rate: u32,
        bit_depth: u32,
        cutoff: f32,
        source: &'static str,
    },
}

fn is_flac(path: &str) -> anyhow::Result<FlacResult> {
    {
        use std::io::Read;
        let mut f = std::fs::File::open(path)?;
        let mut magic = [0u8; 4];
        f.read_exact(&mut magic)?;
        if &magic != b"fLaC" {
            return Ok(FlacResult::NotFlac);
        }
    }

    let mut reader = FlacReader::open(path)?;
    let info = reader.streaminfo();
    let sample_rate = info.sample_rate;
    let bit_depth = info.bits_per_sample;
    let channels = info.channels;

    let nyquist = (sample_rate / 2) as f32;
    let fft_size = 8192usize;
    let scale = 1.0 / (1i64 << (bit_depth - 1)) as f32;

    let mut samples: Vec<f32> = Vec::new();
    while let Ok(Some(block)) = reader.blocks().read_next_or_eof(vec![]) {
        for &s in block.channel(0) {
            samples.push(s as f32 * scale);
        }
    }

    if samples.len() < fft_size {
        return Ok(FlacResult::TrueFlac {
            sample_rate,
            bit_depth,
            channels,
        });
    }

    let half = fft_size / 2;

    let window: Vec<f32> = (0..fft_size)
        .map(|n| {
            let x = std::f32::consts::PI * 2.0 * n as f32 / (fft_size as f32 - 1.0);
            0.5 - 0.5 * x.cos()
        })
        .collect();

    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(fft_size);

    let step = fft_size / 2;
    let mut power = vec![0f64; half];
    let mut buffer: Vec<Complex<f32>> = vec![Complex::default(); fft_size];
    let mut frames = 0u32;

    let mut start = 0;
    while start + fft_size <= samples.len() {
        for i in 0..fft_size {
            buffer[i] = Complex {
                re: samples[start + i] * window[i],
                im: 0.0,
            };
        }
        fft.process(&mut buffer);
        for (p, c) in power.iter_mut().zip(buffer[..half].iter()) {
            *p += c.norm_sqr() as f64;
        }
        frames += 1;
        start += step;
    }

    let magnitudes: Vec<f32> = power
        .iter()
        .map(|&p| (p / frames as f64).sqrt() as f32)
        .collect();

    let max_mag = magnitudes.iter().cloned().fold(0f32, f32::max).max(1e-12);
    let bin_hz = sample_rate as f32 / fft_size as f32;

    let db: Vec<f32> = magnitudes
        .iter()
        .map(|&m| 20.0 * (m.max(1e-12) / max_mag).log10())
        .collect();

    let smooth_bins = ((180.0 / bin_hz) as usize).max(1);
    let db_smooth = moving_average(&db, smooth_bins);

    let floor_db = percentile(&db_smooth, 0.05);

    let gap = ((1500.0 / bin_hz) as usize).max(1);
    let scan_start = ((3000.0 / bin_hz) as usize).min(half);
    let mut cliff_bin = 0usize;
    let mut cliff_drop = 0f32;
    let mut i = scan_start;
    while i + gap < half {
        let drop = db_smooth[i] - db_smooth[i + gap];
        if drop > cliff_drop {
            cliff_drop = drop;
            cliff_bin = i;
        }
        i += 1;
    }

    let above_start = (cliff_bin + gap).min(half);
    let above_avg = if above_start < half {
        db_smooth[above_start..].iter().sum::<f32>() / (half - above_start) as f32
    } else {
        floor_db
    };

    let content_top = db_smooth
        .iter()
        .rposition(|&d| d > floor_db + 10.0)
        .unwrap_or(half - 1);
    let cutoff = content_top as f32 * bin_hz / 1000.0;
    let nyquist_khz = nyquist / 1000.0;
    let coverage = cutoff / nyquist_khz;

    let is_brick_wall = cliff_drop >= 30.0 && above_avg <= floor_db + 15.0 && coverage < 0.92;

    if is_brick_wall {
        let source = classify_cutoff(cutoff);
        return Ok(FlacResult::FakeFlac {
            sample_rate,
            bit_depth,
            cutoff,
            source,
        });
    }

    Ok(FlacResult::TrueFlac {
        sample_rate,
        bit_depth,
        channels,
    })
}

fn moving_average(data: &[f32], window: usize) -> Vec<f32> {
    let half = window / 2;
    (0..data.len())
        .map(|i| {
            let lo = i.saturating_sub(half);
            let hi = (i + half + 1).min(data.len());
            data[lo..hi].iter().sum::<f32>() / (hi - lo) as f32
        })
        .collect()
}

fn percentile(data: &[f32], p: f32) -> f32 {
    let mut sorted: Vec<f32> = data.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let idx = ((sorted.len() as f32 - 1.0) * p).round() as usize;
    sorted[idx]
}

fn classify_cutoff(khz: f32) -> &'static str {
    match khz as u32 {
        0..=11 => "probably encoded at very low bitrate (phone/voice)",
        12..=15 => "probably MP3 ~128kbps or AAC low bitrate",
        16..=17 => "probably MP3 ~192kbps",
        18..=19 => "probably MP3 ~256kbps or AAC 128-192kbps",
        20..=21 => "probably MP3 ~320kbps",
        _ => "suspicious cutoff, possibly transcoded",
    }
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: isflac <file.flac>");
    match is_flac(&path) {
        Ok(FlacResult::NotFlac) => {
            eprintln!("not a FLAC file (bad magic bytes)");
            std::process::exit(1);
        }
        Ok(FlacResult::TrueFlac {
            sample_rate,
            bit_depth,
            channels,
        }) => {
            println!(
                "genuine FLAC — {}Hz / {}bit / {}ch",
                sample_rate, bit_depth, channels
            );
        }
        Ok(FlacResult::FakeFlac {
            sample_rate,
            bit_depth,
            cutoff,
            source,
        }) => {
            println!(
                "WARNING: probably transcoded FLAC\n  header says: {}Hz / {}bit\n  actual content cutoff: {:.1} kHz\n  {}",
                sample_rate, bit_depth, cutoff, source
            );
            std::process::exit(2);
        }
        Err(e) => {
            eprintln!("error: {}", e);
            std::process::exit(1);
        }
    }
}
