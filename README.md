# adb-agent

`adb-agent` is a small Rust CLI that wraps Android Debug Bridge commands in a
session-oriented, agent-friendly interface for macOS and Windows.

It keeps a current device session, so an agent can select a device once and then
run repeated commands without rediscovering or retyping the device serial.

## Requirements

- Rust toolchain
- Android Platform Tools with `adb` available in `PATH`

## Install

```bash
cargo install --path .
```

Or run without installing:

```bash
cargo run --bin adb-agent -- --help
```

## Device Session

List devices:

```bash
adb-agent devices
adb-agent devices --output json
```

Use a USB device or emulator for the current session:

```bash
adb-agent session use emulator-5554 --name default
```

Connect to a TCP/IP device and select it as the current session:

```bash
adb-agent connect 192.168.1.25:5555 --select
```

Show or clear the session:

```bash
adb-agent session show
adb-agent session list
adb-agent session clear
```

Override the session for one command:

```bash
adb-agent --device emulator-5554 shell getprop ro.product.model
```

Use named sessions for multiple devices or multiple agents:

```bash
adb-agent session use R5CX3058KHP --name phone-a
adb-agent session use 192.168.1.25:5555 --name phone-b --no-select
adb-agent --session phone-a screenshot phone-a.jpg
adb-agent --session phone-b shell wm size
adb-agent session select phone-a
adb-agent session remove phone-b
```

Each agent should pass its own `--session <name>` to avoid overwriting another
agent's selected current session.

Agents can also set `AGENT_ADB_CONTROL_SESSION=<name>` and omit `--session` from
each command.

Session state is stored at:

- Windows: `%APPDATA%\agent-adb-control\session.json`
- macOS/Linux: `~/.agent-adb-control/session.json`

Set `AGENT_ADB_CONTROL_HOME` to override the state directory.

## Device Recovery

Recover missing, offline, or unstable devices:

```bash
adb-agent recover
adb-agent --session phone-a recover
adb-agent --session phone-a --auto-recover shell wm size
adb-agent --device R5CX3058KHP recover --force
```

`recover` checks `adb devices -l`, restarts the adb server when the selected
device is missing or offline, reconnects TCP/IP devices when the selected serial
looks like `host:port`, then lists devices again.

If a device is `unauthorized`, the CLI does not treat that as a server restart
problem. It returns a prompt telling the user to unlock the phone and accept the
USB debugging RSA authorization dialog.

## Common Commands

Search files:

```bash
adb-agent find "*.jpg" --root /sdcard --kind file
adb-agent find "Download" --root /sdcard --kind dir --output json
```

Pull and push files:

```bash
adb-agent pull /sdcard/Download/report.pdf .
adb-agent push ./local.txt /sdcard/Download/local.txt
```

Screenshot preview:

```bash
adb-agent screenshot
adb-agent screenshot screen.jpg
adb-agent screenshot screen.png --format png
adb-agent screenshot full.png --full --format png
adb-agent screenshot compact.jpg --max-width 900 --max-height 900 --quality 78
```

By default screenshots are downscaled to fit within `1080x1080` and written as
JPEG quality `82`. JSON output includes the original size, preview size, file
bytes, and coordinate scale:

```bash
adb-agent --output json screenshot screen.jpg
```

Use the returned values like `device_x = round(preview_x * scale_x)` and
`device_y = round(preview_y * scale_y)` before calling `tap`.

Touch and input:

```bash
adb-agent tap 500 1200
adb-agent swipe 500 1400 500 300 --duration-ms 350
adb-agent keyevent HOME
adb-agent text "hello world"
```

Shell and raw adb:

```bash
adb-agent shell pm list packages
adb-agent logcat --dump -t 200
adb-agent raw shell wm size
adb-agent raw --no-device devices -l
```

Open and close apps:

```bash
adb-agent app start com.android.settings
adb-agent app start com.example.app --activity .MainActivity --wait
adb-agent app stop com.example.app
adb-agent app restart com.example.app
```

Install and uninstall:

```bash
adb-agent install -r app-debug.apk
adb-agent uninstall com.example.app
```

## Agent Output Contract

Every command supports `--output json`. The JSON shape is stable:

```json
{
  "ok": true,
  "action": "shell",
  "device": "emulator-5554",
  "exit_code": 0,
  "stdout": "...",
  "stderr": "",
  "data": {},
  "error": null
}
```

For agent workflows, prefer:

```bash
adb-agent --output json devices
adb-agent --output json session use emulator-5554
adb-agent --output json find "*.png" --root /sdcard --kind file
```

Use `shell` for normal device shell commands and `raw` for adb features that are
not yet modeled directly by this wrapper.
