#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod icon;
mod info;
mod platform;

use std::path::PathBuf;

use app::{Launch, ThemeChoice};
use eframe::egui;
use oink_eeg::montage::MontageId;

const USAGE: &str = "\
Oink: a small, fast viewer for EDF, EDF+, BDF and BDF+ recordings.

Usage:
  oink [FILE] [options]
  oink --sample           open the built-in sample recording
  oink --info FILE        print what the viewer makes of a file and exit

Options:
  --montage NAME     as-recorded | banana | transverse | average | tcp,
                     or an EDFbrowser montage file (.mtg)
  --at SECONDS       start the view this far into the recording
  --page SECONDS     seconds per page
  --theme NAME       system | light | dark
  --no-sidebar       start with the sidebar hidden
  --tab NAME         montage | events | file
  --map NAME         scalp map on the sidebar's head: delta | theta | alpha | beta | gamma |
                     alpha-delta | theta-beta | slow-fast
  --size WxH         window size in points
  --screenshot PNG   render the window to PNG and quit
";

enum Command {
    Run(Launch, Option<[f32; 2]>),
    Info(PathBuf),
    Icon(PathBuf, u32),
    Help,
}

fn parse(args: &[String]) -> Result<Command, String> {
    let mut launch = Launch::default();
    let mut size = None;
    let mut it = args.iter().filter(|a| !a.starts_with("-psn_"));
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().cloned().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "-h" | "--help" => return Ok(Command::Help),
            "--info" => return Ok(Command::Info(PathBuf::from(value("--info")?))),
            "--write-icon" => {
                let path = PathBuf::from(value("--write-icon")?);
                let px = it.next().and_then(|s| s.parse().ok()).unwrap_or(1024);
                return Ok(Command::Icon(path, px));
            }
            "--montage" => {
                let v = value("--montage")?;
                if v.to_ascii_lowercase().ends_with(".mtg") {
                    launch.montage_file = Some(PathBuf::from(v));
                    continue;
                }
                launch.montage = Some(match v.to_ascii_lowercase().as_str() {
                    "as-recorded" | "recorded" | "raw" => MontageId::AsRecorded,
                    "banana" | "double-banana" | "lb" | "bipolar" => MontageId::Banana,
                    "transverse" | "tb" => MontageId::Transverse,
                    "average" | "avg" | "car" => MontageId::Average,
                    "tcp" => MontageId::Tcp,
                    other => return Err(format!("unknown montage {other:?}")),
                })
            }
            "--at" => launch.at = Some(value("--at")?.parse().map_err(|_| "--at needs seconds")?),
            "--page" => launch.page = Some(value("--page")?.parse().map_err(|_| "--page needs seconds")?),
            "--theme" => {
                launch.theme = Some(match value("--theme")?.as_str() {
                    "light" => ThemeChoice::Light,
                    "dark" => ThemeChoice::Dark,
                    "system" => ThemeChoice::System,
                    other => return Err(format!("unknown theme {other:?}")),
                })
            }
            "--no-sidebar" => launch.sidebar = Some(false),
            "--sample" => launch.sample = true,
            "--tab" => {
                use app::Tab;
                launch.tab = Some(match value("--tab")?.as_str() {
                    "montage" => Tab::Montage,
                    "events" => Tab::Events,
                    "file" => Tab::File,
                    other => return Err(format!("unknown tab {other:?}")),
                })
            }
            "--map" => {
                let v = value("--map")?;
                launch.map = Some(oink_eeg::maps::Measure::from_key(&v).ok_or_else(|| format!("unknown map {v:?}"))?);
            }
            "--size" => {
                let v = value("--size")?;
                let (w, h) = v.split_once('x').ok_or("--size needs WxH")?;
                size = Some([w.parse().map_err(|_| "bad width")?, h.parse().map_err(|_| "bad height")?]);
            }
            "--screenshot" => launch.screenshot = Some(PathBuf::from(value("--screenshot")?)),
            "--menu" => launch.menu = Some(value("--menu")?),
            "--select" => launch.select = Some(value("--select")?),
            "--measure" => {
                let v = value("--measure")?;
                let (a, b) = v.split_once(',').ok_or("--measure needs START,END")?;
                launch.measure = Some((a.parse().map_err(|_| "bad start")?, b.parse().map_err(|_| "bad end")?));
            }
            other if other.starts_with('-') => return Err(format!("unknown option {other}")),
            file => launch.file = Some(PathBuf::from(file)),
        }
    }
    Ok(Command::Run(launch, size))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    #[cfg(windows)]
    if args.iter().any(|a| a.starts_with("--info") || a == "--help" || a == "-h") {
        platform::attach_console();
    }
    let result = match parse(&args) {
        Ok(Command::Help) => {
            print!("{USAGE}");
            Ok(())
        }
        Ok(Command::Info(path)) => info::print(&path),
        Ok(Command::Icon(path, px)) => icon::write_png(&path, px).map_err(|e| e.to_string()),
        Ok(Command::Run(launch, size)) => run(launch, size).map_err(|e| e.to_string()),
        Err(e) => {
            eprintln!("{e}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

fn run(launch: Launch, size: Option<[f32; 2]>) -> Result<(), Box<dyn std::error::Error>> {
    let screenshot = launch.screenshot.is_some();
    let viewport = egui::ViewportBuilder::default()
        .with_title("Oink")
        .with_app_id("oink")
        .with_inner_size(size.unwrap_or([1360.0, 860.0]))
        .with_min_inner_size([720.0, 460.0])
        .with_icon(icon::icon_data(256));
    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Wgpu,
        multisampling: 0,
        depth_buffer: 0,
        persist_window: !screenshot,
        centered: true,
        wgpu_options: egui_wgpu::WgpuConfiguration {
            // One frame in flight: a full-window buffer less, and lower latency.
            desired_maximum_frame_latency: Some(1),
            ..Default::default()
        },
        ..Default::default()
    };
    // Our own event loop, so the macOS "open file" delegate is in place before launch.
    let event_loop = winit::event_loop::EventLoop::<eframe::UserEvent>::with_user_event().build()?;
    #[cfg(target_os = "macos")]
    platform::install_open_handler();
    let mut app = eframe::create_native(
        "oink",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, launch)))),
        &event_loop,
    );
    event_loop.run_app(&mut app)?;
    Ok(())
}
