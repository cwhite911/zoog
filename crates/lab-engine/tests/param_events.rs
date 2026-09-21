//! Regression test: sending `ParamValueEvent`s during processing must not
//! crash the plugin (crash observed on hardware with Odin2 when moving an
//! encoder). Runs against every installed engine with a zoog mapping;
//! skips engines that are not installed.

use std::ffi::CString;
use std::sync::mpsc::channel;

use clack_extensions::params::{ParamInfoBuffer, PluginParams};
use clack_host::events::Match;
use clack_host::events::event_types::{NoteOnEvent, ParamValueEvent};
use clack_host::prelude::*;

use lab_engine::audio::{PluginBuffers, query_port_layout};
use lab_engine::discovery::find_plugin;
use lab_engine::host::{BenchHost, BenchHostMainThread, BenchHostShared, host_info};

const FRAMES: usize = 256;

fn exercise(plugin_match: &str, param_id: u32, values: &[f64]) {
    let plugin = match find_plugin(plugin_match) {
        Ok(plugin) => plugin,
        Err(e) => {
            eprintln!("skipping {plugin_match}: {e}");
            return;
        }
    };
    println!("exercising {plugin}");

    let (host_tx, _host_rx) = channel();
    let plugin_id = CString::new(plugin.id.as_str()).unwrap();
    let mut instance = PluginInstance::<BenchHost>::new(
        |_| BenchHostShared::new(host_tx),
        |_| BenchHostMainThread::new(),
        &plugin.entry,
        &plugin_id,
        &host_info(),
    )
    .expect("instantiation failed");

    // Fetch the parameter's cookie, as the plugin provided it.
    let cookie = {
        let handle = instance.plugin_handle();
        let params = handle
            .get_extension::<PluginParams>()
            .expect("params extension required");
        let mut buffer = ParamInfoBuffer::new();
        let mut found = None;
        for index in 0..params.count(&handle) {
            if let Some(info) = params.get_info(&handle, index, &mut buffer)
                && info.id.get() == param_id
            {
                found = Some(info.cookie);
                break;
            }
        }
        found.expect("target param must exist")
    };

    let layout_in = query_port_layout(&mut instance, true);
    let layout_out = query_port_layout(&mut instance, false);
    let note_port =
        lab_engine::events::find_main_note_port(&mut instance).expect("note port required");

    let mut buffers = PluginBuffers::new(layout_in, layout_out, FRAMES);
    let mut processor = instance
        .activate(
            |_, _| (),
            PluginAudioConfiguration {
                sample_rate: 48_000.0,
                min_frames_count: 1,
                max_frames_count: FRAMES as u32,
            },
        )
        .expect("activation failed")
        .start_processing()
        .expect("start_processing failed");

    let mut steady = 0u64;
    let mut run = |buffers: &mut PluginBuffers, events: &InputEvents| {
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
    };

    // Hold a note, then sweep the parameter like an encoder move: one
    // event per block, many blocks.
    let mut note_on = EventBuffer::new();
    note_on.push(&NoteOnEvent::new(
        0,
        Pckn::new(note_port.port_index, 0u16, 60u16, Match::All),
        1.0,
    ));
    run(&mut buffers, &note_on.as_input());
    for _ in 0..10 {
        run(&mut buffers, &InputEvents::empty());
    }
    for &value in values {
        let mut events = EventBuffer::new();
        // SAFETY: the cookie comes from this instance's own param_info and
        // no rescan has occurred since it was fetched.
        let event = unsafe {
            ParamValueEvent::new(0, ClapId::new(param_id), Pckn::match_all(), value)
                .with_cookie(cookie)
        };
        events.push(&event);
        run(&mut buffers, &events.as_input());
    }
    for _ in 0..20 {
        run(&mut buffers, &InputEvents::empty());
    }
    println!("{plugin_match}: param sweep survived");
}

#[test]
fn surge_survives_param_sweep() {
    // Macro 1, observed id (mappings/surge-xt.toml).
    let values: Vec<f64> = (0..=100).map(|i| i as f64 / 100.0).collect();
    exercise("Surge XT", 825_615_485, &values);
}

#[test]
fn odin2_survives_param_sweep() {
    // Filter1 Frequency, observed id (mappings/odin2.toml).
    let values: Vec<f64> = (0..=100).map(|i| i as f64 / 100.0).collect();
    exercise("Odin2", 1_489_561_359, &values);
}
