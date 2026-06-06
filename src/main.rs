use claxon::FlacReader;
use rustfft::{num_complex::Complex, FftPlanner};
use std::path::{Path, PathBuf};

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

    let target_frames = 2000;
    let span = samples.len().saturating_sub(fft_size);
    let step = (span / target_frames).max(fft_size / 2);
    let mut magnitudes = vec![0f32; half];
    let mut buffer: Vec<Complex<f32>> = vec![Complex::default(); fft_size];

    let mut start = 0;
    while start + fft_size <= samples.len() {
        for i in 0..fft_size {
            buffer[i] = Complex {
                re: samples[start + i] * window[i],
                im: 0.0,
            };
        }
        fft.process(&mut buffer);
        for (m, c) in magnitudes.iter_mut().zip(buffer[..half].iter()) {
            let v = c.norm();
            if v > *m {
                *m = v;
            }
        }
        start += step;
    }

    let max_mag = magnitudes.iter().cloned().fold(0f32, f32::max).max(1e-12);
    let bin_hz = sample_rate as f32 / fft_size as f32;

    let db: Vec<f32> = magnitudes
        .iter()
        .map(|&m| 20.0 * (m.max(1e-12) / max_mag).log10())
        .collect();

    let smooth_bins = ((180.0 / bin_hz) as usize).max(1);
    let db_smooth = moving_average(&db, smooth_bins);

    let noise_floor = percentile(&db_smooth, 0.02);
    let dead_threshold = noise_floor + 8.0;

    let mut cutoff_bin = half - 1;
    while cutoff_bin > 0 && db_smooth[cutoff_bin] <= dead_threshold {
        cutoff_bin -= 1;
    }

    let dead_zone_khz = (half - 1 - cutoff_bin) as f32 * bin_hz / 1000.0;
    let cutoff = cutoff_bin as f32 * bin_hz / 1000.0;
    let nyquist_khz = nyquist / 1000.0;
    let coverage = cutoff / nyquist_khz;

    let probe_gap = ((1000.0 / bin_hz) as usize).max(1);
    let probe = (cutoff_bin + probe_gap).min(half - 1);
    let edge_drop = db_smooth[cutoff_bin] - db_smooth[probe];

    let is_brick_wall = dead_zone_khz >= 1.5 && coverage < 0.94 && edge_drop >= 12.0;

    if std::env::var("ISFLAC_DEBUG").is_ok() {
        eprintln!(
            "noise_floor={:.1} dead_zone={:.1}kHz cutoff={:.1}kHz coverage={:.2} edge_drop={:.1} brick={}",
            noise_floor, dead_zone_khz, cutoff, coverage, edge_drop, is_brick_wall
        );
        for khz in 1..(nyquist as u32 / 1000) {
            let bin = (khz as f32 * 1000.0 / bin_hz) as usize;
            if bin < half {
                eprintln!("{:>3} kHz: {:7.1} dB", khz, db_smooth[bin]);
            }
        }
    }

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

fn collect_flacs(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                collect_flacs(&p, out);
            } else if p
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("flac"))
                .unwrap_or(false)
            {
                out.push(p);
            }
        }
    }
}

fn scan_one(path: &str) -> i32 {
    match is_flac(path) {
        Ok(FlacResult::NotFlac) => {
            eprintln!("not a FLAC file (bad magic bytes)");
            1
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
            0
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
            2
        }
        Err(e) => {
            eprintln!("error: {}", e);
            1
        }
    }
}

fn scan_folder(dir: &Path) -> i32 {
    let mut files = Vec::new();
    collect_flacs(dir, &mut files);
    files.sort();

    if files.is_empty() {
        eprintln!("no FLAC files found in {}", dir.display());
        return 1;
    }

    let (mut genuine, mut fake, mut errors) = (0, 0, 0);

    for file in &files {
        let name = file.display();
        match is_flac(&file.to_string_lossy()) {
            Ok(FlacResult::TrueFlac { .. }) => {
                println!("GENUINE  {}", name);
                genuine += 1;
            }
            Ok(FlacResult::NotFlac) => {
                println!("NOTFLAC  {}", name);
                errors += 1;
            }
            Ok(FlacResult::FakeFlac { cutoff, source, .. }) => {
                println!("FAKE     {}  ({:.1} kHz, {})", name, cutoff, source);
                fake += 1;
            }
            Err(e) => {
                println!("ERROR    {}  ({})", name, e);
                errors += 1;
            }
        }
    }

    println!(
        "\nscanned {} files: {} genuine, {} transcoded, {} errors",
        files.len(),
        genuine,
        fake,
        errors
    );

    if fake > 0 { 2 } else { 0 }
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: isflac <file.flac | folder>");
    let p = Path::new(&path);
    let code = if p.is_dir() {
        scan_folder(p)
    } else {
        scan_one(&path)
    };
    std::process::exit(code);
}
