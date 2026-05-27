use std::sync::Arc;
use parking_lot::Mutex;
use std::thread;
use std::net::{TcpListener, TcpStream};
use std::io::{Write, BufReader, BufRead, BufWriter};
use crate::traits::Device;

pub struct Monitor {
    devices: Arc<Mutex<Vec<Arc<dyn Device>>>>,
}

impl Monitor {
    pub fn new() -> Self {
        Self {
            devices: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn register_device(&mut self, device: Arc<dyn Device>) {
        self.devices.lock().push(device);
    }

    pub fn start_server(self: Arc<Self>, addr: String) {
        thread::spawn(move || {
            let listener = TcpListener::bind(&addr).expect("Failed to bind monitor port");
            println!("Monitor listening on {}", addr);
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        let devices = self.devices.clone();
                        thread::spawn(move || {
                            handle_client(stream, devices);
                        });
                    }
                    Err(e) => eprintln!("Monitor accept error: {}", e),
                }
            }
        });
    }
}

fn handle_client(stream: TcpStream, devices: Arc<Mutex<Vec<Arc<dyn Device>>>>) {
    // Register this connection with DevLog so log output is broadcast here.
    // Wrap in CrlfWriter so bare \n from log messages renders as CRLF on the
    // telnet client side.
    if let Some(dl) = crate::devlog::DEVLOG.get() {
        let w: crate::devlog::DevLogWriter = Arc::new(Mutex::new(
            BufWriter::new(crate::telnet::CrlfWriter::new(stream.try_clone().unwrap()))
        ));
        dl.add_sink(w);
    }

    // Monitor is line-oriented: stay in NVT (passive telnet — strip inbound
    // IAC, decline whatever the client offers, but don't initiate). The
    // client keeps its local echo and line editor. Outbound goes through
    // CrlfWriter so bare \n becomes CRLF on the wire as NVT requires.
    let mut reader = BufReader::new(crate::telnet::TelnetReader::new_passive(stream.try_clone().unwrap()));
    let mut writer = BufWriter::new(crate::telnet::CrlfWriter::new(stream.try_clone().unwrap()));
    let mut line = String::new();
    
    {
        let logo = include_str!("ascii-art.txt");
        for line in logo.lines() {
            let _ = write!(writer, "{}\r\n", line);
        }
    }
    let _ = write!(writer, "\r\n{} Monitor\r\n> ", crate::machine::emulator_name());
    let _ = writer.flush();
    
    loop {
        line.clear();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        
        let trimmed = line.trim();
        if trimmed.is_empty() {
            let _ = write!(writer, "> ");
            let _ = writer.flush();
            continue;
        }
        
        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        let cmd = parts[0];
        let args = &parts[1..];
        
        if cmd == "quit" || cmd == "exit" {
            break;
        }
        
        let mut target_device: Option<Arc<dyn Device>> = None;
        let mut is_help = false;
        
        if cmd == "help" {
            is_help = true;
        }

        {
            let devs = devices.lock();
            if is_help {
                for dev in devs.iter() {
                    for (c, h) in dev.register_commands() {
                        let _ = writeln!(writer, "{:12} - {}", c, h);
                    }
                }
            } else {
                for dev in devs.iter() {
                    let cmds = dev.register_commands();
                    if cmds.iter().any(|(c, _)| c == cmd) {
                        target_device = Some(dev.clone());
                        break;
                    }
                }
            }
        }
        
        if !is_help {
            if let Some(dev) = target_device {
                let cmd_writer = Box::new(BufWriter::new(crate::telnet::CrlfWriter::new(stream.try_clone().unwrap())));
                match dev.execute_command(cmd, args, cmd_writer) {
                    Ok(_) => {
                    }
                    Err(e) => {
                        let _ = write!(writer, "Error: {}\n", e);
                    }
                }
            } else {
            let _ = writeln!(writer, "Unknown command. Type 'help' for list.");
            }
        }
        
        let _ = write!(writer, "> ");
        let _ = writer.flush();
    }
}