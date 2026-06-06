# isflac

A small command line tool that checks whether a FLAC file is really lossless or just a lossy file (like an MP3 or AAC) that someone re-wrapped into a FLAC container.

FLAC is a lossless format, but a `.flac` file is only as good as whatever was fed into it. If you take a 320kbps MP3, decode it, and re-encode the result as FLAC, you get a file that looks lossless from the outside (big size, FLAC header, 24bit/48kHz tags) but actually contains lossy audio. The data that the lossy codec threw away is gone for good. These are usually called "transcodes" or "fake FLACs", and they are common on download sites and in shared music libraries.

isflac tries to catch these by looking at the actual audio, not just the header.

## How it works

Lossy codecs save space by cutting off high frequencies that are hard to hear. An MP3 at 128kbps might cut everything above 16kHz. At 320kbps it usually cuts around 20kHz. A genuine lossless file keeps content all the way up toward the Nyquist limit (half the sample rate, so 24kHz for a 48kHz file).

That cutoff leaves a very obvious signature. If you open a transcode in a spectrogram viewer like Spek, you see a sharp horizontal line where the sound just stops, with nothing but noise above it. A real lossless file fades out gradually and keeps energy near the top.

isflac measures that cutoff automatically:

1. Read the first four bytes and confirm they are `fLaC`. If not, it is not a FLAC file at all.
2. Decode the whole first channel of the file into samples.
3. Run a Fast Fourier Transform (FFT) over the audio to see how much energy lives at each frequency.
4. Find the highest frequency that still has real content above the noise floor. That is the cutoff.
5. Compare the cutoff to the Nyquist frequency. If the audio only covers less than about 85 percent of the available range, it is almost certainly a transcode and isflac warns you.

### The signal processing details

Getting this measurement right matters, and there are two easy ways to get it wrong:

- **Windowing.** Feeding raw samples straight into an FFT causes spectral leakage. Strong low frequency content smears energy into every frequency bin, including the ones above a real cutoff. That makes a transcode look full spectrum and pass as genuine. isflac applies a Hann window to each block before the FFT, which keeps the leakage down so the cutoff stays sharp.

- **Sampling enough of the file.** A single FFT taken from the start of a track is misleading, because intros are often quiet or fade in and have no high frequency content. isflac uses Welch's method instead: it slides a window across the entire file with 50 percent overlap, takes an FFT of each block, and averages all of the power spectra together. Averaging crushes the random noise floor while keeping the steady high frequency content, so the cutoff becomes easy to spot.

The cutoff is then taken as the highest frequency bin whose averaged level is within 50dB of the loudest bin in the file.

## Building

You need a Rust toolchain. Then:

```
cargo build --release
```

The binary ends up at `target/release/isflac`.

If you are using Nix, there is a `flake.nix` in the repo, so `nix build` or `nix develop` will work too.

## Usage

```
isflac <file.flac>
```

For example:

```
cargo run --release -- /home/you/Music/song.flac
```

## What the output means

A genuine file prints its real format and exits cleanly:

```
genuine FLAC — 48000Hz / 24bit / 2ch
```

A suspected transcode prints a warning, shows where the content actually stops, and takes a guess at the original lossy source based on the cutoff:

```
WARNING: probably transcoded FLAC
  header says: 48000Hz / 24bit
  actual content cutoff: 20.0 kHz
  probably MP3 ~320kbps
```

Exit codes:

- `0` genuine FLAC
- `1` not a FLAC file, or an error reading it
- `2` probably a transcode

That makes it easy to use in scripts, for example to scan a folder and flag the bad files.

## Limitations

This is a heuristic, not proof. A few things to keep in mind:

- Some real recordings genuinely have little high frequency content. Old recordings, voice, and some instruments roll off naturally and can look like a low bitrate source even though the file is honestly lossless.
- The hardest case is a high bitrate transcode on a 44.1kHz source. An MP3 at 320kbps cuts around 20.5kHz, which on a 44.1kHz file is about 93 percent of the available range and slips past the coverage check. A pure cutoff ratio is not enough to catch every one of these, so treat a "genuine" result on a 44.1kHz file with a little caution.
- The bitrate guess in the warning is only a rough label based on common encoder defaults. The important number is the measured cutoff, not the guessed source.

When in doubt, open the file in a spectrogram viewer and look at it yourself. isflac is meant to do that check quickly and in bulk, not to replace your own eyes.

## Dependencies

- [claxon](https://crates.io/crates/claxon) for decoding FLAC
- [rustfft](https://crates.io/crates/rustfft) for the FFT
- [anyhow](https://crates.io/crates/anyhow) for error handling
