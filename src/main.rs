#[macro_use]
extern crate log;

use libremarkable::input::{
    gpio::GPIOEvent,
    gpio::PhysicalButton,
    InputDevice,
    InputEvent,
    ev::EvDevContext,
};
use libremarkable::{image, cgmath};
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

use canvas::{Canvas, mxcfb_rect};

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

    #[clap(multiple = true, about = "Full path to the executable")]
    command: String,
    
    #[clap(multiple = true, about = "Arguments for the executable")]
    args: Vec<String>,
}

fn main() {
    let sigint_received = Arc::new(AtomicBool::new(false));
    let sigterm_received = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::SIGINT, Arc::clone(&sigint_received)).expect("Failed to register SIGINT handler.");
    signal_hook::flag::register(signal_hook::SIGTERM, Arc::clone(&sigterm_received)).expect("Failed to register SIGTERM handler.");

    // Setting up logging
    if let Err(_) = env::var("RUST_LOG") {
        // Default logging level: "info" instead of "error"
        env::set_var("RUST_LOG", "INFO");
    }
    env_logger::init();

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
        warn!("To quit the app, press power and home together.");
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
    EvDevContext::new(InputDevice::GPIO, input_tx).start();
    
    // Input loop and waiting for process to exit
    let pause_duration = Duration::from_millis(150);
    let mut power_pressed = false;
    let mut home_pressed = false;
    let mut last_status_rect: Option<mxcfb_rect> = None;
    loop {
        let before_input = SystemTime::now();

        // Process input events
        for event in input_rx.try_iter() {
            if let InputEvent::GPIO { event: gpio_event } = event {
                match gpio_event {
                    GPIOEvent::Press { button } => {
                        match button {
                            PhysicalButton::POWER => power_pressed = true,
                            PhysicalButton::MIDDLE => home_pressed = true,
                            _ => {}
                        }
                    },
                    GPIOEvent::Unpress { button } => {
                        match button {
                            PhysicalButton::POWER => power_pressed = false,
                            PhysicalButton::MIDDLE => home_pressed = false,
                            _ => {}
                        }
                    },
                    _ => { }
                }
            }
        }

        // Check if user requested quiting (using buttons or the terminal)
        if (home_pressed && power_pressed)
            || sigint_received.load(Ordering::Relaxed) || sigterm_received.load(Ordering::Relaxed) {
            // Prevent running this code again is triggered with buttons (dirty hack)
            home_pressed = false;
            power_pressed = false;
            
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
    canvas.draw_text(cgmath::Point2 { x: None, y: Some(1872 - 30) }, "Press the power and home button together to manually quit.", 35.0);
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
            let resized = icon.resize(icon_size as u32, icon_size as u32, image::FilterType::Lanczos3);
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
    let img_rect = match image::open(image_path) {
        Ok(img) => {
            canvas.draw_image(cgmath::Point2 { x: None /* Center */, y: None /* Center */ }, &img, true)
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