use clap::{app_from_crate, crate_authors, crate_description, crate_name, crate_version, Arg};
use crossbeam_channel::bounded;
use crossbeam_channel::{Select, Sender};
use libc::kill;
use serde_derive::Deserialize;
use signal_hook::iterator::Signals;
use std::collections::HashMap;
use std::fs::File;
use std::io::prelude::*;
use std::process;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;

type Signal = i32;

static CHILD_PROCESS_COUNT: AtomicUsize = AtomicUsize::new(0);
static IS_SIGNALED: AtomicBool = AtomicBool::new(false);

fn main() {
    let matches = app_from_crate!()
        .arg(
            Arg::with_name("config")
                .default_value("init.toml")
                .help("Path to config file.")
                .required(true),
        )
        .get_matches();

    let config_path = matches
        .value_of("config")
        .expect("failed to read config arg");

    let config = read_config(config_path);

    println!("{:?}", config);

    let (exit_tx, exit_rx) = bounded::<()>(0);

    let processes: Vec<Process> = config
        .processes
        .iter()
        .map(|proc_def| {
            CHILD_PROCESS_COUNT.fetch_add(1, Ordering::Relaxed);
            let (name, command) = proc_def;
            Process::new(name.clone(), command.clone(), exit_tx.clone())
        })
        .collect();

    let processes_thread_safe = Arc::new(processes);

    let is_pid1 = process::id() == 1;

    let (signal_tx, signal_rx) = bounded::<Signal>(0);
    let signal_tx_clone = signal_tx.clone();
    register_sig_handler(signal_tx_clone, is_pid1);

    if is_pid1 {
        println!("running as pid1");

        let processes_thread_safe = processes_thread_safe.clone();
        thread::spawn(move || {
            let signal = signal_rx
                .recv()
                .expect("failed to receive signal message in main");
            processes_thread_safe.iter().for_each(|process| {
                process.send_signal(signal);
            })
        });
    }
    let signal_tx_clone = signal_tx.clone();
    thread::spawn(move || {
        loop {
            exit_rx.recv().expect("failed to receive exit message");
            let remaining_processes = CHILD_PROCESS_COUNT.fetch_sub(1, Ordering::Relaxed) - 1;
            if remaining_processes == 0 {
                break;
            }
            if !IS_SIGNALED.load(Ordering::Relaxed) {
                signal_tx_clone
                    .send(signal_hook::SIGTERM)
                    .expect("failed to send signal message based on exit message");
            }
        }
        println!("done");
    })
    .join()
    .expect("failed to join exit loop thread");
}

#[derive(Deserialize, Debug)]
struct Config {
    processes: HashMap<String, String>,
}

fn read_config(config_path: &str) -> Config {
    let mut config_file = File::open(config_path).expect("failed to open config file");
    let mut config_toml_string = String::new();
    config_file
        .read_to_string(&mut config_toml_string)
        .expect("failed to read from config file");
    toml::from_str(config_toml_string.as_ref()).expect("failed to parse config file")
}

fn register_sig_handler(signal_tx: Sender<Signal>, is_pid1: bool) {
    let signals = Signals::new(&[
        signal_hook::SIGTERM,
        signal_hook::SIGINT,
        signal_hook::SIGQUIT,
    ])
    .expect("failed to register signal handler");

    thread::spawn(move || {
        signals.forever().for_each(|signal| {
            IS_SIGNALED.store(true, Ordering::Relaxed);
            if is_pid1 {
                signal_tx
                    .send(signal)
                    .expect("failed to send signal from handler");
            }
        });
    });
}

#[derive(Debug)]
struct Process {
    name: String,
    pid: u32,
    signal_tx: Sender<Signal>,
    is_alive: Arc<AtomicBool>,
}

impl Process {
    pub fn new(name: String, command: String, exit_tx: Sender<()>) -> Self {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(&command)
            .spawn()
            .expect(&format!("failed to execute {}: `{}`", name, command));

        let pid = child.id();
        let is_alive = Arc::new(AtomicBool::new(true));
        let (signal_tx, signal_rx) = bounded::<Signal>(0);
        let (local_exit_tx, local_exit_rx) = bounded::<()>(0);

        let name_clone = name.clone();
        thread::spawn(move || loop {
            let mut select = Select::new();
            select.recv(&signal_rx);
            select.recv(&local_exit_rx);
            select.ready();
            if let Ok(signal) = signal_rx.try_recv() {
                let signal_name = match signal {
                    signal_hook::SIGTERM => "SIGTERM",
                    signal_hook::SIGINT => "SIGINT",
                    signal_hook::SIGQUIT => "SIGQUIT",
                    _ => "SIGUNKNOWN",
                };
                println!("sending {} to {}", signal_name, name_clone);
                unsafe {
                    kill(pid as i32, signal);
                }
            }
            if let Ok(()) = local_exit_rx.try_recv() {
                break;
            }
        });

        let is_alive_clone = is_alive.clone();
        let name_clone = name.clone();
        thread::spawn(move || {
            let exit_status = child
                .wait()
                .expect(&format!("failed to wait on {}", name_clone));
            is_alive_clone.store(false, Ordering::Relaxed);
            println!("{} exited with: {}", name_clone, exit_status);
            exit_tx
                .send(())
                .expect("failed to send exit message in Process::new");
            local_exit_tx
                .send(())
                .expect("failed to send local exit message in Process::new");
        });

        Self {
            name,
            pid,
            signal_tx,
            is_alive,
        }
    }

    pub fn send_signal(&self, signal: Signal) {
        if self.is_alive.load(Ordering::Relaxed) {
            self.signal_tx
                .send(signal)
                .expect("failed to send signal message in Process::send_signal");
        }
    }
}
