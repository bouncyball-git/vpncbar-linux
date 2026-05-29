//! VpncBar (Linux/Wayland) — tray front-end for vpnc and openconnect.
//!
//! Default launch starts the SNI tray under a GTK application loop. A handful of
//! headless subcommands remain for debugging the core.

// Some helpers are consumed across milestones; quiet the noise meanwhile.
#![allow(dead_code)]

mod app;
mod backend;
mod config_import;
mod model;
mod notify;
mod privilege;
mod secrets;
mod sys;
mod tray;
mod tray_icon;
mod tunnel;
mod ui;

use app::{App, Cmd};
use gtk::glib;
use gtk::prelude::*;
use std::rc::Rc;

const APP_ID: &str = "io.github.vpncbar";

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // Headless subcommands (debugging) — anything else launches the GUI.
    if let Some(c) = std::env::args().nth(1) {
        if matches!(c.as_str(), "list" | "connect" | "disconnect" | "info" | "groups" | "set-secret" | "config") {
            return cli::run();
        }
    }
    run_gui();
}

fn run_gui() {
    // A unique application id gives single-instance behaviour for free: a second
    // launch just activates the running one (replacing the macOS bundle-id check).
    let application = gtk::Application::builder().application_id(APP_ID).build();

    // Tray-only app: nothing to do on activate, but GTK warns if it's unhandled.
    application.connect_activate(|_| {});

    application.connect_startup(|application| {
        // Build the tray and its command channel.
        let (tx, rx) = async_channel::unbounded::<Cmd>();
        let tray = tray::Tray {
            profiles: vec![],
            connected: Default::default(),
            tx: tx.clone(),
        };
        let service = ksni::TrayService::new(tray);
        let handle = service.handle();
        service.spawn();

        let app = App::new(handle, tx);
        ui::install_hooks(&app, application);

        // Keep the GTK application alive even with no window open (tray-only).
        let hold = application.hold();

        // Drain tray/UI commands on the main thread.
        {
            let app = app.clone();
            glib::spawn_future_local(async move {
                let _hold = hold; // tie the hold guard to the app's lifetime
                while let Ok(cmd) = rx.recv().await {
                    app.handle(cmd);
                }
            });
        }

        // Poll tunnel state periodically (live timers + drop detection).
        {
            let app = app.clone();
            glib::timeout_add_seconds_local(2, move || {
                app.refresh();
                glib::ControlFlow::Continue
            });
        }

        // Graceful teardown on SIGTERM/SIGINT: disconnect tunnels, then quit.
        install_signal(&app, libc_sigterm());
        install_signal(&app, libc_sigint());

        app.refresh(); // initial paint
    });

    // Don't pass our argv to GTK (we handle subcommands ourselves).
    application.run_with_args::<&str>(&[]);
}

fn libc_sigterm() -> i32 {
    15
}
fn libc_sigint() -> i32 {
    2
}

fn install_signal(app: &Rc<App>, signum: i32) {
    let app = app.clone();
    glib::unix_signal_add_local(signum, move || {
        app.disconnect_all_sync();
        std::process::exit(0);
        #[allow(unreachable_code)]
        glib::ControlFlow::Break
    });
}

/// Headless debugging CLI (unchanged from milestone 1).
mod cli {
    use crate::backend::{self, ActionResult};
    use crate::model::{load_profiles, Profile};
    use crate::{model, secrets, tunnel};

    fn find<'a>(profiles: &'a [Profile], name: &str) -> Option<&'a Profile> {
        profiles.iter().find(|p| p.name == name)
    }
    fn print_result(r: ActionResult) {
        match r {
            ActionResult::Ok => println!("ok"),
            ActionResult::Message(m) => eprintln!("{m}"),
        }
    }

    pub fn run() {
        let args: Vec<String> = std::env::args().collect();
        let cmd = args[1].as_str();
        let profiles = load_profiles();
        match cmd {
            "list" => {
                println!("{} profile(s) in {}", profiles.len(), model::profiles_path().display());
                let live = tunnel::connected_tunnels(&profiles);
                for p in &profiles {
                    let state = match live.get(&p.name) {
                        Some((pid, secs)) => format!("UP pid={pid} {}", tunnel::format_elapsed(*secs)),
                        None => "down".into(),
                    };
                    println!(
                        "  [{:>4}] {:20} {}  ({})",
                        if p.is_openconnect() { "oc" } else { "vpnc" },
                        p.name, p.gateway, state
                    );
                }
            }
            "connect" => match args.get(2).and_then(|n| find(&profiles, n)) {
                Some(p) => print_result(backend::connect(p, args.get(3).map(String::as_str))),
                None => eprintln!("usage: vpncbar connect <name> [otp]"),
            },
            "disconnect" => match args.get(2).and_then(|n| find(&profiles, n)) {
                Some(p) => print_result(backend::disconnect(p)),
                None => eprintln!("usage: vpncbar disconnect <name>"),
            },
            "info" => match args.get(2).and_then(|n| find(&profiles, n)) {
                Some(p) => println!("{:#?}", tunnel::read_tunnel_info(p)),
                None => eprintln!("usage: vpncbar info <name>"),
            },
            "groups" => match args.get(2) {
                Some(server) => {
                    for (g, otp) in backend::openconnect::group_list(server, args.get(3).map(String::as_str)) {
                        println!("{g}{}", if otp { "  (2FA)" } else { "" });
                    }
                }
                None => eprintln!("usage: vpncbar groups <server> [servercert-pin]"),
            },
            "set-secret" => match (args.get(2).and_then(|n| find(&profiles, n)), args.get(3), args.get(4)) {
                (Some(p), Some(kind), Some(val)) => {
                    let acct = if kind == "secret" { &p.id } else { &p.username };
                    let ok = secrets::store(&secrets::kc_service(p, kind), acct, val);
                    println!("{}", if ok { "stored" } else { "failed" });
                }
                _ => eprintln!("usage: vpncbar set-secret <name> <secret|password> <value>"),
            },
            "config" => match args.get(2).and_then(|n| find(&profiles, n)) {
                Some(p) if !p.is_openconnect() => match backend::vpnc::build_config(p) {
                    Ok(c) => print!("{c}"),
                    Err(ActionResult::Message(m)) => eprintln!("{m}"),
                    Err(_) => {}
                },
                Some(p) => println!("pkexec {}", backend::openconnect::build_args(p).join(" ")),
                None => eprintln!("usage: vpncbar config <name>"),
            },
            _ => {}
        }
    }
}
