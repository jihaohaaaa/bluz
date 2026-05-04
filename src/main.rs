use std::{
    collections::HashMap,
    error::Error,
    io,
    process::Command,
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
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
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
    profiles: Vec<String>,
    path: String,
}

#[derive(Debug)]
struct App {
    devices: Vec<Headphone>,
    status: String,
    last_refresh: Instant,
}

impl App {
    fn new() -> Self {
        Self {
            devices: Vec::new(),
            status: "Press r to refresh, q to quit".to_string(),
            last_refresh: Instant::now() - REFRESH_INTERVAL,
        }
    }

    fn refresh(&mut self) {
        match connected_headphones() {
            Ok(devices) => {
                let count = devices.len();
                self.devices = devices;
                self.status =
                    format!("Connected bluetooth headphones: {count} | r refresh | q/Esc quit");
            }
            Err(err) => {
                self.devices.clear();
                self.status = format!("BlueZ query failed: {err}");
            }
        }

        self.last_refresh = Instant::now();
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
        terminal.draw(|frame| draw(frame, &app))?;

        if event::poll(Duration::from_millis(200))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break,
                        KeyCode::Char('r') => app.refresh(),
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

fn draw(frame: &mut Frame<'_>, app: &App) {
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
        Row::new([
            Cell::from(device.name.clone()),
            Cell::from(device.address.clone()),
            Cell::from(device.codec.clone()),
            Cell::from(device.profiles.join(", ")),
            Cell::from(device.path.clone()),
        ])
    });

    let table = Table::new(
        rows,
        [
            Constraint::Percentage(22),
            Constraint::Length(17),
            Constraint::Length(10),
            Constraint::Percentage(28),
            Constraint::Percentage(23),
        ],
    )
    .header(
        Row::new(["Name", "Address", "Codec", "Profiles", "BlueZ path"]).style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
    )
    .block(Block::default().borders(Borders::ALL));

    if app.devices.is_empty() {
        let empty = Paragraph::new("No connected bluetooth headphones found.")
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL));
        frame.render_widget(empty, body);
    } else {
        frame.render_widget(table, body);
    }

    let footer_text = Paragraph::new(app.status.as_str())
        .style(Style::default().fg(Color::Gray))
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(footer_text, footer);
}

fn connected_headphones() -> Result<Vec<Headphone>, Box<dyn Error>> {
    let connection = Connection::system()?;
    let manager = ObjectManagerProxy::builder(&connection)
        .destination(BLUEZ_SERVICE)?
        .path(BLUEZ_ROOT)?
        .build()?;
    let codecs = current_codecs_by_address();

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
            let profiles = audio_profiles(&uuids);
            if profiles.is_empty() && !looks_like_headphones(&icon) {
                return None;
            }

            let alias = prop_string(properties.get("Alias"));
            let name = alias
                .or_else(|| prop_string(properties.get("Name")))
                .unwrap_or_else(|| "Unknown device".to_string());

            Some(Headphone {
                name,
                address: prop_string(properties.get("Address")).unwrap_or_else(|| "-".to_string()),
                codec: codecs
                    .get(&normalized_address(
                        &prop_string(properties.get("Address")).unwrap_or_default(),
                    ))
                    .cloned()
                    .unwrap_or_else(|| "-".to_string()),
                profiles,
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

fn current_codecs_by_address() -> HashMap<String, String> {
    let mut codecs = HashMap::new();
    read_pactl_sinks(&mut codecs);
    read_pactl_cards(&mut codecs);
    codecs
}

fn read_pactl_sinks(codecs: &mut HashMap<String, String>) {
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

        codecs.insert(normalized_address(&address), display_codec(&codec));
    }
}

fn read_pactl_cards(codecs: &mut HashMap<String, String>) {
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
        let Some(properties) = card.get("properties").and_then(Value::as_object) else {
            continue;
        };
        let Some(address) = string_property(properties, "api.bluez5.address")
            .or_else(|| string_property(properties, "device.string"))
        else {
            continue;
        };
        let address = normalized_address(&address);
        if codecs.contains_key(&address) {
            continue;
        }

        let Some(profile) = card.get("active_profile").and_then(Value::as_str) else {
            continue;
        };
        if let Some(codec) = codec_from_profile(profile) {
            codecs.insert(address, codec);
        }
    }
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
