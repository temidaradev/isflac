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

    let max_mag = magnitudes.iter().cloned().fold(0f32, f32::max);
    let threshold = max_mag * 10f32.powf(-50.0 / 20.0);

    let last_active_bin = magnitudes
        .iter()
        .rposition(|&m| m > threshold)
        .unwrap_or(half - 1);

    let cutoff = (last_active_bin as f32 / fft_size as f32) * sample_rate as f32 / 1000.0;
    let nyquist_khz = nyquist / 1000.0;

    let coverage = cutoff / nyquist_khz;

    if coverage < 0.85 {
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
