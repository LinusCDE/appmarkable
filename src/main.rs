#[macro_use]
extern crate log;

use libremarkable::input::{
    MultitouchEvent,
    InputDevice,
    InputEvent,
    ev::EvDevContext,
    InputDeviceState,
};
use libremarkable::framebuffer::common::{DISPLAYHEIGHT, DISPLAYWIDTH};
use libremarkable::{image, cgmath, device::{CURRENT_DEVICE, Model}};
use nix::unistd::Pid;
use nix::sys::signal::{self, Signal};
use signal_hook;
use std::env;
use std::process::{Child, Command, exit, ExitStatus};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use std::time::{Duration, SystemTime};
use std::thread::sleep;

use clap::{Clap, crate_version, crate_authors};
use env_logger;
use wait_timeout::ChildExt;

mod canvas;

use canvas::{Canvas, mxcfb_rect, Point2};

const CORNER_SIZE: u32 = 100;
const CORNER_BOTTOM_LEFT: mxcfb_rect = mxcfb_rect { top: DISPLAYHEIGHT as u32 - CORNER_SIZE, left: 0, width: CORNER_SIZE, height: CORNER_SIZE };
const CORNER_BOTTOM_RIGHT: mxcfb_rect = mxcfb_rect { top: DISPLAYHEIGHT as u32 - CORNER_SIZE, left: DISPLAYWIDTH as u32 - CORNER_SIZE, width: CORNER_SIZE, height: CORNER_SIZE };

#[derive(Clap, Debug)]
#[clap(version = crate_version!(), author = crate_authors!())]
struct Opts {
    #[clap(long, short, about = "Display a custom full image instead of name and icon.")]
    custom_image: Option<String>,

    #[clap(long, short, about = "Path for icon to display")]
    icon: Option<String>,

    #[clap(long, about = "Size of icon to display (squared)", default_value = "500")]
    icon_size: u16,

    #[clap(long, short, about = "App name to display")]
    name: Option<String>,

    #[clap(about = "Full path to the executable")]
    command: String,
    
    #[clap(multiple = true, about = "Arguments for the executable")]
    args: Vec<String>,
}

fn main() {
    // Setting up logging
    if let Err(_) = env::var("RUST_LOG") {
        // Default logging level: "info" instead of "error"
        env::set_var("RUST_LOG", "INFO");
    }
    env_logger::init();

    if CURRENT_DEVICE.model == Model::Gen2 && std::env::var_os("RM2FB_ACTIVE").is_none() {
        error!("You executed appmarkable on a reMarkable 2 without using rm2fb-client.");
        error!("      This suggests that you didn't use/enable rm2fb. Without rm2fb you");
        error!("      won't see anything on the display!");
        error!("      ");
        error!("      See https://github.com/ddvk/remarkable2-framebuffer/ on how to solve");
        error!("      this. Launchers (installed through toltec) should automatically do this.");
    }

    let sigint_received = Arc::new(AtomicBool::new(false));
    let sigterm_received = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::SIGINT, Arc::clone(&sigint_received)).expect("Failed to register SIGINT handler.");
    signal_hook::flag::register(signal_hook::SIGTERM, Arc::clone(&sigterm_received)).expect("Failed to register SIGTERM handler.");

    // Parsing arguments
    let opts: Opts = Opts::parse();

    // Argument validation
    if opts.icon_size < 50 || opts.icon_size > 1404 {
        error!("Icon size invalid. Must be between 50 and 1404!");
        exit(1);
    }

    // Find app name
    let name = if let Some(app_name) = opts.name {
        app_name.clone()
    }else {
        warn!("No app name was provided. Using command instead.");
        opts.command.clone()
    };

    // Start process
    info!("Staring process \"{}\" with arguments: {:?}", &opts.command, &opts.args);
    let mut proc = Command::new(&opts.command).args(&opts.args).spawn().unwrap();
    info!("Process started");

    // Draw screen
    let mut canvas = Canvas::new();
    canvas.clear();

    if let Some(custom_image_path) = opts.custom_image {
        draw_custom_image(&mut canvas, &custom_image_path);
        warn!("Using a custom image will NOT display how to quit the app.");
        warn!("To quit the app, touch both bottom corners.");
    }else if let Some(icon_path) = opts.icon {
        draw_base(&mut canvas);
        draw_icon_and_name(&mut canvas, &name, opts.icon_size, &icon_path);
    }else {
        draw_base(&mut canvas);
        draw_name(&mut canvas, &name);
    }
    canvas.update_full();

    // Setting up gpio input
    let (input_tx, input_rx) = std::sync::mpsc::channel::<InputEvent>();
    let mut ev_context = EvDevContext::new(InputDevice::Multitouch, input_tx);
    ev_context.start();

    // Input loop and waiting for process to exit
    let pause_duration = Duration::from_millis(150);
    let mut last_status_rect: Option<mxcfb_rect> = None;
    loop {
        let before_input = SystemTime::now();

        // Process input events
        let mut was_press = false;
        for input_event in input_rx.try_iter() {
            if let InputEvent::MultitouchEvent { event: mt_event } = input_event {
                if let MultitouchEvent::Press { .. } = mt_event {
                    was_press = true;
                }
            }
        }

        let fingers = match ev_context.state {
            InputDeviceState::MultitouchState(ref state) => {
                state.fingers.lock().expect("Failed to lock finger states")
            }
            _ => panic!("Unexpected!")
        };

        let trigger_quit = if was_press && fingers.values().filter(|f| f.pressed).count() == 2 {
            let hitting_bottom_left = fingers.values().filter(|f| f.pressed).any(|f| Canvas::is_hitting(f.pos, CORNER_BOTTOM_LEFT));
            let hitting_bottom_right = fingers.values().filter(|f| f.pressed).any(|f| Canvas::is_hitting(f.pos, CORNER_BOTTOM_RIGHT));

            hitting_bottom_left && hitting_bottom_right
        }else {
            false
        };
        drop(fingers); // Prevent mutex from being locked even when waiting


        // Check if user requested quiting (using buttons or the terminal)
        if (trigger_quit)
            || sigint_received.load(Ordering::Relaxed) || sigterm_received.load(Ordering::Relaxed) {

            info!("Termination requested by user. Killing {}...", &opts.command);
            if let Some(rect) = last_status_rect { canvas.clear_area(&rect); }
            last_status_rect = Some(canvas.draw_text(cgmath::Point2 { x: None, y: Some(1872 - 300)}, "Killing process...", 60.0));
            canvas.update_partial(&last_status_rect.unwrap());

            if let Err(e) = kill_process(&mut proc) {
                error!("kill_process() failed: {}", e);
                info!("The application will continue to run until either the process terminates or killing succeeds.");

                canvas.clear_area(&last_status_rect.unwrap());
                last_status_rect = Some(canvas.draw_text(cgmath::Point2 { x: None, y: Some(1872 - 300)}, &format!("Failed to kill {}", &opts.command), 60.0));
                canvas.update_partial(&last_status_rect.unwrap());
                continue;
            }

            info!("Process was successfully killed. Exiting...");

            // Clear screen
            canvas.clear();
            canvas.update_full();
            exit(0);
        }

        // Check for process self termination
        if let Ok(status) = wait_termination(&mut proc, 50, true) {
            log_exit_status(&status);
            info!("Process exited by itself. Quitting...");
            canvas.clear();
            canvas.update_full();
            exit(0);
        }

        // Wait remaining pause time
        let elapsed = before_input.elapsed().unwrap();
        if elapsed < pause_duration {
            sleep(pause_duration - elapsed);
        }
    }
}


fn draw_base(canvas: &mut Canvas) {
    // Draw centered text
    canvas.draw_text(cgmath::Point2 { x: None, y: Some(1872 - 30) }, "Touch both bottom corners to manually quit.", 35.0);
    canvas.draw_rect(Point2 { x: Some(CORNER_BOTTOM_LEFT.left as i32), y: Some(CORNER_BOTTOM_LEFT.top as i32) }, CORNER_BOTTOM_LEFT.size().into(), 1);
    canvas.draw_rect(Point2 { x: Some(CORNER_BOTTOM_RIGHT.left as i32), y: Some(CORNER_BOTTOM_RIGHT.top as i32) }, CORNER_BOTTOM_RIGHT.size().into(), 1);
}

fn draw_name(canvas: &mut Canvas, name: &str) {
    info!("Drawing name only screen...");
    // Draw centered text
    let rect = canvas.draw_text(cgmath::Point2 { x: None, y: None }, name, 50.0);
    canvas.draw_text(cgmath::Point2 { x: None, y: Some(rect.top as i32 + rect.height as i32 + 25) }, "is running", 25.0);
}


fn draw_icon_and_name(canvas: &mut Canvas, name: &str, icon_size: u16, icon_path: &str) {
    info!("Drawing icon and name screen...");
    let img_rect = match image::open(icon_path) {
        Ok(icon) => {
            let start = SystemTime::now();
            let resized = icon.resize(icon_size as u32, icon_size as u32, image::imageops::FilterType::Lanczos3);
            debug!("Resizing image took {:?}", start.elapsed().unwrap()); // Prints when env RUST_LOG=debug
            canvas.draw_image(cgmath::Point2 { x: None /* Center */, y: None /* Center */ }, &resized, true)
        },
        Err(e) => {
            error!("Failed to load icon: {}", e);
            return;
        }
    };

    let rect = canvas.draw_text(cgmath::Point2 {
            x: None /* Center */,
            y: Some(img_rect.top as i32 + img_rect.height as i32 + 55)
        }, name, 50.0);
    canvas.draw_text(cgmath::Point2 { x: None, y: Some(rect.top as i32 + rect.height as i32 + 25) }, "is running", 25.0);
}


fn draw_custom_image(canvas: &mut Canvas, image_path: &str) {
    info!("Drawing custom icon screen...");
    match image::open(image_path) {
        Ok(img) => {
            canvas.draw_image(cgmath::Point2 { x: None /* Center */, y: None /* Center */ }, &img, true);
        },
        Err(e) => {
            error!("Failed to load custom image: {}", e);
            canvas.draw_text(cgmath::Point2 { x: None, y: Some(1872 - 50) }, "Failed to load custom image (see console)!", 50.0);
            return;
        }
    };
}


fn kill_process(child: &mut Child) -> Result<(), Box<dyn std::error::Error>> {
    let child_pid = Pid::from_raw(child.id() as i32);
    info!("Killing process gracefully...");
    signal::kill(child_pid, Signal::SIGINT)?;
    
    // Terminated gracefully
    info!("Waiting for process to exit...");

    match wait_termination(child, 3000, false) {
        Ok(status) => {
            log_exit_status(&status);
            Ok(())
        },
        Err(e) => {
            // Waiting on exit failed.
            warn!("Graceful kill (SIGINT) failed: {}", e);
            warn!("Stabbing process (SIGKILL)...");
            if let Err(e) = child.kill() {
                error!("Stabbing failed. Quitting anyway. Error: {}", e);
            }
            child.wait()?;
            Ok(())
        }
    }
}

fn wait_termination(child: &mut Child, timeout_ms: u32, require_actual_code: bool) -> Result<ExitStatus, Box<dyn std::error::Error>> {
    let status = child.wait_timeout_ms(timeout_ms)?;
    let status = status.ok_or("Got not exit status. Program is likely still running.")?;
    if require_actual_code {
        status.code().ok_or("Got ExitStatus bot no code.")?; // Ensure code exists
    }
    Ok(status)
}

fn log_exit_status(status: &ExitStatus) {
    if status.success() {
        info!("Process exited with code 0");
    }else {
        if let Some(code) = status.code() {
            warn!("Process exit with code {}", code);
        }else {
            warn!("Process exit with unknown code. Feel free to fix this bug here: https://github.com/LinusCDE/appmarkable");
            // If you come here to fix this bug. See the wait_termination() method above.
            // Usually there should be a code but sometimes there isn't. I haven't
            // looked too deeply into it, feel free to do that (docs and stuff)
            // and fix that. :)
        }
        
    }
}
