use crossbeam_channel::bounded;
use crossbeam_channel::{Select, Sender};
use libc::kill;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

type Signal = i32;

#[derive(Debug)]
pub struct Process {
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
