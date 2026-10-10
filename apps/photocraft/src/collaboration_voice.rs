//! Opt-in native push-to-talk. Bounded 20ms mono G.711 µ-law frames travel on the
//! room's disposable WebRTC channel; drawing acknowledgements never delay speech.
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::{HashMap, VecDeque};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

const RATE: u32 = 16_000;
const FRAME: usize = 320;
const QUEUE: usize = FRAME * 8;
type Mixer = Arc<Mutex<HashMap<String, Playback>>>;

#[derive(Default)]
struct Playback {
    samples: VecDeque<f32>,
    sequence: Option<u32>,
}

pub struct Voice {
    _input: cpal::Stream,
    _output: cpal::Stream,
    talking: Arc<AtomicBool>,
    packets: mpsc::Receiver<Vec<u8>>,
    errors: mpsc::Receiver<String>,
    mixer: Mixer,
}

impl Voice {
    pub fn open() -> Result<Self, String> {
        let host = cpal::default_host();
        let input = host.default_input_device().ok_or("No microphone is available")?;
        let output = host.default_output_device().ok_or("No speaker is available")?;
        let ic = input.default_input_config().map_err(|e| e.to_string())?;
        let oc = output.default_output_config().map_err(|e| e.to_string())?;
        let talking = Arc::new(AtomicBool::new(false));
        let mixer: Mixer = Default::default();
        let (tx, packets) = mpsc::sync_channel(8);
        let (error_tx, errors) = mpsc::sync_channel(8);
        let input_stream = match ic.sample_format() {
            cpal::SampleFormat::F32 => capture::<f32>(&input, &ic.into(), talking.clone(), tx, error_tx.clone()),
            cpal::SampleFormat::I16 => capture::<i16>(&input, &ic.into(), talking.clone(), tx, error_tx.clone()),
            cpal::SampleFormat::U16 => capture::<u16>(&input, &ic.into(), talking.clone(), tx, error_tx.clone()),
            other => return Err(format!("Microphone format {other:?} is unsupported")),
        }?;
        let output_stream = match oc.sample_format() {
            cpal::SampleFormat::F32 => playback::<f32>(&output, &oc.into(), mixer.clone(), error_tx),
            cpal::SampleFormat::I16 => playback::<i16>(&output, &oc.into(), mixer.clone(), error_tx),
            cpal::SampleFormat::U16 => playback::<u16>(&output, &oc.into(), mixer.clone(), error_tx),
            other => return Err(format!("Speaker format {other:?} is unsupported")),
        }?;
        input_stream.play().map_err(|e| e.to_string())?;
        output_stream.play().map_err(|e| e.to_string())?;
        Ok(Self { _input: input_stream, _output: output_stream, talking, packets, errors, mixer })
    }

    pub fn set_talking(&self, talking: bool) {
        if !talking {
            while self.packets.try_recv().is_ok() {}
        }
        self.talking.store(talking, Ordering::Relaxed);
    }

    pub fn take_packet(&self) -> Option<Vec<u8>> {
        self.packets.try_recv().ok()
    }
    pub fn take_error(&self) -> Option<String> {
        self.errors.try_recv().ok()
    }

    pub fn receive(&self, peer: &str, packet: &[u8]) -> Result<(), String> {
        let (sequence, samples) = decode_frame(packet)?;
        let mut mixer = self.mixer.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if !mixer.contains_key(peer) && mixer.len() >= 64 {
            return Err("Voice participant limit reached".into());
        }
        let p = mixer.entry(peer.to_owned()).or_default();
        if p.sequence.is_some_and(|prev| sequence.wrapping_sub(prev) > u32::MAX / 2 || sequence == prev) {
            return Ok(());
        }
        p.sequence = Some(sequence);
        if p.samples.len().saturating_add(samples.len()) > QUEUE {
            p.samples.clear();
        }
        p.samples.extend(samples);
        Ok(())
    }
}

fn capture<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    talking: Arc<AtomicBool>,
    packets: mpsc::SyncSender<Vec<u8>>,
    errors: mpsc::SyncSender<String>,
) -> Result<cpal::Stream, String>
where
    T: cpal::SizedSample + Copy,
    f32: cpal::FromSample<T>,
{
    let channels = usize::from(config.channels).max(1);
    let source_rate = config.sample_rate.0.max(1);
    let mut phase = 0u64;
    let mut frame = Vec::with_capacity(FRAME);
    let mut sequence = 0u32;
    device
        .build_input_stream(
            config,
            move |data: &[T], _| {
                if !talking.load(Ordering::Relaxed) {
                    frame.clear();
                    phase = 0;
                    return;
                }
                for sample in data.chunks(channels) {
                    let mono = sample.iter().map(|s| <f32 as cpal::FromSample<T>>::from_sample_(*s)).sum::<f32>() / channels as f32;
                    phase = phase.saturating_add(u64::from(RATE));
                    while phase >= u64::from(source_rate) {
                        phase -= u64::from(source_rate);
                        frame.push(mulaw_encode(mono));
                        if frame.len() == FRAME {
                            let mut packet = Vec::with_capacity(FRAME + 5);
                            packet.push(1);
                            packet.extend_from_slice(&sequence.to_le_bytes());
                            packet.extend_from_slice(&frame);
                            let _ = packets.try_send(packet);
                            sequence = sequence.wrapping_add(1);
                            frame.clear();
                        }
                    }
                }
            },
            move |e| {
                let _ = errors.try_send(e.to_string());
            },
            None,
        )
        .map_err(|e| e.to_string())
}

fn playback<T>(device: &cpal::Device, config: &cpal::StreamConfig, mixer: Mixer, errors: mpsc::SyncSender<String>) -> Result<cpal::Stream, String>
where
    T: cpal::SizedSample + cpal::FromSample<f32> + Copy,
{
    let channels = usize::from(config.channels).max(1);
    let output_rate = config.sample_rate.0.max(1);
    let mut phase = u64::from(output_rate);
    let mut current = 0.0f32;
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _| {
                // Never wait for network/UI work on the audio callback.
                let Ok(mut voices) = mixer.try_lock() else {
                    data.fill(T::from_sample_(0.0));
                    return;
                };
                for frame in data.chunks_mut(channels) {
                    phase = phase.saturating_add(u64::from(RATE));
                    while phase >= u64::from(output_rate) {
                        phase -= u64::from(output_rate);
                        current = voices.values_mut().filter_map(|p| p.samples.pop_front()).sum::<f32>().clamp(-1.0, 1.0);
                    }
                    frame.fill(T::from_sample_(current));
                }
            },
            move |e| {
                let _ = errors.try_send(e.to_string());
            },
            None,
        )
        .map_err(|e| e.to_string())
}

fn mulaw_encode(value: f32) -> u8 {
    let value = if value.is_finite() { value.clamp(-1.0, 1.0) } else { 0.0 };
    let mut sample = (value * 32767.0) as i32;
    let sign = if sample < 0 {
        sample = -sample;
        0x80
    } else {
        0
    };
    sample = sample.min(32635) + 132;
    let mut exponent = 7u8;
    let mut mask = 0x4000;
    while exponent > 0 && sample & mask == 0 {
        exponent -= 1;
        mask >>= 1;
    }
    let mantissa = ((sample >> (u32::from(exponent) + 3)) & 0xf) as u8;
    !(sign | (exponent << 4) | mantissa)
}

fn mulaw_decode(value: u8) -> f32 {
    let value = !value;
    let sample = ((i32::from(value & 0x0f) << 3) + 132) << u32::from((value >> 4) & 7);
    let sample = sample - 132;
    (if value & 0x80 != 0 { -sample } else { sample }) as f32 / 32768.0
}

fn decode_frame(packet: &[u8]) -> Result<(u32, Vec<f32>), String> {
    if packet.len() != FRAME + 5 || packet.first() != Some(&1) {
        return Err("Invalid voice frame".into());
    }
    let bytes: [u8; 4] = packet.get(1..5).ok_or("Truncated voice frame")?.try_into().map_err(|_| "Invalid voice sequence")?;
    Ok((u32::from_le_bytes(bytes), packet.get(5..).ok_or("Truncated voice samples")?.iter().map(|s| mulaw_decode(*s)).collect()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn codec_preserves_sign_and_speech_amplitude() {
        for v in [-1.0f32, -0.5, -0.01, 0.0, 0.01, 0.5, 1.0] {
            assert!((mulaw_decode(mulaw_encode(v)) - v).abs() < 0.04);
        }
        assert_eq!(mulaw_encode(f32::NAN), mulaw_encode(0.0));
    }
    #[test]
    fn malformed_or_oversized_frames_fail() {
        for packet in [vec![], vec![1; 4], vec![1; FRAME + 6]] {
            assert!(decode_frame(&packet).is_err());
        }
        let mut packet = vec![1];
        packet.extend_from_slice(&42u32.to_le_bytes());
        packet.extend(vec![mulaw_encode(0.3); FRAME]);
        let (sequence, samples) = decode_frame(&packet).unwrap();
        assert_eq!(sequence, 42);
        assert_eq!(samples.len(), FRAME);
    }
}
