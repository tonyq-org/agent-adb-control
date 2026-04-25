use std::{
    env,
    fs::{self, File},
    io::{self, Write},
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand, ValueEnum};
use image::{GenericImageView, ImageFormat, codecs::jpeg::JpegEncoder, imageops::FilterType};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Parser, Debug)]
#[command(
    name = "adb-agent",
    version,
    about = "Agent-friendly ADB control CLI with device sessions"
)]
struct Cli {
    /// Path or executable name for adb.
    #[arg(long, global = true, default_value = "adb")]
    adb: String,

    /// Override the current session device for this command.
    #[arg(short = 's', long, global = true)]
    device: Option<String>,

    /// Output format. JSON is stable for agents.
    #[arg(long, global = true, value_enum, default_value_t = OutputFormat::Text)]
    output: OutputFormat,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Clone, Debug, ValueEnum)]
enum OutputFormat {
    Text,
    Json,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Show connected USB/TCP devices.
    Devices,
    /// Connect to a TCP/IP Android device or emulator, e.g. 192.168.1.25:5555.
    Connect {
        target: String,
        /// Save the connected target as the current session device.
        #[arg(long)]
        select: bool,
    },
    /// Disconnect one TCP/IP target, or all TCP/IP targets when omitted.
    Disconnect { target: Option<String> },
    /// Start adb server.
    StartServer,
    /// Kill adb server.
    KillServer,
    /// Manage the current device session.
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
    /// List files on device.
    Ls {
        #[arg(default_value = "/sdcard")]
        path: String,
    },
    /// Search files on device with Android find.
    Find {
        pattern: String,
        #[arg(long, default_value = "/sdcard")]
        root: String,
        #[arg(long, value_enum, default_value_t = FindKind::Any)]
        kind: FindKind,
    },
    /// Pull a file or directory from device.
    Pull {
        remote: String,
        local: Option<PathBuf>,
    },
    /// Push a file or directory to device.
    Push { local: PathBuf, remote: String },
    /// Capture a compact screenshot preview.
    Screenshot {
        /// Output image path. Defaults to screenshot-<unix>.jpg in the current directory.
        #[arg(value_name = "OUTPUT")]
        path: Option<PathBuf>,
        /// Maximum preview width. Ignored with --full.
        #[arg(long, default_value_t = 1080, value_parser = clap::value_parser!(u32).range(1..))]
        max_width: u32,
        /// Maximum preview height. Ignored with --full.
        #[arg(long, default_value_t = 1080, value_parser = clap::value_parser!(u32).range(1..))]
        max_height: u32,
        /// JPEG quality when writing JPEG.
        #[arg(long, default_value_t = 82, value_parser = clap::value_parser!(u8).range(1..=100))]
        quality: u8,
        /// Output format. Inferred from extension when omitted.
        #[arg(long, value_enum)]
        format: Option<ScreenshotFormat>,
        /// Keep full device resolution instead of generating a smaller preview.
        #[arg(long)]
        full: bool,
    },
    /// Tap screen coordinates.
    Tap { x: i32, y: i32 },
    /// Swipe between screen coordinates.
    Swipe {
        x1: i32,
        y1: i32,
        x2: i32,
        y2: i32,
        /// Duration in milliseconds.
        #[arg(long)]
        duration_ms: Option<u32>,
    },
    /// Send an Android keyevent, e.g. HOME, BACK, ENTER, 26.
    Keyevent { key: String },
    /// Type text with adb shell input text.
    Text { text: String },
    /// Run an adb shell command on the session device.
    Shell {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        command: Vec<String>,
    },
    /// Install an APK.
    Install {
        apk: PathBuf,
        /// Replace existing application.
        #[arg(short = 'r', long)]
        replace: bool,
        /// Allow version code downgrade.
        #[arg(short = 'd', long)]
        downgrade: bool,
    },
    /// Uninstall a package.
    Uninstall {
        package: String,
        /// Keep data and cache directories.
        #[arg(short = 'k', long)]
        keep_data: bool,
    },
    /// Run logcat. Use --dump for a bounded command agents can parse.
    Logcat {
        /// Dump current logs and exit.
        #[arg(short = 'd', long)]
        dump: bool,
        /// Clear logs.
        #[arg(short = 'c', long)]
        clear: bool,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Open, close, or restart Android apps by package name.
    App {
        #[command(subcommand)]
        command: AppCommand,
    },
    /// Pass raw arguments to adb. Uses the session device unless --no-device is set.
    Raw {
        #[arg(long)]
        no_device: bool,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        args: Vec<String>,
    },
}

#[derive(Subcommand, Debug)]
enum SessionCommand {
    /// Set current session device.
    Use { device: String },
    /// Show current session device and state file path.
    Show,
    /// Clear current session.
    Clear,
    /// Print session state file path.
    Path,
}

#[derive(Subcommand, Debug)]
enum AppCommand {
    /// Start an app by package name. Uses launcher intent when activity is omitted.
    Start {
        package: String,
        /// Activity component. Accepts .MainActivity, MainActivity, or package/.MainActivity.
        #[arg(long)]
        activity: Option<String>,
        /// Wait for launch completion when using --activity.
        #[arg(long)]
        wait: bool,
    },
    /// Force-stop an app by package name.
    Stop { package: String },
    /// Force-stop then start an app.
    Restart {
        package: String,
        /// Activity component. Accepts .MainActivity, MainActivity, or package/.MainActivity.
        #[arg(long)]
        activity: Option<String>,
        /// Wait for launch completion when using --activity.
        #[arg(long)]
        wait: bool,
    },
}

#[derive(Clone, Debug, ValueEnum)]
enum FindKind {
    Any,
    File,
    Dir,
}

#[derive(Clone, Copy, Debug, Serialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
enum ScreenshotFormat {
    Jpeg,
    Png,
}

#[derive(Debug, Serialize, Deserialize)]
struct SessionState {
    device: String,
    updated_at_unix: u64,
}

#[derive(Debug, Serialize)]
struct ScreenshotResult {
    path: PathBuf,
    format: ScreenshotFormat,
    original_width: u32,
    original_height: u32,
    output_width: u32,
    output_height: u32,
    scale_x: f64,
    scale_y: f64,
    file_bytes: u64,
}

#[derive(Debug, Serialize)]
struct DeviceInfo {
    serial: String,
    state: String,
    details: Vec<String>,
}

#[derive(Debug)]
struct AdbOutput {
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
}

#[derive(Debug, Serialize)]
struct Response {
    ok: bool,
    action: String,
    device: Option<String>,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    data: Value,
    error: Option<String>,
}

struct App {
    adb: String,
    cli_device: Option<String>,
    format: OutputFormat,
}

fn main() {
    let cli = Cli::parse();
    let app = App {
        adb: cli.adb,
        cli_device: cli.device,
        format: cli.output,
    };

    let response = match dispatch(&app, cli.command) {
        Ok(response) => response,
        Err(error) => Response {
            ok: false,
            action: "error".to_string(),
            device: app
                .cli_device
                .clone()
                .or_else(|| read_session().ok().map(|s| s.device)),
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            data: json!({}),
            error: Some(error.to_string()),
        },
    };

    let exit_code = if response.ok { 0 } else { 1 };
    if let Err(error) = emit(&app.format, &response) {
        eprintln!("failed to write output: {error:#}");
        std::process::exit(1);
    }
    std::process::exit(exit_code);
}

fn dispatch(app: &App, command: Commands) -> Result<Response> {
    match command {
        Commands::Devices => {
            let output = app.adb(false, Vec::<String>::new(), ["devices", "-l"])?;
            let devices = parse_devices(&output.stdout);
            Ok(response_from_output(
                "devices",
                None,
                output,
                json!({ "devices": devices }),
            ))
        }
        Commands::Connect { target, select } => {
            let output = app.adb(false, Vec::<String>::new(), ["connect", target.as_str()])?;
            if select && output.exit_code == Some(0) {
                write_session(&target)?;
            }
            Ok(response_from_output(
                "connect",
                None,
                output,
                json!({ "target": target, "selected": select }),
            ))
        }
        Commands::Disconnect { target } => {
            let mut args = vec!["disconnect".to_string()];
            if let Some(target) = target {
                args.push(target);
            }
            let output = app.adb(false, Vec::<String>::new(), args)?;
            Ok(response_from_output("disconnect", None, output, json!({})))
        }
        Commands::StartServer => {
            let output = app.adb(false, Vec::<String>::new(), ["start-server"])?;
            Ok(response_from_output(
                "start-server",
                None,
                output,
                json!({}),
            ))
        }
        Commands::KillServer => {
            let output = app.adb(false, Vec::<String>::new(), ["kill-server"])?;
            Ok(response_from_output("kill-server", None, output, json!({})))
        }
        Commands::Session { command } => handle_session(command),
        Commands::Ls { path } => {
            let device = app.device()?;
            let output = app.adb(
                true,
                [device.as_str()],
                ["shell", "ls", "-la", path.as_str()],
            )?;
            Ok(response_from_output(
                "ls",
                Some(device),
                output,
                json!({ "path": path }),
            ))
        }
        Commands::Find {
            pattern,
            root,
            kind,
        } => {
            let device = app.device()?;
            let mut args = vec![
                "shell".to_string(),
                "find".to_string(),
                root.clone(),
                "-iname".to_string(),
                pattern.clone(),
            ];
            match kind {
                FindKind::Any => {}
                FindKind::File => args.extend(["-type".to_string(), "f".to_string()]),
                FindKind::Dir => args.extend(["-type".to_string(), "d".to_string()]),
            }
            let output = app.adb(true, [device.as_str()], args)?;
            let matches: Vec<String> = output
                .stdout
                .lines()
                .filter(|line| !line.is_empty())
                .map(ToString::to_string)
                .collect();
            Ok(response_from_output(
                "find",
                Some(device),
                output,
                json!({ "root": root, "pattern": pattern, "matches": matches }),
            ))
        }
        Commands::Pull { remote, local } => {
            let device = app.device()?;
            let mut args = vec!["pull".to_string(), remote.clone()];
            if let Some(local) = &local {
                args.push(local.display().to_string());
            }
            let output = app.adb(true, [device.as_str()], args)?;
            Ok(response_from_output(
                "pull",
                Some(device),
                output,
                json!({ "remote": remote, "local": local }),
            ))
        }
        Commands::Push { local, remote } => {
            let device = app.device()?;
            let output = app.adb(
                true,
                [device.as_str()],
                vec![
                    "push".to_string(),
                    local.display().to_string(),
                    remote.clone(),
                ],
            )?;
            Ok(response_from_output(
                "push",
                Some(device),
                output,
                json!({ "local": local, "remote": remote }),
            ))
        }
        Commands::Screenshot {
            path,
            max_width,
            max_height,
            quality,
            format,
            full,
        } => {
            let device = app.device()?;
            let screenshot_format = resolve_screenshot_format(path.as_ref(), format);
            let path = path.unwrap_or_else(|| default_screenshot_path(screenshot_format));
            let result = screenshot(
                &app.adb,
                &device,
                &path,
                screenshot_format,
                max_width,
                max_height,
                quality,
                full,
            )?;
            Ok(Response {
                ok: true,
                action: "screenshot".to_string(),
                device: Some(device),
                exit_code: Some(0),
                stdout: format!("saved {}\n", path.display()),
                stderr: String::new(),
                data: json!({
                    "screenshot": result,
                    "coordinate_mapping": {
                        "device_x": "round(preview_x * scale_x)",
                        "device_y": "round(preview_y * scale_y)"
                    }
                }),
                error: None,
            })
        }
        Commands::Tap { x, y } => {
            let device = app.device()?;
            let output = app.adb(
                true,
                [device.as_str()],
                vec![
                    "shell".to_string(),
                    "input".to_string(),
                    "tap".to_string(),
                    x.to_string(),
                    y.to_string(),
                ],
            )?;
            Ok(response_from_output(
                "tap",
                Some(device),
                output,
                json!({ "x": x, "y": y }),
            ))
        }
        Commands::Swipe {
            x1,
            y1,
            x2,
            y2,
            duration_ms,
        } => {
            let device = app.device()?;
            let mut args = vec![
                "shell".to_string(),
                "input".to_string(),
                "swipe".to_string(),
                x1.to_string(),
                y1.to_string(),
                x2.to_string(),
                y2.to_string(),
            ];
            if let Some(duration_ms) = duration_ms {
                args.push(duration_ms.to_string());
            }
            let output = app.adb(true, [device.as_str()], args)?;
            Ok(response_from_output(
                "swipe",
                Some(device),
                output,
                json!({ "x1": x1, "y1": y1, "x2": x2, "y2": y2, "duration_ms": duration_ms }),
            ))
        }
        Commands::Keyevent { key } => {
            let device = app.device()?;
            let output = app.adb(
                true,
                [device.as_str()],
                ["shell", "input", "keyevent", key.as_str()],
            )?;
            Ok(response_from_output(
                "keyevent",
                Some(device),
                output,
                json!({ "key": key }),
            ))
        }
        Commands::Text { text } => {
            let device = app.device()?;
            let adb_text = text.replace(' ', "%s");
            let output = app.adb(
                true,
                [device.as_str()],
                ["shell", "input", "text", adb_text.as_str()],
            )?;
            Ok(response_from_output(
                "text",
                Some(device),
                output,
                json!({ "text": text }),
            ))
        }
        Commands::Shell { command } => {
            let device = app.device()?;
            let mut args = vec!["shell".to_string()];
            args.extend(command);
            let output = app.adb(true, [device.as_str()], args)?;
            Ok(response_from_output(
                "shell",
                Some(device),
                output,
                json!({}),
            ))
        }
        Commands::Install {
            apk,
            replace,
            downgrade,
        } => {
            let device = app.device()?;
            let mut args = vec!["install".to_string()];
            if replace {
                args.push("-r".to_string());
            }
            if downgrade {
                args.push("-d".to_string());
            }
            args.push(apk.display().to_string());
            let output = app.adb(true, [device.as_str()], args)?;
            Ok(response_from_output(
                "install",
                Some(device),
                output,
                json!({ "apk": apk, "replace": replace, "downgrade": downgrade }),
            ))
        }
        Commands::Uninstall { package, keep_data } => {
            let device = app.device()?;
            let mut args = vec!["uninstall".to_string()];
            if keep_data {
                args.push("-k".to_string());
            }
            args.push(package.clone());
            let output = app.adb(true, [device.as_str()], args)?;
            Ok(response_from_output(
                "uninstall",
                Some(device),
                output,
                json!({ "package": package, "keep_data": keep_data }),
            ))
        }
        Commands::Logcat { dump, clear, args } => {
            let device = app.device()?;
            let mut adb_args = vec!["logcat".to_string()];
            if dump {
                adb_args.push("-d".to_string());
            }
            if clear {
                adb_args.push("-c".to_string());
            }
            adb_args.extend(args);
            let output = app.adb(true, [device.as_str()], adb_args)?;
            Ok(response_from_output(
                "logcat",
                Some(device),
                output,
                json!({ "dump": dump, "clear": clear }),
            ))
        }
        Commands::App { command } => handle_app(app, command),
        Commands::Raw { no_device, args } => {
            let device = if no_device { None } else { app.device().ok() };
            let output = if let Some(device) = &device {
                app.adb(true, [device.as_str()], args)?
            } else {
                app.adb(false, Vec::<String>::new(), args)?
            };
            Ok(response_from_output("raw", device, output, json!({})))
        }
    }
}

fn handle_app(app: &App, command: AppCommand) -> Result<Response> {
    let device = app.device()?;
    match command {
        AppCommand::Start {
            package,
            activity,
            wait,
        } => {
            let (args, mode, component) = build_app_start_args(&package, activity.as_deref(), wait);
            let output = app.adb(true, [device.as_str()], args)?;
            Ok(response_from_output(
                "app.start",
                Some(device),
                output,
                json!({
                    "package": package,
                    "activity": activity,
                    "component": component,
                    "mode": mode,
                    "wait": wait
                }),
            ))
        }
        AppCommand::Stop { package } => {
            let output = app.adb(
                true,
                [device.as_str()],
                ["shell", "am", "force-stop", package.as_str()],
            )?;
            Ok(response_from_output(
                "app.stop",
                Some(device),
                output,
                json!({ "package": package }),
            ))
        }
        AppCommand::Restart {
            package,
            activity,
            wait,
        } => {
            let stop_output = app.adb(
                true,
                [device.as_str()],
                ["shell", "am", "force-stop", package.as_str()],
            )?;
            if stop_output.exit_code != Some(0) {
                return Ok(response_from_output(
                    "app.restart",
                    Some(device),
                    stop_output,
                    json!({ "package": package, "phase": "stop" }),
                ));
            }

            let (args, mode, component) = build_app_start_args(&package, activity.as_deref(), wait);
            let start_output = app.adb(true, [device.as_str()], args)?;
            let ok = start_output.exit_code == Some(0);
            let stderr = format!("{}{}", stop_output.stderr, start_output.stderr);
            Ok(Response {
                ok,
                action: "app.restart".to_string(),
                device: Some(device),
                exit_code: start_output.exit_code,
                stdout: format!("{}{}", stop_output.stdout, start_output.stdout),
                error: if ok {
                    None
                } else {
                    Some(if stderr.trim().is_empty() {
                        "app restart failed".to_string()
                    } else {
                        stderr.trim().to_string()
                    })
                },
                stderr,
                data: json!({
                    "package": package,
                    "activity": activity,
                    "component": component,
                    "mode": mode,
                    "wait": wait
                }),
            })
        }
    }
}

fn build_app_start_args(
    package: &str,
    activity: Option<&str>,
    wait: bool,
) -> (Vec<String>, &'static str, Option<String>) {
    if let Some(activity) = activity {
        let component = normalize_activity_component(package, activity);
        let mut args = vec!["shell".to_string(), "am".to_string(), "start".to_string()];
        if wait {
            args.push("-W".to_string());
        }
        args.extend(["-n".to_string(), component.clone()]);
        (args, "activity", Some(component))
    } else {
        (
            vec![
                "shell".to_string(),
                "monkey".to_string(),
                "-p".to_string(),
                package.to_string(),
                "-c".to_string(),
                "android.intent.category.LAUNCHER".to_string(),
                "1".to_string(),
            ],
            "launcher",
            None,
        )
    }
}

fn normalize_activity_component(package: &str, activity: &str) -> String {
    if activity.contains('/') {
        activity.to_string()
    } else {
        format!("{package}/{activity}")
    }
}

impl App {
    fn device(&self) -> Result<String> {
        self.cli_device
            .clone()
            .or_else(|| read_session().ok().map(|session| session.device))
            .ok_or_else(|| {
                anyhow!(
                    "no device selected; pass --device <serial> or run `agent-adb-control session use <serial>`"
                )
            })
    }

    fn adb<D, A, S>(&self, use_device: bool, device: D, args: A) -> Result<AdbOutput>
    where
        D: IntoIterator,
        D::Item: AsRef<str>,
        A: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut command = Command::new(&self.adb);
        if use_device {
            let device_args: Vec<String> = device
                .into_iter()
                .map(|part| part.as_ref().to_string())
                .collect();
            if device_args.is_empty() {
                bail!("internal error: adb command requested device mode without a device");
            }
            command.arg("-s").arg(&device_args[0]);
        }
        for arg in args {
            command.arg(arg.as_ref());
        }
        let output = command
            .output()
            .with_context(|| format!("failed to execute `{}`", self.adb))?;
        Ok(AdbOutput {
            exit_code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }
}

fn handle_session(command: SessionCommand) -> Result<Response> {
    match command {
        SessionCommand::Use { device } => {
            write_session(&device)?;
            Ok(Response {
                ok: true,
                action: "session.use".to_string(),
                device: Some(device.clone()),
                exit_code: Some(0),
                stdout: format!("current device: {device}\n"),
                stderr: String::new(),
                data: json!({ "device": device, "path": session_path()? }),
                error: None,
            })
        }
        SessionCommand::Show => {
            let path = session_path()?;
            match read_session() {
                Ok(session) => Ok(Response {
                    ok: true,
                    action: "session.show".to_string(),
                    device: Some(session.device.clone()),
                    exit_code: Some(0),
                    stdout: format!("current device: {}\n", session.device),
                    stderr: String::new(),
                    data: json!({ "session": session, "path": path }),
                    error: None,
                }),
                Err(_) => Ok(Response {
                    ok: true,
                    action: "session.show".to_string(),
                    device: None,
                    exit_code: Some(0),
                    stdout: "no current device\n".to_string(),
                    stderr: String::new(),
                    data: json!({ "session": null, "path": path }),
                    error: None,
                }),
            }
        }
        SessionCommand::Clear => {
            let path = session_path()?;
            if path.exists() {
                fs::remove_file(&path)
                    .with_context(|| format!("failed to remove {}", path.display()))?;
            }
            Ok(Response {
                ok: true,
                action: "session.clear".to_string(),
                device: None,
                exit_code: Some(0),
                stdout: "session cleared\n".to_string(),
                stderr: String::new(),
                data: json!({ "path": path }),
                error: None,
            })
        }
        SessionCommand::Path => {
            let path = session_path()?;
            Ok(Response {
                ok: true,
                action: "session.path".to_string(),
                device: None,
                exit_code: Some(0),
                stdout: format!("{}\n", path.display()),
                stderr: String::new(),
                data: json!({ "path": path }),
                error: None,
            })
        }
    }
}

fn response_from_output(
    action: impl Into<String>,
    device: Option<String>,
    output: AdbOutput,
    data: Value,
) -> Response {
    let ok = output.exit_code == Some(0);
    let stderr = output.stderr;
    Response {
        ok,
        action: action.into(),
        device,
        exit_code: output.exit_code,
        stdout: output.stdout,
        error: if ok {
            None
        } else {
            Some(if stderr.trim().is_empty() {
                "adb command failed".to_string()
            } else {
                stderr.trim().to_string()
            })
        },
        stderr,
        data,
    }
}

fn emit(format: &OutputFormat, response: &Response) -> Result<()> {
    match format {
        OutputFormat::Json => {
            let stdout = io::stdout();
            let mut lock = stdout.lock();
            serde_json::to_writer_pretty(&mut lock, response)?;
            writeln!(lock)?;
        }
        OutputFormat::Text => {
            let mut stdout = io::stdout().lock();
            let mut stderr = io::stderr().lock();
            if response.ok {
                if !response.stdout.is_empty() {
                    write!(stdout, "{}", response.stdout)?;
                } else if !response.data.is_null() && response.data != json!({}) {
                    writeln!(stdout, "{}", serde_json::to_string_pretty(&response.data)?)?;
                }
            } else {
                if let Some(error) = &response.error {
                    writeln!(stderr, "{error}")?;
                }
                if !response.stderr.is_empty() {
                    write!(stderr, "{}", response.stderr)?;
                }
            }
        }
    }
    Ok(())
}

fn parse_devices(stdout: &str) -> Vec<DeviceInfo> {
    stdout
        .lines()
        .skip_while(|line| !line.starts_with("List of devices attached"))
        .skip(1)
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let serial = parts.next()?;
            let state = parts.next()?;
            let details = parts.map(ToString::to_string).collect();
            Some(DeviceInfo {
                serial: serial.to_string(),
                state: state.to_string(),
                details,
            })
        })
        .collect()
}

fn screenshot(
    adb: &str,
    device: &str,
    path: &PathBuf,
    format: ScreenshotFormat,
    max_width: u32,
    max_height: u32,
    quality: u8,
    full: bool,
) -> Result<ScreenshotResult> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let output = Command::new(adb)
        .arg("-s")
        .arg(device)
        .arg("exec-out")
        .arg("screencap")
        .arg("-p")
        .output()
        .with_context(|| format!("failed to execute `{adb}`"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("screenshot failed: {}", stderr.trim());
    }

    let image =
        image::load_from_memory(&output.stdout).context("failed to decode adb screenshot PNG")?;
    let (original_width, original_height) = image.dimensions();
    let (output_width, output_height) = if full {
        (original_width, original_height)
    } else {
        fit_within(original_width, original_height, max_width, max_height)
    };

    let processed = if output_width == original_width && output_height == original_height {
        image
    } else {
        image.resize_exact(output_width, output_height, FilterType::Lanczos3)
    };

    match format {
        ScreenshotFormat::Jpeg => {
            let mut file = File::create(path)
                .with_context(|| format!("failed to create {}", path.display()))?;
            let rgb = processed.to_rgb8();
            let mut encoder = JpegEncoder::new_with_quality(&mut file, quality);
            encoder
                .encode_image(&image::DynamicImage::ImageRgb8(rgb))
                .with_context(|| format!("failed to encode JPEG {}", path.display()))?;
        }
        ScreenshotFormat::Png => {
            processed
                .save_with_format(path, ImageFormat::Png)
                .with_context(|| format!("failed to write PNG {}", path.display()))?;
        }
    }

    let file_bytes = fs::metadata(path)
        .with_context(|| format!("failed to stat {}", path.display()))?
        .len();

    Ok(ScreenshotResult {
        path: path.clone(),
        format,
        original_width,
        original_height,
        output_width,
        output_height,
        scale_x: original_width as f64 / output_width as f64,
        scale_y: original_height as f64 / output_height as f64,
        file_bytes,
    })
}

fn fit_within(width: u32, height: u32, max_width: u32, max_height: u32) -> (u32, u32) {
    let scale = (max_width as f64 / width as f64)
        .min(max_height as f64 / height as f64)
        .min(1.0);
    let output_width = ((width as f64 * scale).round() as u32).max(1);
    let output_height = ((height as f64 * scale).round() as u32).max(1);
    (output_width, output_height)
}

fn resolve_screenshot_format(
    path: Option<&PathBuf>,
    requested: Option<ScreenshotFormat>,
) -> ScreenshotFormat {
    if let Some(format) = requested {
        return format;
    }

    path.and_then(|path| path.extension())
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
        .and_then(|extension| match extension.as_str() {
            "jpg" | "jpeg" => Some(ScreenshotFormat::Jpeg),
            "png" => Some(ScreenshotFormat::Png),
            _ => None,
        })
        .unwrap_or(ScreenshotFormat::Jpeg)
}

fn default_screenshot_path(format: ScreenshotFormat) -> PathBuf {
    let extension = match format {
        ScreenshotFormat::Jpeg => "jpg",
        ScreenshotFormat::Png => "png",
    };
    PathBuf::from(format!("screenshot-{}.{}", unix_now(), extension))
}

fn read_session() -> Result<SessionState> {
    let path = session_path()?;
    let content =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&content)
        .with_context(|| format!("invalid session file {}", path.display()))
}

fn write_session(device: &str) -> Result<()> {
    let path = session_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let state = SessionState {
        device: device.to_string(),
        updated_at_unix: unix_now(),
    };
    let content = serde_json::to_string_pretty(&state)?;
    fs::write(&path, content).with_context(|| format!("failed to write {}", path.display()))
}

fn session_path() -> Result<PathBuf> {
    let base = if let Ok(path) = env::var("AGENT_ADB_CONTROL_HOME") {
        PathBuf::from(path)
    } else if cfg!(windows) {
        env::var_os("APPDATA")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("APPDATA is not set"))?
            .join("agent-adb-control")
    } else {
        env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("HOME is not set"))?
            .join(".agent-adb-control")
    };
    Ok(base.join("session.json"))
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_adb_devices_l_output() {
        let output = "\
List of devices attached
emulator-5554 device product:sdk_gphone64 model:sdk_gphone64 transport_id:1
192.168.1.9:5555 unauthorized
";

        let devices = parse_devices(output);

        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].serial, "emulator-5554");
        assert_eq!(devices[0].state, "device");
        assert_eq!(devices[0].details[0], "product:sdk_gphone64");
        assert_eq!(devices[1].serial, "192.168.1.9:5555");
        assert_eq!(devices[1].state, "unauthorized");
    }

    #[test]
    fn default_screenshot_name_matches_format() {
        let jpeg_path = default_screenshot_path(ScreenshotFormat::Jpeg);
        let png_path = default_screenshot_path(ScreenshotFormat::Png);

        assert_eq!(
            jpeg_path.extension().and_then(|ext| ext.to_str()),
            Some("jpg")
        );
        assert_eq!(
            png_path.extension().and_then(|ext| ext.to_str()),
            Some("png")
        );
    }

    #[test]
    fn fit_within_preserves_aspect_ratio_without_upscaling() {
        assert_eq!(fit_within(1440, 3120, 1080, 1080), (498, 1080));
        assert_eq!(fit_within(800, 600, 1080, 1080), (800, 600));
    }

    #[test]
    fn normalizes_activity_component() {
        assert_eq!(
            normalize_activity_component("com.example.app", ".MainActivity"),
            "com.example.app/.MainActivity"
        );
        assert_eq!(
            normalize_activity_component("com.example.app", "com.example.app.MainActivity"),
            "com.example.app/com.example.app.MainActivity"
        );
        assert_eq!(
            normalize_activity_component("com.example.app", "other/.MainActivity"),
            "other/.MainActivity"
        );
    }

    #[test]
    fn builds_launcher_start_args_when_activity_is_omitted() {
        let (args, mode, component) = build_app_start_args("com.example.app", None, false);

        assert_eq!(mode, "launcher");
        assert_eq!(component, None);
        assert_eq!(
            args,
            vec![
                "shell",
                "monkey",
                "-p",
                "com.example.app",
                "-c",
                "android.intent.category.LAUNCHER",
                "1"
            ]
        );
    }
}
