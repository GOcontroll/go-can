use std::io::{self, Write};
use std::process::Command as Proc;

use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind},
    execute, queue,
    style::{Color, Print, ResetColor, SetForegroundColor},
    terminal::{self, ClearType},
};

use crate::cli;
use crate::config;
use crate::error::Error;
use crate::output;

const BITRATES: &[(u32, &str)] = &[
    (125_000, "125 kbps"),
    (250_000, "250 kbps"),
    (500_000, "500 kbps"),
    (1_000_000, "1000 kbps"),
];

struct Bus {
    name: String,
    /// Bitrate from config file, or detected live from running interface.
    bitrate: Option<u32>,
    up: bool,
}

enum Mode {
    List { cursor: usize },
    Bitrate { bus: usize, cursor: usize, error: Option<String> },
}

enum Action {
    Update(Mode),
    Quit,
}

pub fn run() -> Result<(), Error> {
    let snap = output::Snapshot::collect()?;

    let mut buses: Vec<Bus> = snap
        .interfaces
        .iter()
        .filter(|i| i.present)
        .map(|i| {
            // Prefer the config file; fall back to the live interface for controllers
            // that were configured via the old /etc/network/interfaces.d/ mechanism.
            let bitrate = config::load(&i.name)
                .ok()
                .map(|c| c.bitrate)
                .or_else(|| read_live_bitrate(&i.name));
            Bus { name: i.name.clone(), bitrate, up: i.up }
        })
        .collect();

    if buses.is_empty() {
        println!("No CAN interfaces present on this system.");
        return Ok(());
    }

    let mut mode = Mode::List { cursor: 0 };
    let mut stdout = io::stdout();

    terminal::enable_raw_mode()?;
    execute!(stdout, terminal::EnterAlternateScreen, cursor::Hide)?;

    let result = event_loop(&mut stdout, &mut buses, &mut mode, &snap.baseboard);

    let _ = execute!(stdout, terminal::LeaveAlternateScreen, cursor::Show);
    let _ = terminal::disable_raw_mode();

    result
}

/// Read the live bitrate of a running CAN interface by parsing `ip -details link show`.
/// Returns None if the interface is down or the bitrate cannot be determined.
fn read_live_bitrate(iface: &str) -> Option<u32> {
    let out = Proc::new("ip")
        .args(["-details", "link", "show", iface])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    // Output contains a line like: "      bitrate 500000 sample-point 0.875"
    // Walk word-by-word and return the u32 after the first "bitrate" token.
    let mut iter = text.split_whitespace();
    while let Some(word) = iter.next() {
        if word == "bitrate" {
            if let Some(val) = iter.next() {
                if let Ok(n) = val.parse::<u32>() {
                    return Some(n);
                }
            }
        }
    }
    None
}

fn event_loop(
    stdout: &mut impl Write,
    buses: &mut Vec<Bus>,
    mode: &mut Mode,
    baseboard: &str,
) -> Result<(), Error> {
    loop {
        draw(stdout, buses, mode, baseboard)?;

        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                let current = std::mem::replace(mode, Mode::List { cursor: 0 });
                match handle_key(key.code, current, buses) {
                    Action::Update(next) => *mode = next,
                    Action::Quit => return Ok(()),
                }
            }
            Event::Resize(_, _) => {}
            _ => {}
        }
    }
}

fn handle_key(key: KeyCode, mode: Mode, buses: &mut Vec<Bus>) -> Action {
    match mode {
        Mode::List { mut cursor } => match key {
            KeyCode::Up => {
                if cursor > 0 {
                    cursor -= 1;
                }
                Action::Update(Mode::List { cursor })
            }
            KeyCode::Down => {
                if cursor + 1 < buses.len() {
                    cursor += 1;
                }
                Action::Update(Mode::List { cursor })
            }
            KeyCode::Enter => {
                let init = bitrate_idx(buses[cursor].bitrate);
                Action::Update(Mode::Bitrate { bus: cursor, cursor: init, error: None })
            }
            KeyCode::Char('q') | KeyCode::Esc => Action::Quit,
            _ => Action::Update(Mode::List { cursor }),
        },

        Mode::Bitrate { bus, mut cursor, .. } => match key {
            KeyCode::Up => {
                if cursor > 0 {
                    cursor -= 1;
                }
                Action::Update(Mode::Bitrate { bus, cursor, error: None })
            }
            KeyCode::Down => {
                if cursor + 1 < BITRATES.len() {
                    cursor += 1;
                }
                Action::Update(Mode::Bitrate { bus, cursor, error: None })
            }
            KeyCode::Enter => {
                let (bitrate, _) = BITRATES[cursor];
                let name = buses[bus].name.clone();
                match cli::handle_set(&name, "bitrate", &bitrate.to_string(), true) {
                    Ok(()) => {
                        buses[bus].bitrate = Some(bitrate);
                        Action::Update(Mode::List { cursor: bus })
                    }
                    Err(e) => Action::Update(Mode::Bitrate {
                        bus,
                        cursor,
                        error: Some(e.to_string()),
                    }),
                }
            }
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => {
                Action::Update(Mode::List { cursor: bus })
            }
            _ => Action::Update(Mode::Bitrate { bus, cursor, error: None }),
        },
    }
}

fn draw(
    stdout: &mut impl Write,
    buses: &[Bus],
    mode: &Mode,
    baseboard: &str,
) -> Result<(), Error> {
    queue!(stdout, cursor::MoveTo(0, 0), terminal::Clear(ClearType::All))?;

    queue!(
        stdout,
        SetForegroundColor(Color::Rgb { r: 255, g: 102, b: 0 }),
        Print(concat!("  GOcontroll Moduline CAN tool  v", env!("CARGO_PKG_VERSION"), "\r\n")),
        ResetColor,
        Print(format!("  Controller: Moduline {}\r\n\r\n", baseboard)),
    )?;

    match mode {
        Mode::List { cursor } => draw_list(stdout, buses, *cursor)?,
        Mode::Bitrate { bus, cursor, error } => {
            draw_bitrate(stdout, buses, *bus, *cursor, error.as_deref())?
        }
    }

    stdout.flush()?;
    Ok(())
}

fn draw_list(stdout: &mut impl Write, buses: &[Bus], cursor: usize) -> Result<(), Error> {
    for (i, bus) in buses.iter().enumerate() {
        if i == cursor {
            queue!(stdout, SetForegroundColor(Color::Cyan), Print("  \u{25ba} "), ResetColor)?;
        } else {
            queue!(stdout, Print("    "))?;
        }

        queue!(stdout, Print(format!("{:<6}  ", bus.name)))?;

        match bus.bitrate {
            Some(br) => queue!(
                stdout,
                SetForegroundColor(Color::Green),
                Print(format!("{:<14}", fmt_bitrate(br))),
                ResetColor,
            )?,
            None => queue!(
                stdout,
                SetForegroundColor(Color::DarkGrey),
                Print(format!("{:<14}", "unconfigured")),
                ResetColor,
            )?,
        }

        let status = if bus.up { "up" } else { "down" };
        queue!(stdout, Print(format!("  {}\r\n", status)))?;
    }

    queue!(
        stdout,
        Print("\r\n"),
        SetForegroundColor(Color::DarkGrey),
        Print("  \u{2191}/\u{2193} navigate   Enter select bitrate   q quit\r\n"),
        ResetColor,
    )?;

    Ok(())
}

fn draw_bitrate(
    stdout: &mut impl Write,
    buses: &[Bus],
    bus_idx: usize,
    cursor: usize,
    error: Option<&str>,
) -> Result<(), Error> {
    let bus = &buses[bus_idx];
    queue!(stdout, Print(format!("  {}  \u{2014}  Select Bitrate\r\n\r\n", bus.name)))?;

    for (i, (bitrate, label)) in BITRATES.iter().enumerate() {
        if i == cursor {
            queue!(stdout, SetForegroundColor(Color::Cyan), Print("  \u{25ba} "), ResetColor)?;
        } else {
            queue!(stdout, Print("    "))?;
        }

        if bus.bitrate == Some(*bitrate) {
            queue!(stdout, SetForegroundColor(Color::Green), Print(*label), ResetColor)?;
        } else {
            queue!(stdout, Print(*label))?;
        }

        queue!(stdout, Print("\r\n"))?;
    }

    queue!(
        stdout,
        Print("\r\n"),
        SetForegroundColor(Color::DarkGrey),
        Print("  \u{2191}/\u{2193} navigate   Enter apply   \u{2190}/Esc back\r\n"),
        ResetColor,
    )?;

    if let Some(err) = error {
        queue!(
            stdout,
            Print("\r\n"),
            SetForegroundColor(Color::Red),
            Print(format!("  Error: {}\r\n", err)),
            ResetColor,
        )?;
    }

    Ok(())
}

fn fmt_bitrate(br: u32) -> String {
    match br {
        125_000 => "125 kbps".into(),
        250_000 => "250 kbps".into(),
        500_000 => "500 kbps".into(),
        1_000_000 => "1000 kbps".into(),
        n => format!("{n} bps"),
    }
}

fn bitrate_idx(current: Option<u32>) -> usize {
    match current {
        Some(br) => BITRATES.iter().position(|(b, _)| *b == br).unwrap_or(0),
        None => 0,
    }
}
