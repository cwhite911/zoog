// SPDX-License-Identifier: GPL-3.0-only
// SPDX-FileCopyrightText: 2026 Corey T. White

//! Offline render test: load Surge XT, push a note-on,
//! process blocks with no audio device or hardware, and assert the output is
//! sound-shaped: audible after the attack, decaying after note-off.
//!
//! Skips with a message when no Surge XT CLAP is installed.

use std::ffi::CString;
use std::sync::mpsc::channel;

use clack_host::events::Match;
use clack_host::events::event_types::{NoteOffEvent, NoteOnEvent};
use clack_host::prelude::*;

use lab_engine::audio::{PluginBuffers, query_port_layout};
use lab_engine::discovery::find_plugin;
use lab_engine::host::{BenchHost, BenchHostMainThread, BenchHostShared, host_info};

const SAMPLE_RATE: f64 = 48_000.0;
const FRAMES: usize = 256;
const NOTE_ON_BLOCKS: usize = 100;
const MAX_RELEASE_BLOCKS: usize = 400;

#[test]
fn surge_renders_sound_and_decays() {
    let plugin = match find_plugin("surge") {
        Ok(plugin) => plugin,
        Err(e) => {
            eprintln!("skipping offline render test: {e}");
            return;
        }
    };
    println!("rendering with {plugin}");

    let (host_tx, _host_rx) = channel();
    let plugin_id = CString::new(plugin.id.as_str()).unwrap();
    let mut instance = PluginInstance::<BenchHost>::new(
        |_| BenchHostShared::new(host_tx),
        |_| BenchHostMainThread::new(),
        &plugin.entry,
        &plugin_id,
        &host_info(),
    )
    .expect("plugin instantiation failed");

    let layout_in = query_port_layout(&mut instance, true);
    let layout_out = query_port_layout(&mut instance, false);
    let note_port = lab_engine::events::find_main_note_port(&mut instance)
        .expect("Surge XT should expose a note-in port");
    assert!(
        !note_port.prefers_midi,
        "Surge XT should accept CLAP note events"
    );

    let mut buffers = PluginBuffers::new(layout_in, layout_out, FRAMES);

    let mut processor = instance
        .activate(
            |_, _| (),
            PluginAudioConfiguration {
                sample_rate: SAMPLE_RATE,
                min_frames_count: 1,
                max_frames_count: FRAMES as u32,
            },
        )
        .expect("activation failed")
        .start_processing()
        .expect("start_processing failed");

    let mut steady = 0u64;
    let mut process_block = |buffers: &mut PluginBuffers, events: &InputEvents| -> f64 {
        let (ins, mut outs) = buffers.prepare(FRAMES);
        processor
            .process(
                &ins,
                &mut outs,
                events,
                &mut OutputEvents::void(),
                Some(steady),
                None,
            )
            .expect("process failed");
        steady += FRAMES as u64;
        buffers.main_output_rms(FRAMES)
    };

    // Note on (middle C, full velocity), then hold for NOTE_ON_BLOCKS.
    let mut note_on = EventBuffer::new();
    note_on.push(&NoteOnEvent::new(
        0,
        Pckn::new(note_port.port_index, 0u16, 60u16, Match::All),
        1.0,
    ));

    let mut sustain_rms: f64 = 0.0;
    for i in 0..NOTE_ON_BLOCKS {
        let events = if i == 0 {
            note_on.as_input()
        } else {
            InputEvents::empty()
        };
        let rms = process_block(&mut buffers, &events);
        // Ignore the first blocks (attack); track the loudest block after.
        if i >= 10 {
            sustain_rms = sustain_rms.max(rms);
        }
    }
    assert!(
        sustain_rms > 1e-3,
        "expected audible output while note held, got RMS {sustain_rms}"
    );

    // Note off, then let the tail decay.
    let mut note_off = EventBuffer::new();
    note_off.push(&NoteOffEvent::new(
        0,
        Pckn::new(note_port.port_index, 0u16, 60u16, Match::All),
        0.0,
    ));

    let mut tail_rms = f64::MAX;
    for i in 0..MAX_RELEASE_BLOCKS {
        let events = if i == 0 {
            note_off.as_input()
        } else {
            InputEvents::empty()
        };
        tail_rms = process_block(&mut buffers, &events);
        if tail_rms < sustain_rms * 0.01 {
            break;
        }
    }
    assert!(
        tail_rms < sustain_rms * 0.1,
        "expected tail to decay after note off: sustain RMS {sustain_rms}, tail RMS {tail_rms}"
    );
}
