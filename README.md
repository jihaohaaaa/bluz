# bluz

A terminal UI for showing currently connected Bluetooth headphones on Linux.

`bluz` queries BlueZ over the system D-Bus and lists connected devices that expose
Bluetooth audio profiles or headphone/headset-style icons. When PulseAudio or
PipeWire exposes the active BlueZ sink, `bluz` also shows and switches the
current audio codec, such as SBC, AAC, LDAC, or aptX.

## Usage

```sh
cargo run
```

Press `Up`/`Down` to select a device, `Left`/`Right` to cycle codec profiles,
number keys to pick a codec by its displayed order, `r` to refresh, and `q` or
`Esc` to quit. The `h`/`j`/`k`/`l` keys work as left/down/up/right shortcuts.

Some PipeWire/WirePlumber combinations keep LDAC unavailable after switching
away from it. When an LDAC switch request is ignored, `bluz` reconnects the
Bluetooth device once and refreshes the actual active codec.

## Requirements

- Linux with BlueZ running.
- Permission to read BlueZ device information from the system D-Bus.
- `pactl` from PulseAudio or PipeWire Pulse for codec detection and switching.
- `bluetoothctl` for LDAC recovery when the audio server requires a reconnect.
