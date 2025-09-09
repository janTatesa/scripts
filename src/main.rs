use std::{
    env, fs,
    io::{Read, Write, stdin},
    iter,
    path::PathBuf,
    process::{Command, Stdio},
};

use chrono::Local;
use clap::{ArgAction, Parser, Subcommand};
use color_eyre::{
    Result,
    eyre::{ContextCompat, OptionExt, eyre},
};
use notify_rust::Notification;
use regex::Regex;
use swayipc::NodeType;
use wl_clipboard_rs::copy::{MimeType, Options};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    script: Script,
}

#[derive(Subcommand)]
enum Script {
    Nixos {
        #[arg(long)]
        open_editor: bool,
        #[arg(long, env = "EDITOR")]
        editor_name: String,
        #[arg(long, action = ArgAction::SetTrue)]
        update: bool,
        #[arg(long, env = "NH_FLAKE")]
        flake: PathBuf,
        #[arg(long, env = "DEVICE")]
        device: String,
    },

    Scrollback {
        #[arg(long, env = "EDITOR")]
        editor_name: String,
    },
    Screenshot {
        #[command(subcommand)]
        area: ScreenshotArea,
    },
}

#[derive(Subcommand)]
enum ScreenshotArea {
    Fullscreen,
    Window,
    Region {
        #[arg(long)]
        slurp_fg: String,
        #[arg(long)]
        slurp_bg: String,
    },
}

fn main() -> Result<()> {
    color_eyre::install()?;

    match Cli::parse().script {
        Script::Nixos {
            open_editor,
            editor_name,
            update,
            flake,
            device,
        } => nixos(open_editor.then_some(editor_name), update, flake, device),
        Script::Scrollback { editor_name } => scrollback(editor_name),
        Script::Screenshot { area } => screenshot(area),
    }?;

    Ok(())
}

fn run_command<'a>(command: &'a str, args: impl IntoIterator<Item = &'a str>) -> Result<()> {
    run_command_with_stdio(command, args, Stdio::inherit(), None).map(|_| ())
}

fn run_command_with_stdio<'a>(
    command: &'a str,
    args: impl IntoIterator<Item = &'a str>,
    stdout: Stdio,
    stdin: Option<&[u8]>,
) -> Result<Vec<u8>> {
    let mut cmd = Command::new(command)
        .args(args)
        .stdout(stdout)
        .stdin(Stdio::piped())
        .spawn()?;
    if let Some(stdin) = stdin {
        cmd.stdin.take().unwrap().write_all(stdin)?;
    }

    let out = cmd.wait_with_output()?;
    if !out.status.success() {
        return Err(eyre!(
            "Command {command} exited with exit status {}",
            out.status
        ));
    }

    Ok(out.stdout)
}

fn nixos(open_editor: Option<String>, update: bool, flake: PathBuf, device: String) -> Result<()> {
    env::set_current_dir(&flake)?;
    if let Some(editor) = open_editor {
        run_command(&editor, None)?;
    }

    run_command("git", ["add", "."])?;
    let args = ["os", "switch", "-H", &device, "."]
        .into_iter()
        .chain(update.then_some("--update"));
    run_command("nh", args)?;
    run_command("git", ["commit", "-am", "update"])?;
    run_command("git", iter::once("push"))?;
    Ok(())
}

fn screenshot(area: ScreenshotArea) -> Result<()> {
    let mut path = dirs::picture_dir().wrap_err("Cannot determine pictures dir")?;
    path.push("screenshots");
    fs::create_dir_all(&path)?;
    const FMT: &str = "screenshot-%Y-%m-%d-%H:%M:%S.png";
    let file_name = Local::now().format(FMT).to_string();
    path.push(file_name);
    match area {
        ScreenshotArea::Fullscreen => run_command("grim", iter::once(path.to_str().unwrap())),
        ScreenshotArea::Window => {
            let sway_tree = swayipc::Connection::new()?.get_tree()?;
            let rect = sway_tree
                .find_focused(|node| node.node_type == NodeType::Con)
                .ok_or_eyre("Cannot get focused window")?
                .rect;
            let rect_formatted = format!("{},{} {}x{}", rect.x, rect.y, rect.width, rect.height);
            run_command("grim", ["-g", &rect_formatted, path.to_str().unwrap()])
        }
        ScreenshotArea::Region { slurp_fg, slurp_bg } => {
            let slurp_output = run_command_with_stdio(
                "slurp",
                ["-c", &slurp_fg, "-b", &slurp_bg],
                Stdio::piped(),
                None,
            )?;
            let region = String::from_utf8(slurp_output)?;
            run_command("grim", ["-g", region.trim(), path.to_str().unwrap()])
        }
    }?;

    wl_clipboard_rs::copy::copy(
        Options::new(),
        wl_clipboard_rs::copy::Source::Bytes(fs::read(&path)?.into_boxed_slice()),
        MimeType::Autodetect,
    )?;

    Notification::new()
        .summary("Screenshot")
        .body(&format!(
            "File saved as {}, and copied to clipboard",
            path.to_str().unwrap()
        ))
        .image(&path)?
        .show()?;
    Ok(())
}

fn scrollback(editor_name: String) -> Result<()> {
    let mut input = String::new();
    stdin().read_to_string(&mut input)?;

    const CONTROL_SEQUENCES: &str = r"\x1b\[[\x30-\x3F]*[\x20-\x2F]*[\x40-\x7E]";
    const INDEPENDENT_CONTROL_FUNCTIONS: &str = r"\x1b[\x60-\x7E]";
    const COMMAND_STRINGS: &str = r"\x1b[\x5F\x50\x5D\x5E][\x08-\x0D\x20-\x7E]*(\x1b\\|\x07)";
    const CARRIAGE_RETURN: &str = r"\r";
    let re = &format!(
        "({CONTROL_SEQUENCES}|{INDEPENDENT_CONTROL_FUNCTIONS}|{COMMAND_STRINGS}|{CARRIAGE_RETURN})"
    );

    let str = Regex::new(re)?.replace_all(input.trim(), "");
    run_command_with_stdio(&editor_name, None, Stdio::inherit(), Some(str.as_bytes()))?;
    Ok(())
}
