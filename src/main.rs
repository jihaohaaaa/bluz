use std::{
    collections::HashMap,
    error::Error,
    io,
    process::Command,
    thread,
    time::{Duration, Instant},
};

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState},
};
use serde_json::Value;
use zbus::{blocking::Connection, blocking::fdo::ObjectManagerProxy, zvariant::OwnedValue};

const BLUEZ_SERVICE: &str = "org.bluez";
const BLUEZ_ROOT: &str = "/";
const DEVICE_INTERFACE: &str = "org.bluez.Device1";
const REFRESH_INTERVAL: Duration = Duration::from_secs(2);

const AUDIO_UUIDS: &[(&str, &str)] = &[
    ("00001108-0000-1000-8000-00805f9b34fb", "Headset"),
    ("0000110b-0000-1000-8000-00805f9b34fb", "Audio Sink"),
    ("0000110e-0000-1000-8000-00805f9b34fb", "A/V Remote"),
    ("0000111e-0000-1000-8000-00805f9b34fb", "Handsfree"),
    (
        "0000111f-0000-1000-8000-00805f9b34fb",
        "Handsfree Audio Gateway",
    ),
    ("00001203-0000-1000-8000-00805f9b34fb", "Generic Audio"),
];

#[derive(Clone, Debug)]
struct Headphone {
    name: String,
    address: String,
    codec: String,
    card_name: Option<String>,
    active_profile: Option<String>,
    codec_options: Vec<CodecOption>,
    path: String,
}

#[derive(Clone, Debug)]
struct CodecOption {
    label: String,
    profile: String,
}

#[derive(Clone, Debug, Default)]
struct AudioCard {
    card_name: String,
    active_profile: Option<String>,
    codec: Option<String>,
    codec_options: Vec<CodecOption>,
}

#[derive(Debug)]
struct App {
    devices: Vec<Headphone>,
    selected: usize,
    status: String,
    last_refresh: Instant,
}

impl App {
    fn new() -> Self {
        Self {
            devices: Vec::new(),
            selected: 0,
            status: "Press r to refresh, q to quit".to_string(),
            last_refresh: Instant::now() - REFRESH_INTERVAL,
        }
    }

    fn refresh(&mut self) {
        match connected_headphones() {
            Ok(devices) => {
                let count = devices.len();
                self.devices = devices;
                self.selected = self.selected.min(self.devices.len().saturating_sub(1));
                self.status =
                    format!("Connected bluetooth headphones: {count} | ↑/↓ select | ←/→ codec");
            }
            Err(err) => {
                self.devices.clear();
                self.selected = 0;
                self.status = format!("BlueZ query failed: {err}");
            }
        }

        self.last_refresh = Instant::now();
    }

    fn select_next(&mut self) {
        if !self.devices.is_empty() {
            self.selected = (self.selected + 1).min(self.devices.len() - 1);
        }
    }

    fn select_previous(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    fn switch_selected_codec(&mut self, delta: isize) {
        let Some(device) = self.devices.get(self.selected) else {
            self.status = "No bluetooth headphones available".to_string();
            return;
        };
        let Some(current_index) = selected_codec_index(device) else {
            self.status = format!("{} has no switchable codec profiles", device.name);
            return;
        };
        let next_index = (current_index as isize + delta)
            .rem_euclid(device.codec_options.len() as isize) as usize;

        self.switch_selected_codec_to(next_index);
    }

    fn switch_selected_codec_to(&mut self, option_index: usize) {
        let Some(device) = self.devices.get(self.selected) else {
            self.status = "No bluetooth headphones available".to_string();
            return;
        };
        let selected_address = device.address.clone();
        let selected_name = device.name.clone();

        let Some(card_name) = device.card_name.clone() else {
            self.status = format!("{} has no PulseAudio/PipeWire card", device.name);
            return;
        };

        if device.codec_options.is_empty() {
            self.status = format!("{} has no switchable codec profiles", device.name);
            return;
        }

        let Some(option) = device.codec_options.get(option_index).cloned() else {
            self.status = format!("{} has no codec option {}", device.name, option_index + 1);
            return;
        };

        match set_card_profile(&card_name, &option.profile) {
            Ok(()) => {
                thread::sleep(Duration::from_secs(2));
                if !self.refresh_after_switch(&selected_address, &selected_name, &option, false)
                    && option.profile == "a2dp-sink"
                {
                    match reconnect_bluetooth_device(&selected_address) {
                        Ok(()) => {
                            thread::sleep(Duration::from_secs(2));
                            self.refresh_after_switch(
                                &selected_address,
                                &selected_name,
                                &option,
                                true,
                            );
                        }
                        Err(err) => {
                            self.status =
                                format!("Requested LDAC, but bluetooth reconnect failed: {err}");
                        }
                    }
                }
                self.last_refresh = Instant::now();
            }
            Err(err) => {
                self.status = format!("Failed to switch {}: {err}", device.name);
            }
        }
    }

    fn refresh_after_switch(
        &mut self,
        selected_address: &str,
        selected_name: &str,
        option: &CodecOption,
        used_reconnect: bool,
    ) -> bool {
        match connected_headphones() {
            Ok(devices) => {
                self.devices = devices;
                self.selected = self
                    .devices
                    .iter()
                    .position(|device| device.address == selected_address)
                    .unwrap_or(0);
                let active = self
                    .devices
                    .get(self.selected)
                    .and_then(|device| device.active_profile.as_deref())
                    .unwrap_or("-");
                if active == option.profile {
                    let suffix = if used_reconnect {
                        " after reconnect"
                    } else {
                        ""
                    };
                    self.status = format!("Switched {selected_name} to {}{suffix}", option.label);
                    true
                } else {
                    self.status =
                        format!("Requested {}, but active profile is {active}", option.label);
                    false
                }
            }
            Err(err) => {
                self.status = format!("Switched {}, but refresh failed: {err}", option.label);
                false
            }
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut terminal = setup_terminal()?;
    let result = run(&mut terminal);
    restore_terminal(&mut terminal)?;
    result
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>, Box<dyn Error>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    Terminal::new(CrosstermBackend::new(stdout)).map_err(Into::into)
}

fn restore_terminal(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> Result<(), Box<dyn Error>> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<(), Box<dyn Error>> {
    let mut app = App::new();
    app.refresh();

    loop {
        terminal.draw(|frame| draw(frame, &mut app))?;

        if event::poll(Duration::from_millis(200))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break,
                        KeyCode::Char('r') => app.refresh(),
                        KeyCode::Down | KeyCode::Char('j') => app.select_next(),
                        KeyCode::Up | KeyCode::Char('k') => app.select_previous(),
                        KeyCode::Right | KeyCode::Char('l') => app.switch_selected_codec(1),
                        KeyCode::Left | KeyCode::Char('h') => app.switch_selected_codec(-1),
                        KeyCode::Char(ch) if ch.is_ascii_digit() => {
                            if let Some(index) =
                                ch.to_digit(10).and_then(|digit| digit.checked_sub(1))
                            {
                                app.switch_selected_codec_to(index as usize);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        if app.last_refresh.elapsed() >= REFRESH_INTERVAL {
            app.refresh();
        }
    }

    Ok(())
}

fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(5),
        Constraint::Length(3),
    ])
    .areas(frame.area());

    let title = Paragraph::new(Line::from(vec![
        Span::styled(
            "bluz",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  connected bluetooth headphones"),
    ]))
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(title, header);

    let rows = app.devices.iter().map(|device| {
        let codec_choices = if device.codec_options.is_empty() {
            "-".to_string()
        } else {
            device
                .codec_options
                .iter()
                .map(|option| {
                    if device.active_profile.as_deref() == Some(option.profile.as_str()) {
                        format!("*{}", option.label)
                    } else {
                        option.label.clone()
                    }
                })
                .collect::<Vec<_>>()
                .join(", ")
        };

        Row::new([
            Cell::from(device.name.clone()),
            Cell::from(device.address.clone()),
            Cell::from(device.codec.clone()),
            Cell::from(codec_choices),
            Cell::from(device.path.clone()),
        ])
    });

    let table = Table::new(
        rows,
        [
            Constraint::Percentage(22),
            Constraint::Length(17),
            Constraint::Length(10),
            Constraint::Percentage(33),
            Constraint::Percentage(18),
        ],
    )
    .header(
        Row::new(["Name", "Address", "Codec", "Available codecs", "BlueZ path"]).style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
    )
    .row_highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan))
    .block(Block::default().borders(Borders::ALL));

    if app.devices.is_empty() {
        let empty = Paragraph::new("No connected bluetooth headphones found.")
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL));
        frame.render_widget(empty, body);
    } else {
        let mut state = TableState::default().with_selected(Some(app.selected));
        frame.render_stateful_widget(table, body, &mut state);
    }

    let footer_text = Paragraph::new(app.status.as_str())
        .style(Style::default().fg(Color::Gray))
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(footer_text, footer);
}

fn selected_codec_index(device: &Headphone) -> Option<usize> {
    if device.codec_options.is_empty() {
        return None;
    }

    Some(
        device
            .active_profile
            .as_ref()
            .and_then(|active| {
                device
                    .codec_options
                    .iter()
                    .position(|option| &option.profile == active)
            })
            .unwrap_or(0),
    )
}

fn connected_headphones() -> Result<Vec<Headphone>, Box<dyn Error>> {
    let connection = Connection::system()?;
    let manager = ObjectManagerProxy::builder(&connection)
        .destination(BLUEZ_SERVICE)?
        .path(BLUEZ_ROOT)?
        .build()?;
    let audio_cards = audio_cards_by_address();

    let mut devices = manager
        .get_managed_objects()?
        .into_iter()
        .filter_map(|(path, interfaces)| {
            let properties = interfaces.get(DEVICE_INTERFACE)?;
            let connected = prop_bool(properties.get("Connected")).unwrap_or(false);
            if !connected {
                return None;
            }

            let uuids = prop_strings(properties.get("UUIDs")).unwrap_or_default();
            let icon = prop_string(properties.get("Icon")).unwrap_or_default();
            if audio_profiles(&uuids).is_empty() && !looks_like_headphones(&icon) {
                return None;
            }

            let alias = prop_string(properties.get("Alias"));
            let name = alias
                .or_else(|| prop_string(properties.get("Name")))
                .unwrap_or_else(|| "Unknown device".to_string());
            let address = prop_string(properties.get("Address")).unwrap_or_else(|| "-".to_string());
            let audio_card = audio_cards
                .get(&normalized_address(&address))
                .cloned()
                .unwrap_or_default();

            Some(Headphone {
                name,
                address,
                codec: audio_card.codec.unwrap_or_else(|| "-".to_string()),
                card_name: (!audio_card.card_name.is_empty()).then_some(audio_card.card_name),
                active_profile: audio_card.active_profile,
                codec_options: audio_card.codec_options,
                path: path.to_string(),
            })
        })
        .collect::<Vec<_>>();

    devices.sort_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then_with(|| left.address.cmp(&right.address))
    });

    Ok(devices)
}

fn audio_cards_by_address() -> HashMap<String, AudioCard> {
    let mut cards = HashMap::new();
    read_pactl_cards(&mut cards);
    read_pactl_sinks(&mut cards);
    cards
}

fn read_pactl_sinks(cards: &mut HashMap<String, AudioCard>) {
    let Ok(output) = Command::new("pactl")
        .args(["--format=json", "list", "sinks"])
        .output()
    else {
        return;
    };

    if !output.status.success() {
        return;
    }

    let Ok(sinks) = serde_json::from_slice::<Vec<Value>>(&output.stdout) else {
        return;
    };

    for sink in sinks {
        let Some(properties) = sink.get("properties").and_then(Value::as_object) else {
            continue;
        };
        let Some(address) = string_property(properties, "api.bluez5.address")
            .or_else(|| string_property(properties, "device.string"))
        else {
            continue;
        };
        let Some(codec) = string_property(properties, "api.bluez5.codec")
            .or_else(|| string_property(properties, "bluetooth.codec"))
        else {
            continue;
        };

        cards.entry(normalized_address(&address)).or_default().codec = Some(display_codec(&codec));
    }
}

fn read_pactl_cards(cards_by_address: &mut HashMap<String, AudioCard>) {
    let Ok(output) = Command::new("pactl")
        .args(["--format=json", "list", "cards"])
        .output()
    else {
        return;
    };

    if !output.status.success() {
        return;
    }

    let Ok(cards) = serde_json::from_slice::<Vec<Value>>(&output.stdout) else {
        return;
    };

    for card in cards {
        let Some(card_name) = card.get("name").and_then(Value::as_str) else {
            continue;
        };
        let Some(properties) = card.get("properties").and_then(Value::as_object) else {
            continue;
        };
        let Some(address) = string_property(properties, "api.bluez5.address")
            .or_else(|| string_property(properties, "device.string"))
        else {
            continue;
        };
        let address = normalized_address(&address);
        let active_profile = card
            .get("active_profile")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let codec_options = card
            .get("profiles")
            .and_then(Value::as_object)
            .map(codec_options_from_profiles)
            .unwrap_or_default();
        let fallback_codec = active_profile.as_deref().and_then(|profile| {
            codec_options
                .iter()
                .find(|option| option.profile == profile)
                .map(|option| option.label.clone())
                .or_else(|| codec_from_profile(profile))
        });

        cards_by_address.insert(
            address,
            AudioCard {
                card_name: card_name.to_string(),
                active_profile,
                codec: fallback_codec,
                codec_options,
            },
        );
    }
}

fn codec_options_from_profiles(profiles: &serde_json::Map<String, Value>) -> Vec<CodecOption> {
    let mut options = profiles
        .iter()
        .filter_map(|(profile, data)| {
            let available = data
                .get("available")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let sinks = data.get("sinks").and_then(Value::as_u64).unwrap_or(0);
            if !available || sinks == 0 || profile == "off" {
                return None;
            }

            let label = data
                .get("description")
                .and_then(Value::as_str)
                .and_then(codec_from_description)
                .or_else(|| codec_from_profile(profile))?;
            let priority = data.get("priority").and_then(Value::as_i64).unwrap_or(0);

            Some((
                priority,
                CodecOption {
                    label,
                    profile: profile.clone(),
                },
            ))
        })
        .collect::<Vec<_>>();

    options.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.label.cmp(&right.1.label))
    });
    options.into_iter().map(|(_, option)| option).collect()
}

fn string_property(properties: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    properties
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn codec_from_profile(profile: &str) -> Option<String> {
    let codec = profile
        .rsplit_once('-')
        .map(|(_, codec)| codec)
        .filter(|codec| !matches!(*codec, "sink" | "unit"))?;
    Some(display_codec(codec))
}

fn codec_from_description(description: &str) -> Option<String> {
    let marker = "codec ";
    let start = description.find(marker)? + marker.len();
    let codec = description[start..]
        .trim_end_matches(')')
        .trim()
        .trim_end_matches('.');
    (!codec.is_empty()).then(|| display_codec(codec))
}

fn set_card_profile(card_name: &str, profile: &str) -> Result<(), Box<dyn Error>> {
    let output = Command::new("pactl")
        .args(["set-card-profile", card_name, profile])
        .output()?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if stderr.is_empty() {
            "pactl set-card-profile failed".into()
        } else {
            stderr.into()
        })
    }
}

fn reconnect_bluetooth_device(address: &str) -> Result<(), Box<dyn Error>> {
    let disconnect = Command::new("bluetoothctl")
        .args(["disconnect", address])
        .output()?;
    if !disconnect.status.success() {
        let stderr = String::from_utf8_lossy(&disconnect.stderr);
        let stdout = String::from_utf8_lossy(&disconnect.stdout);
        let message = format!("{stdout}{stderr}");
        if !message.contains("not connected") && !message.contains("No such") {
            return Err(message.trim().to_string().into());
        }
    }

    thread::sleep(Duration::from_secs(4));

    let connect = Command::new("bluetoothctl")
        .args(["connect", address])
        .output()?;
    if connect.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&connect.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&connect.stdout).trim().to_string();
        Err(if stderr.is_empty() {
            stdout.into()
        } else {
            stderr.into()
        })
    }
}

fn display_codec(codec: &str) -> String {
    match codec {
        "sbc" => "SBC".to_string(),
        "sbc_xq" => "SBC-XQ".to_string(),
        "aac" => "AAC".to_string(),
        "ldac" => "LDAC".to_string(),
        "aptx" => "aptX".to_string(),
        "aptx_hd" => "aptX HD".to_string(),
        "aptx_ll" => "aptX LL".to_string(),
        "aptx_ll_duplex" => "aptX LL Duplex".to_string(),
        "faststream" => "FastStream".to_string(),
        "faststream_duplex" => "FastStream Duplex".to_string(),
        "cvsd" => "CVSD".to_string(),
        "msbc" => "mSBC".to_string(),
        other => other.replace('_', "-").to_uppercase(),
    }
}

fn normalized_address(address: &str) -> String {
    address.replace('_', ":").to_uppercase()
}

fn prop_bool(value: Option<&OwnedValue>) -> Option<bool> {
    value.and_then(|value| bool::try_from(value).ok())
}

fn prop_string(value: Option<&OwnedValue>) -> Option<String> {
    value.and_then(|value| <&str>::try_from(value).ok().map(str::to_owned))
}

fn prop_strings(value: Option<&OwnedValue>) -> Option<Vec<String>> {
    value
        .and_then(|value| value.try_clone().ok())
        .and_then(|value| Vec::<String>::try_from(value).ok())
}

fn audio_profiles(uuids: &[String]) -> Vec<String> {
    AUDIO_UUIDS
        .iter()
        .filter_map(|(uuid, label)| {
            uuids
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(uuid))
                .then(|| (*label).to_string())
        })
        .collect()
}

fn looks_like_headphones(icon: &str) -> bool {
    matches!(
        icon,
        "audio-headphones" | "audio-headset" | "audio-card" | "audio-speakers"
    )
}
