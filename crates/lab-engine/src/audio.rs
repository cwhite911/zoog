//! Audio output: plugin activation, buffer plumbing, and the cpal stream.
//!
//! Adapted from the clack cpal host example's audio/buffers/config modules,
//! simplified to a stereo output stream (the plugin's main output port may
//! be mono or stereo; anything else is rejected for now).

use std::error::Error;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

use clack_extensions::audio_ports::{AudioPortFlags, AudioPortInfoBuffer, PluginAudioPorts};
use clack_host::prelude::*;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, FromSample, Sample, SampleFormat, SizedSample, StreamConfig};
use rtrb::Consumer;

use clack_host::process::StoppedPluginAudioProcessor;

use crate::events::{EventCollector, ParamChange, RtMidi};
use crate::host::BenchHost;

/// A per-loop-slot plugin engine, built on the host thread and installed
/// into the running audio callback (multi-timbral loop playback: each slot
/// keeps the preset it was recorded with).
pub struct SlotEngine {
    pub slot: usize,
    pub processor: StartedPluginAudioProcessor<BenchHost>,
    pub buffers: PluginBuffers,
    pub events: EventCollector,
    pub midi: Consumer<RtMidi>,
}

/// Everything a successful stream start hands back: the stream itself
/// (keep it alive), callback stats, the negotiated configuration, and the
/// slot-engine management handles.
pub type ActiveStream = (cpal::Stream, Arc<AudioStats>, AudioInfo, SlotChannels);

/// Host-side handles for managing slot engines in the audio callback.
pub struct SlotChannels {
    pub install: rtrb::Producer<SlotEngine>,
    pub remove: rtrb::Producer<usize>,
    /// Slot processors handed back for main-thread deactivation.
    pub retired: rtrb::Consumer<(usize, StoppedPluginAudioProcessor<BenchHost>)>,
}

/// Requested engine configuration.
#[derive(Debug, Clone, Copy)]
pub struct EngineConfig {
    pub sample_rate: u32,
    pub buffer_frames: u32,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            buffer_frames: 256,
        }
    }
}

/// Audio-callback health counters, shared with the host thread for display.
/// The audio thread only does atomic stores/adds here.
#[derive(Debug, Default)]
pub struct AudioStats {
    /// Number of process() calls.
    pub callbacks: AtomicU64,
    /// Number of callbacks whose wall time exceeded their frame budget.
    pub overruns: AtomicU64,
    /// Worst callback duration seen, in nanoseconds.
    pub max_callback_ns: AtomicU64,
    /// Backend stream errors (reported on cpal's error callback, e.g. the
    /// ALSA EIO burst PipeWire produces while the stream settles).
    pub stream_errors: AtomicU64,
    /// Smallest block the backend delivered, in frames (0 = none yet).
    pub min_frames: AtomicU64,
    /// Largest block the backend delivered, in frames.
    pub max_frames: AtomicU64,
    /// Total wall time spent inside process(), in nanoseconds.
    pub busy_ns: AtomicU64,
    /// Total frame-time budget of all processed blocks, in nanoseconds.
    /// busy/budget is the DSP load.
    pub budget_ns: AtomicU64,
    /// Peak absolute output sample since last read, as f32 bits.
    /// Readers swap in 0 to consume; the callback does fetch_max on bits,
    /// which orders correctly for non-negative floats.
    pub peak_bits: AtomicU32,
}

/// The negotiated audio stream, for display.
#[derive(Debug, Clone)]
pub struct AudioInfo {
    pub device_name: Option<String>,
    pub sample_rate: u32,
    pub buffer_frames: u32,
    pub sample_format: String,
}

/// Number of channels and per-port channel layout of the plugin's ports.
#[derive(Debug, Clone)]
pub struct PortLayout {
    /// Channel count for each port, in port order.
    pub port_channels: Vec<u16>,
    /// Index of the main port in `port_channels`.
    pub main_port: usize,
}

impl PortLayout {
    fn total_channels(&self) -> usize {
        self.port_channels.iter().map(|&c| c as usize).sum()
    }

    pub fn main_channel_count(&self) -> u16 {
        self.port_channels[self.main_port]
    }
}

/// Queries the plugin's input or output port layout, defaulting to a single
/// stereo port when the extension is missing (as the clack example does).
pub fn query_port_layout(instance: &mut PluginInstance<BenchHost>, is_input: bool) -> PortLayout {
    let handle = instance.plugin_handle();
    let Some(ports) = handle.get_extension::<PluginAudioPorts>() else {
        return PortLayout {
            port_channels: if is_input { vec![] } else { vec![2] },
            main_port: 0,
        };
    };

    let mut buffer = AudioPortInfoBuffer::new();
    let mut port_channels = Vec::new();
    let mut main_port = None;

    for i in 0..ports.count(&handle, is_input) {
        let Some(info) = ports.get(&handle, i, is_input, &mut buffer) else {
            continue;
        };
        if info.flags.contains(AudioPortFlags::IS_MAIN) {
            main_port.get_or_insert(i as usize);
        }
        port_channels.push(info.channel_count as u16);
    }

    if port_channels.is_empty() && !is_input {
        return PortLayout {
            port_channels: vec![2],
            main_port: 0,
        };
    }

    PortLayout {
        port_channels,
        main_port: main_port.unwrap_or(0),
    }
}

/// Per-block audio buffers for the plugin, preallocated for `max_frames`.
pub struct PluginBuffers {
    input_ports: AudioPorts,
    output_ports: AudioPorts,
    input_channels: Vec<Vec<f32>>,
    output_channels: Vec<Vec<f32>>,
    layout_in: PortLayout,
    layout_out: PortLayout,
    max_frames: usize,
}

impl PluginBuffers {
    pub fn new(layout_in: PortLayout, layout_out: PortLayout, max_frames: usize) -> Self {
        Self {
            input_ports: AudioPorts::with_capacity(
                layout_in.total_channels(),
                layout_in.port_channels.len(),
            ),
            output_ports: AudioPorts::with_capacity(
                layout_out.total_channels(),
                layout_out.port_channels.len(),
            ),
            input_channels: layout_in
                .port_channels
                .iter()
                .map(|&c| vec![0.0; max_frames * c as usize])
                .collect(),
            output_channels: layout_out
                .port_channels
                .iter()
                .map(|&c| vec![0.0; max_frames * c as usize])
                .collect(),
            layout_in,
            layout_out,
            max_frames,
        }
    }

    /// Grows the channel buffers if the backend hands us a bigger block than
    /// planned. Allocation here is the accepted, rare exception (same
    /// approach as the clack example).
    fn ensure_frames(&mut self, frames: usize) {
        if frames <= self.max_frames {
            return;
        }
        self.max_frames = frames;
        for (buf, &c) in self
            .input_channels
            .iter_mut()
            .zip(&self.layout_in.port_channels)
        {
            buf.resize(frames * c as usize, 0.0);
        }
        for (buf, &c) in self
            .output_channels
            .iter_mut()
            .zip(&self.layout_out.port_channels)
        {
            buf.resize(frames * c as usize, 0.0);
        }
    }

    /// Prepares zeroed input and output CLAP buffers for one block.
    pub fn prepare(&mut self, frames: usize) -> (InputAudioBuffers<'_>, OutputAudioBuffers<'_>) {
        assert!(frames <= self.max_frames);
        for buf in self.output_channels.iter_mut() {
            buf.fill(0.0);
        }
        for buf in self.input_channels.iter_mut() {
            buf.fill(0.0);
        }
        let max_frames = self.max_frames;

        (
            self.input_ports
                .with_input_buffers(self.input_channels.iter_mut().map(|port_buf| {
                    AudioPortBuffer {
                        latency: 0,
                        channels: AudioPortBufferType::f32_input_only(
                            port_buf
                                .chunks_exact_mut(max_frames)
                                .map(|buffer| InputChannel {
                                    buffer: &mut buffer[..frames],
                                    is_constant: true,
                                }),
                        ),
                    }
                })),
            self.output_ports
                .with_output_buffers(self.output_channels.iter_mut().map(|port_buf| {
                    AudioPortBuffer {
                        latency: 0,
                        channels: AudioPortBufferType::f32_output_only(
                            port_buf
                                .chunks_exact_mut(max_frames)
                                .map(|buf| &mut buf[..frames]),
                        ),
                    }
                })),
        )
    }

    /// Mixes the main output port into an interleaved stereo f32 buffer,
    /// either overwriting or accumulating. A mono main port is duplicated
    /// to both channels.
    pub fn mix_main(&self, frames: usize, out: &mut [f32], accumulate: bool) {
        let main = &self.output_channels[self.layout_out.main_port];
        match self.layout_out.main_channel_count() {
            1 => {
                let (pairs, _) = out.as_chunks_mut::<2>();
                for (frame, &sample) in pairs.iter_mut().zip(main.iter().take(frames)) {
                    if accumulate {
                        frame[0] += sample;
                        frame[1] += sample;
                    } else {
                        frame[0] = sample;
                        frame[1] = sample;
                    }
                }
            }
            _ => {
                let (left, rest) = main.split_at(self.max_frames);
                let right = &rest[..self.max_frames];
                let (pairs, _) = out.as_chunks_mut::<2>();
                for (i, frame) in pairs.iter_mut().take(frames).enumerate() {
                    if accumulate {
                        frame[0] += left[i];
                        frame[1] += right[i];
                    } else {
                        frame[0] = left[i];
                        frame[1] = right[i];
                    }
                }
            }
        }
    }

    /// Peak absolute sample of the main output port over `frames`.
    pub fn main_output_peak(&self, frames: usize) -> f32 {
        let main = &self.output_channels[self.layout_out.main_port];
        let channels = self.layout_out.main_channel_count() as usize;
        let mut peak = 0.0f32;
        for ch in 0..channels {
            let start = ch * self.max_frames;
            for &s in &main[start..start + frames] {
                peak = peak.max(s.abs());
            }
        }
        peak
    }

    /// RMS of the main output port over `frames`, for the offline test.
    pub fn main_output_rms(&self, frames: usize) -> f64 {
        let main = &self.output_channels[self.layout_out.main_port];
        let channels = self.layout_out.main_channel_count() as usize;
        let mut sum = 0.0f64;
        let mut count = 0usize;
        for ch in 0..channels {
            let start = ch * self.max_frames;
            for &s in &main[start..start + frames] {
                sum += (s as f64) * (s as f64);
                count += 1;
            }
        }
        if count == 0 {
            0.0
        } else {
            (sum / count as f64).sqrt()
        }
    }
}

/// Everything the audio callback owns.
pub struct StreamProcessor {
    processor: StartedPluginAudioProcessor<BenchHost>,
    buffers: PluginBuffers,
    events: EventCollector,
    midi: Consumer<RtMidi>,
    looper: Option<Consumer<RtMidi>>,
    params: Option<Consumer<ParamChange>>,
    stats: Arc<AudioStats>,
    sample_rate: u64,
    steady_counter: u64,
    /// Interleaved stereo mix scratch (main + slot engines).
    mix: Vec<f32>,
    slots: Vec<Option<SlotEngine>>,
    slot_install: Consumer<SlotEngine>,
    slot_remove: Consumer<usize>,
    slot_retired: rtrb::Producer<(usize, StoppedPluginAudioProcessor<BenchHost>)>,
}

impl StreamProcessor {
    /// Processes one cpal output block (interleaved stereo).
    fn process<S: Sample + FromSample<f32>>(&mut self, data: &mut [S]) {
        let started = Instant::now();
        let frames = data.len() / 2;
        self.buffers.ensure_frames(frames);
        if self.mix.len() < frames * 2 {
            self.mix.resize(frames * 2, 0.0);
        }
        let mix = &mut self.mix[..frames * 2];

        // Slot engine maintenance: install fresh engines, retire removed
        // ones back to the host thread for deactivation.
        while let Ok(engine) = self.slot_install.pop() {
            let index = engine.slot.min(self.slots.len().saturating_sub(1));
            if let Some(old) = self.slots[index].replace(engine) {
                let _ = self
                    .slot_retired
                    .push((old.slot, old.processor.stop_processing()));
            }
        }
        while let Ok(index) = self.slot_remove.pop() {
            if let Some(old) = self.slots.get_mut(index).and_then(Option::take) {
                let _ = self
                    .slot_retired
                    .push((old.slot, old.processor.stop_processing()));
            }
        }

        let events = self.events.collect(
            &mut self.midi,
            self.looper.as_mut(),
            self.params.as_mut(),
            frames as u64,
        );
        let (ins, mut outs) = self.buffers.prepare(frames);

        match self.processor.process(
            &ins,
            &mut outs,
            &events,
            &mut OutputEvents::void(),
            Some(self.steady_counter),
            None,
        ) {
            Ok(_) => self.buffers.mix_main(frames, mix, false),
            Err(_) => mix.fill(0.0),
        }

        // Slot engines: each replays its loop with its own preset.
        for slot in self.slots.iter_mut().flatten() {
            slot.buffers.ensure_frames(frames);
            let events = slot
                .events
                .collect(&mut slot.midi, None, None, frames as u64);
            let (ins, mut outs) = slot.buffers.prepare(frames);
            if slot
                .processor
                .process(
                    &ins,
                    &mut outs,
                    &events,
                    &mut OutputEvents::void(),
                    Some(self.steady_counter),
                    None,
                )
                .is_ok()
            {
                slot.buffers.mix_main(frames, mix, true);
            }
        }

        let mut peak = 0.0f32;
        for (out, &m) in data.iter_mut().zip(mix.iter()) {
            peak = peak.max(m.abs());
            *out = m.to_sample();
        }
        self.stats
            .peak_bits
            .fetch_max(peak.to_bits(), Ordering::Relaxed);
        self.steady_counter += frames as u64;

        let elapsed_ns = started.elapsed().as_nanos() as u64;
        let budget_ns = frames as u64 * 1_000_000_000 / self.sample_rate;
        self.stats.callbacks.fetch_add(1, Ordering::Relaxed);
        self.stats
            .max_callback_ns
            .fetch_max(elapsed_ns, Ordering::Relaxed);
        if elapsed_ns > budget_ns {
            self.stats.overruns.fetch_add(1, Ordering::Relaxed);
        }
        self.stats.busy_ns.fetch_add(elapsed_ns, Ordering::Relaxed);
        self.stats.budget_ns.fetch_add(budget_ns, Ordering::Relaxed);
        self.stats
            .max_frames
            .fetch_max(frames as u64, Ordering::Relaxed);
        let prev_min = self.stats.min_frames.load(Ordering::Relaxed);
        if prev_min == 0 || (frames as u64) < prev_min {
            self.stats
                .min_frames
                .store(frames as u64, Ordering::Relaxed);
        }
    }
}

#[derive(Debug)]
pub enum AudioError {
    NoOutputDevice,
    NoStereoConfig,
    UnsupportedMainPort(u16),
    Backend(String),
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AudioError::NoOutputDevice => write!(f, "no default audio output device"),
            AudioError::NoStereoConfig => {
                write!(f, "output device offers no supported stereo f32/i16 config")
            }
            AudioError::UnsupportedMainPort(c) => {
                write!(
                    f,
                    "plugin main output port has {c} channels; only 1 or 2 supported"
                )
            }
            AudioError::Backend(e) => write!(f, "audio backend error: {e}"),
        }
    }
}

impl Error for AudioError {}

/// Activates the plugin and starts a stereo cpal output stream feeding from
/// it. Returns the stream (keep it alive) and the shared callback stats.
pub fn activate_to_stream(
    instance: &mut PluginInstance<BenchHost>,
    midi: Consumer<RtMidi>,
    looper: Option<Consumer<RtMidi>>,
    params: Option<Consumer<ParamChange>>,
    config: EngineConfig,
) -> Result<ActiveStream, Box<dyn Error>> {
    let layout_in = query_port_layout(instance, true);
    let layout_out = query_port_layout(instance, false);
    let main_channels = layout_out.main_channel_count();
    if main_channels == 0 || main_channels > 2 {
        return Err(AudioError::UnsupportedMainPort(main_channels).into());
    }

    let note_port = crate::events::find_main_note_port(instance).unwrap_or(
        // A synth without note-in would be useless here, but don't crash:
        // events will simply be sent to port 0 as MIDI.
        crate::events::NotePortConfig {
            port_index: 0,
            prefers_midi: true,
        },
    );

    let cpal_host = cpal::default_host();
    let device = cpal_host
        .default_output_device()
        .ok_or(AudioError::NoOutputDevice)?;

    let (sample_format, sample_rate, buffer_frames) = negotiate(&device, config)?;
    let info = AudioInfo {
        device_name: device.description().ok().map(|d| d.name().to_string()),
        sample_rate,
        buffer_frames,
        sample_format: sample_format.to_string(),
    };

    let stream_config = StreamConfig {
        channels: 2,
        sample_rate,
        buffer_size: BufferSize::Fixed(buffer_frames),
    };

    let processor = instance
        .activate(
            |_, _| (),
            PluginAudioConfiguration {
                sample_rate: sample_rate as f64,
                min_frames_count: 1,
                max_frames_count: buffer_frames.max(1024),
            },
        )?
        .start_processing()?;

    let stats = Arc::new(AudioStats::default());
    let (install_tx, install_rx) = rtrb::RingBuffer::<SlotEngine>::new(16);
    let (remove_tx, remove_rx) = rtrb::RingBuffer::<usize>::new(16);
    let (retired_tx, retired_rx) =
        rtrb::RingBuffer::<(usize, StoppedPluginAudioProcessor<BenchHost>)>::new(16);
    let max_frames = buffer_frames.max(1024) as usize;
    let stream_processor = StreamProcessor {
        processor,
        buffers: PluginBuffers::new(layout_in, layout_out, max_frames),
        events: EventCollector::new(sample_rate as u64, note_port),
        midi,
        looper,
        params,
        stats: stats.clone(),
        sample_rate: sample_rate as u64,
        steady_counter: 0,
        mix: vec![0.0; max_frames * 2],
        slots: (0..8).map(|_| None).collect(),
        slot_install: install_rx,
        slot_remove: remove_rx,
        slot_retired: retired_tx,
    };

    let stream = build_stream(&device, stream_config, sample_format, stream_processor)?;
    stream
        .play()
        .map_err(|e| AudioError::Backend(e.to_string()))?;

    Ok((
        stream,
        stats,
        info,
        SlotChannels {
            install: install_tx,
            remove: remove_tx,
            retired: retired_rx,
        },
    ))
}

/// Picks a supported stereo output configuration closest to the request.
/// Prefers f32, falls back to i16/u16.
fn negotiate(
    device: &cpal::Device,
    config: EngineConfig,
) -> Result<(SampleFormat, u32, u32), AudioError> {
    let mut best: Option<(u8, SampleFormat, u32, u32)> = None;

    let configs = device
        .supported_output_configs()
        .map_err(|e| AudioError::Backend(e.to_string()))?;

    for range in configs {
        if range.channels() != 2 {
            continue;
        }
        let rank = match range.sample_format() {
            SampleFormat::F32 => 0,
            SampleFormat::I16 => 1,
            SampleFormat::U16 => 2,
            _ => continue,
        };
        let rate = config
            .sample_rate
            .clamp(range.min_sample_rate(), range.max_sample_rate());
        let frames = match range.buffer_size() {
            cpal::SupportedBufferSize::Range { min, max } => {
                config.buffer_frames.clamp((*min).max(1), *max)
            }
            cpal::SupportedBufferSize::Unknown => config.buffer_frames,
        };
        if best.as_ref().is_none_or(|(r, ..)| rank < *r) {
            best = Some((rank, range.sample_format(), rate, frames));
        }
    }

    best.map(|(_, f, r, b)| (f, r, b))
        .ok_or(AudioError::NoStereoConfig)
}

fn build_stream(
    device: &cpal::Device,
    config: StreamConfig,
    sample_format: SampleFormat,
    processor: StreamProcessor,
) -> Result<cpal::Stream, AudioError> {
    match sample_format {
        SampleFormat::F32 => build_typed::<f32>(device, config, processor),
        SampleFormat::I16 => build_typed::<i16>(device, config, processor),
        SampleFormat::U16 => build_typed::<u16>(device, config, processor),
        other => Err(AudioError::Backend(format!(
            "unsupported sample format {other:?}"
        ))),
    }
}

fn build_typed<S: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: StreamConfig,
    mut processor: StreamProcessor,
) -> Result<cpal::Stream, AudioError> {
    let error_stats = processor.stats.clone();
    device
        .build_output_stream(
            config,
            move |data: &mut [S], _| processor.process(data),
            move |_| {
                // Counted, not printed: PipeWire produces a burst of ALSA
                // EIO errors while the stream settles at startup.
                error_stats.stream_errors.fetch_add(1, Ordering::Relaxed);
            },
            None,
        )
        .map_err(|e| AudioError::Backend(e.to_string()))
}
