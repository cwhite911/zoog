//! Per-engine macro mapping files (`mappings/<engine>.toml`) and the
//! runtime binding of hardware controls to plugin parameters.
//!
//! A mapping entry references its parameter primarily by CLAP param id
//! (observed via `labctl params`, recorded in the mapping file). The
//! parameter name is stored alongside for readability and as a fallback:
//! exact name match first, then prefix match (Surge macro names mutate when
//! a patch renames them, e.g. "M1: -" becomes "M1: Cutoff").

use std::collections::HashMap;
use std::fmt;
use std::path::Path;

use serde::Deserialize;

use lab_engine::events::ParamChange;
use lab_engine::params::ParamDescription;
use lab_midi::event::DeviceEvent;

use crate::takeover::SoftTakeover;

/// One of the mappable absolute controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Control {
    Encoder(u8),
    Fader(u8),
}

/// The mapping file as written on disk.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct MappingFile {
    /// CLAP plugin id this mapping applies to.
    pub plugin_id: String,
    #[serde(default)]
    pub encoder: Vec<ControlEntry>,
    #[serde(default)]
    pub fader: Vec<ControlEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct ControlEntry {
    /// 1-based hardware index (encoders 1..=8, faders 1..=4).
    pub index: u8,
    /// Short label shown on the device display.
    pub label: String,
    /// CLAP param id, as listed by `labctl params`.
    pub param_id: Option<u32>,
    /// Parameter name, informational and fallback lookup.
    pub param_name: Option<String>,
    /// Optional plain-value range override (defaults to the param's range).
    pub min: Option<f64>,
    pub max: Option<f64>,
}

impl MappingFile {
    pub fn parse(text: &str) -> Result<Self, MappingError> {
        toml::from_str(text).map_err(|e| MappingError::Parse(e.to_string()))
    }

    pub fn load(path: &Path) -> Result<Self, MappingError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| MappingError::Io(path.display().to_string(), e.to_string()))?;
        Self::parse(&text)
    }
}

/// A control bound to a live plugin parameter.
#[derive(Debug, Clone)]
pub struct Binding {
    pub control: Control,
    pub label: String,
    pub param_id: u32,
    /// Plain-value range the 0..=127 control sweeps over.
    pub lo: f64,
    pub hi: f64,
}

impl Binding {
    fn value_for(&self, normalized: f64) -> f64 {
        self.lo + normalized * (self.hi - self.lo)
    }

    fn normalized_of(&self, plain: f64) -> f64 {
        if self.hi == self.lo {
            0.0
        } else {
            ((plain - self.lo) / (self.hi - self.lo)).clamp(0.0, 1.0)
        }
    }
}

/// The result of a handled control move.
#[derive(Debug, Clone, PartialEq)]
pub struct ParamUpdate {
    pub change: ParamChange,
    /// `label` for the display's top line.
    pub label: String,
    /// Percent text for the display's bottom line.
    pub display_value: String,
}

impl fmt::Debug for MappingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

pub enum MappingError {
    Io(String, String),
    Parse(String),
    WrongPlugin { expected: String, actual: String },
    UnresolvedParam { label: String },
    BadIndex { control: &'static str, index: u8 },
}

impl fmt::Display for MappingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MappingError::Io(path, e) => write!(f, "cannot read {path}: {e}"),
            MappingError::Parse(e) => write!(f, "mapping parse error: {e}"),
            MappingError::WrongPlugin { expected, actual } => {
                write!(f, "mapping is for plugin {expected}, loaded {actual}")
            }
            MappingError::UnresolvedParam { label } => {
                write!(f, "mapping entry {label:?} matches no plugin parameter")
            }
            MappingError::BadIndex { control, index } => {
                write!(f, "mapping entry has out-of-range {control} index {index}")
            }
        }
    }
}

impl std::error::Error for MappingError {}

/// Live control surface state: bindings, pickup state, and the last known
/// plain value of each bound parameter.
pub struct MacroControls {
    bindings: HashMap<Control, Binding>,
    takeover: HashMap<Control, SoftTakeover>,
    values: HashMap<u32, f64>,
}

impl MacroControls {
    /// Resolves a mapping file against the plugin's enumerated parameters.
    pub fn bind(
        file: &MappingFile,
        plugin_id: &str,
        params: &[ParamDescription],
    ) -> Result<Self, MappingError> {
        if file.plugin_id != plugin_id {
            return Err(MappingError::WrongPlugin {
                expected: file.plugin_id.clone(),
                actual: plugin_id.to_string(),
            });
        }

        let mut bindings = HashMap::new();
        let mut values = HashMap::new();

        let entries = file
            .encoder
            .iter()
            .map(|e| (e, true))
            .chain(file.fader.iter().map(|e| (e, false)));

        for (entry, is_encoder) in entries {
            let control = if is_encoder {
                if !(1..=8).contains(&entry.index) {
                    return Err(MappingError::BadIndex {
                        control: "encoder",
                        index: entry.index,
                    });
                }
                Control::Encoder(entry.index - 1)
            } else {
                if !(1..=4).contains(&entry.index) {
                    return Err(MappingError::BadIndex {
                        control: "fader",
                        index: entry.index,
                    });
                }
                Control::Fader(entry.index - 1)
            };

            let param =
                resolve_param(entry, params).ok_or_else(|| MappingError::UnresolvedParam {
                    label: entry.label.clone(),
                })?;

            values.insert(param.id, param.value.unwrap_or(param.default_value));
            bindings.insert(
                control,
                Binding {
                    control,
                    label: entry.label.clone(),
                    param_id: param.id,
                    lo: entry.min.unwrap_or(param.min_value),
                    hi: entry.max.unwrap_or(param.max_value),
                },
            );
        }

        Ok(MacroControls {
            takeover: bindings.keys().map(|&c| (c, SoftTakeover::new())).collect(),
            bindings,
            values,
        })
    }

    /// Handles a typed device event. Returns a parameter update when the
    /// event is a bound control move that has picked up its parameter.
    pub fn handle(&mut self, event: &DeviceEvent) -> Option<ParamUpdate> {
        let (control, cc) = match *event {
            DeviceEvent::Encoder { index, value } => (Control::Encoder(index), value),
            DeviceEvent::Fader { index, value } => (Control::Fader(index), value),
            _ => return None,
        };
        let binding = self.bindings.get(&control)?;
        let current = self.values.get(&binding.param_id).copied().unwrap_or(0.0);

        let normalized = self
            .takeover
            .get_mut(&control)?
            .update(cc, binding.normalized_of(current))?;

        let value = binding.value_for(normalized);
        self.values.insert(binding.param_id, value);
        Some(ParamUpdate {
            change: ParamChange {
                param_id: binding.param_id,
                value,
            },
            label: binding.label.clone(),
            display_value: format!("{:.0}%", normalized * 100.0),
        })
    }

    /// Records externally-changed parameter values (preset load) and drops
    /// all pickup latches so controls do not jump.
    pub fn reset_values(&mut self, params: &[ParamDescription]) {
        for param in params {
            if self.values.contains_key(&param.id)
                && let Some(value) = param.value
            {
                self.values.insert(param.id, value);
            }
        }
        for takeover in self.takeover.values_mut() {
            takeover.release();
        }
    }

    pub fn bindings(&self) -> impl Iterator<Item = &Binding> {
        self.bindings.values()
    }
}

/// Finds the parameter an entry refers to: by id first, then exact name,
/// then name prefix.
fn resolve_param<'a>(
    entry: &ControlEntry,
    params: &'a [ParamDescription],
) -> Option<&'a ParamDescription> {
    if let Some(id) = entry.param_id
        && let Some(param) = params.iter().find(|p| p.id == id)
    {
        return Some(param);
    }
    let name = entry.param_name.as_deref()?;
    params
        .iter()
        .find(|p| p.name == name)
        .or_else(|| params.iter().find(|p| p.name.starts_with(name)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAPPING: &str = r#"
plugin-id = "org.example.synth"

[[encoder]]
index = 1
label = "Macro 1"
param-id = 100

[[encoder]]
index = 2
label = "Cutoff"
param-name = "Filter Cutoff"
min = 0.2
max = 0.8

[[fader]]
index = 1
label = "Attack"
param-id = 200
"#;

    fn params() -> Vec<ParamDescription> {
        let base = ParamDescription {
            id: 0,
            name: String::new(),
            module: String::new(),
            min_value: 0.0,
            max_value: 1.0,
            default_value: 0.0,
            value: Some(0.0),
            value_text: None,
            is_automatable: true,
        };
        vec![
            ParamDescription {
                id: 100,
                name: "M1: -".into(),
                ..base.clone()
            },
            ParamDescription {
                id: 101,
                name: "Filter Cutoff".into(),
                value: Some(0.5),
                ..base.clone()
            },
            ParamDescription {
                id: 200,
                name: "Amp EG Attack".into(),
                ..base
            },
        ]
    }

    fn bound() -> MacroControls {
        let file = MappingFile::parse(MAPPING).unwrap();
        MacroControls::bind(&file, "org.example.synth", &params()).unwrap()
    }

    #[test]
    fn parses_and_binds() {
        let controls = bound();
        assert_eq!(controls.bindings().count(), 3);
        let cutoff = controls.bindings().find(|b| b.label == "Cutoff").unwrap();
        // Resolved by name, with the range override applied.
        assert_eq!(cutoff.param_id, 101);
        assert_eq!((cutoff.lo, cutoff.hi), (0.2, 0.8));
    }

    #[test]
    fn wrong_plugin_is_rejected() {
        let file = MappingFile::parse(MAPPING).unwrap();
        assert!(matches!(
            MacroControls::bind(&file, "org.other.synth", &params()),
            Err(MappingError::WrongPlugin { .. })
        ));
    }

    #[test]
    fn unresolved_param_is_an_error() {
        let text = MAPPING.replace("param-id = 100", "param-id = 999");
        let file = MappingFile::parse(&text).unwrap();
        assert!(matches!(
            MacroControls::bind(&file, "org.example.synth", &params()),
            Err(MappingError::UnresolvedParam { .. })
        ));
    }

    #[test]
    fn encoder_move_produces_scaled_update() {
        let mut controls = bound();
        // Macro 1 param is at 0.0; knob starts at 0 -> immediate pickup.
        let update = controls
            .handle(&DeviceEvent::Encoder { index: 0, value: 0 })
            .expect("picked up at matching position");
        assert_eq!(update.change.param_id, 100);
        assert_eq!(update.change.value, 0.0);

        let update = controls
            .handle(&DeviceEvent::Encoder {
                index: 0,
                value: 127,
            })
            .unwrap();
        assert_eq!(update.change.value, 1.0);
        assert_eq!(update.label, "Macro 1");
        assert_eq!(update.display_value, "100%");
    }

    #[test]
    fn range_override_scales_output() {
        let mut controls = bound();
        // Cutoff param is at 0.5 (normalized 0.5 within 0.2..0.8); sweep the
        // encoder up from below to pick it up, then to the top.
        assert!(
            controls
                .handle(&DeviceEvent::Encoder {
                    index: 1,
                    value: 20
                })
                .is_none()
        );
        assert!(
            controls
                .handle(&DeviceEvent::Encoder {
                    index: 1,
                    value: 80
                })
                .is_some()
        );
        let update = controls
            .handle(&DeviceEvent::Encoder {
                index: 1,
                value: 127,
            })
            .unwrap();
        assert!((update.change.value - 0.8).abs() < 1e-9);
    }

    #[test]
    fn takeover_prevents_jump_after_reset() {
        let mut controls = bound();
        controls
            .handle(&DeviceEvent::Fader { index: 0, value: 0 })
            .expect("pickup at 0");
        controls
            .handle(&DeviceEvent::Fader {
                index: 0,
                value: 100,
            })
            .expect("latched");

        // Preset load moves the param to 0.1 externally.
        let mut new_params = params();
        new_params[2].value = Some(0.1);
        controls.reset_values(&new_params);

        // Fader still sits near 100; small move must NOT jump the param.
        assert!(
            controls
                .handle(&DeviceEvent::Fader {
                    index: 0,
                    value: 99
                })
                .is_none()
        );
        // Sweeping down through 0.1 picks it up again.
        assert!(
            controls
                .handle(&DeviceEvent::Fader { index: 0, value: 5 })
                .is_some()
        );
    }

    #[test]
    fn unbound_events_pass_through() {
        let mut controls = bound();
        assert!(
            controls
                .handle(&DeviceEvent::Encoder {
                    index: 7,
                    value: 64
                })
                .is_none()
        );
        assert!(
            controls
                .handle(&DeviceEvent::NoteOn {
                    note: 60,
                    velocity: 100
                })
                .is_none()
        );
    }
}
