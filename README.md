# bluz

A terminal UI for showing currently connected Bluetooth headphones on Linux.

`bluz` queries BlueZ over the system D-Bus and lists connected devices that expose
Bluetooth audio profiles or headphone/headset-style icons. When PulseAudio or
PipeWire exposes the active BlueZ sink, `bluz` also shows the current audio
codec, such as SBC, AAC, LDAC, or aptX.

## Usage

```sh
cargo run
```

Press `r` to refresh and `q` or `Esc` to quit.

## Requirements

- Linux with BlueZ running.
- Permission to read BlueZ device information from the system D-Bus.
