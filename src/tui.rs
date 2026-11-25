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
use crate::config::{self, DEFAULT_DATA_BITRATE};
use crate::error::Error;
use crate::netlink::{self, LinkCaps};
use crate::output;

/// Nominal bitrates: what the arbitration phase runs at, and a classic bus as
/// a whole.
const BITRATES: &[u32] = &[125_000, 250_000, 500_000, 1_000_000];

/// Data-phase bitrates of CAN FD. A bus is only offered the ones its
/// controller can make exactly and that are not below its nominal bitrate; see
/// [`data_bitrates`].
const DATA_BITRATES: &[u32] = &[
    500_000, 1_000_000, 2_000_000, 4_000_000, 5_000_000, 8_000_000,
];

const SEP: &str = "  ------------------------------------\r\n";

struct Bus {
    name: String,
    /// Bitrate from config file, or detected live from running interface.
    bitrate: Option<u32>,
    /// CAN FD and its data bitrate, from the config file.
    fd: bool,
    data_bitrate: Option<u32>,
    up: bool,
    /// What the controller behind the interface can do.
    caps: LinkCaps,
    /// A bus on a Multibus module: go-multibus decides CAN or CAN FD there.
    module: bool,
}

impl Bus {
    fn load(name: &str, up: bool) -> Bus {
        let cfg = config::load(name).ok();
        let caps = netlink::link_caps(name).unwrap_or_default();
        Bus {
            name: name.to_string(),
            bitrate: cfg
                .as_ref()
                .map(|c| c.bitrate)
                .or_else(|| read_live_bitrate(name)),
            // Without a config file the interface itself is the only answer.
            fd: cfg.as_ref().map(|c| c.fd).unwrap_or(caps.fd_active),
            data_bitrate: match &cfg {
                Some(c) => c.data_bitrate,
                None => caps.data_bitrate,
            },
            up,
            caps,
            module: output::is_module_can(name),
        }
    }

    /// Read the bus again after a change, so the screen shows what is stored.
    fn refresh(&mut self) {
        let up = is_up(&self.name).unwrap_or(self.up);
        *self = Bus::load(&self.name, up);
    }

    /// The rows of this bus' settings screen.
    fn rows(&self) -> Vec<Row> {
        let mut rows = vec![Row::Bitrate, Row::Fd];
        if self.fd {
            rows.push(Row::DataBitrate);
        }
        rows
    }
}

/// One line of a bus' settings screen.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Row {
    Bitrate,
    Fd,
    DataBitrate,
}

enum Mode {
    List { cursor: usize },
    /// The settings of one CAN FD capable bus.
    Bus { bus: usize, cursor: usize, error: Option<String> },
    Bitrate { bus: usize, cursor: usize, error: Option<String> },
    DataBitrate { bus: usize, cursor: usize, error: Option<String> },
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
        .map(|i| Bus::load(&i.name, i.up))
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
fn read_live_bitrate(iface: &str) -> Option<u32> {
    let out = Proc::new("ip")
        .args(["-details", "link", "show", iface])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
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

fn is_up(iface: &str) -> Option<bool> {
    let flags = std::fs::read_to_string(format!("/sys/class/net/{iface}/flags")).ok()?;
    let flags = flags.trim();
    let flags = flags.strip_prefix("0x").unwrap_or(flags);
    u32::from_str_radix(flags, 16).ok().map(|v| v & 0x1 != 0)
}

/// The data bitrates this bus can be given.
///
/// Two filters. The data phase is never slower than the arbitration phase, so
/// nothing below the nominal bitrate. And only what the controller's clock
/// divides into exactly: the kernel would accept a rate that is a few percent
/// off, and a CAN FD bus would then throw errors under load. What the bus is
/// set to now is always listed, so the screen never hides the value in force.
fn data_bitrates(bus: &Bus) -> Vec<u32> {
    let nominal = bus.bitrate.unwrap_or(0);
    let mut rates: Vec<u32> = DATA_BITRATES
        .iter()
        .copied()
        .filter(|rate| *rate >= nominal && bus.caps.data_bitrate_exact(*rate))
        .collect();
    if let Some(current) = bus.data_bitrate {
        if bus.fd && !rates.contains(&current) {
            rates.push(current);
            rates.sort_unstable();
        }
    }
    rates
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

/// Where a picker returns to: the bus' settings screen when it has one, the
/// list for a classic-only bus, which goes straight from the list to the
/// bitrate picker as it always did.
fn back_from_picker(buses: &[Bus], bus: usize, row: Row) -> Mode {
    if buses[bus].caps.fd {
        let cursor = buses[bus]
            .rows()
            .iter()
            .position(|candidate| *candidate == row)
            .unwrap_or(0);
        Mode::Bus { bus, cursor, error: None }
    } else {
        Mode::List { cursor: bus }
    }
}

fn handle_key(key: KeyCode, mode: Mode, buses: &mut [Bus]) -> Action {
    match mode {
        Mode::List { mut cursor } => match key {
            KeyCode::Up => {
                cursor = cursor.saturating_sub(1);
                Action::Update(Mode::List { cursor })
            }
            KeyCode::Down => {
                if cursor + 1 < buses.len() {
                    cursor += 1;
                }
                Action::Update(Mode::List { cursor })
            }
            KeyCode::Enter | KeyCode::Right => {
                if buses[cursor].caps.fd {
                    Action::Update(Mode::Bus { bus: cursor, cursor: 0, error: None })
                } else {
                    let init = index_of(BITRATES, buses[cursor].bitrate);
                    Action::Update(Mode::Bitrate { bus: cursor, cursor: init, error: None })
                }
            }
            KeyCode::Char('q') | KeyCode::Esc => Action::Quit,
            _ => Action::Update(Mode::List { cursor }),
        },

        Mode::Bus { bus, mut cursor, error } => {
            let rows = buses[bus].rows();
            match key {
                KeyCode::Up => {
                    cursor = cursor.saturating_sub(1);
                    Action::Update(Mode::Bus { bus, cursor, error: None })
                }
                KeyCode::Down => {
                    if cursor + 1 < rows.len() {
                        cursor += 1;
                    }
                    Action::Update(Mode::Bus { bus, cursor, error: None })
                }
                KeyCode::Enter | KeyCode::Right => match rows[cursor.min(rows.len() - 1)] {
                    Row::Bitrate => {
                        let init = index_of(BITRATES, buses[bus].bitrate);
                        Action::Update(Mode::Bitrate { bus, cursor: init, error: None })
                    }
                    Row::DataBitrate => {
                        let init = index_of(&data_bitrates(&buses[bus]), buses[bus].data_bitrate);
                        Action::Update(Mode::DataBitrate { bus, cursor: init, error: None })
                    }
                    // The row says where the choice is made; nothing to do here.
                    Row::Fd if buses[bus].module => {
                        Action::Update(Mode::Bus { bus, cursor, error: None })
                    }
                    Row::Fd => {
                        let value = if buses[bus].fd { "off" } else { "on" };
                        let name = buses[bus].name.clone();
                        let error = cli::handle_set(&name, "fd", value, true)
                            .err()
                            .map(|e| e.to_string());
                        buses[bus].refresh();
                        Action::Update(Mode::Bus { bus, cursor, error })
                    }
                },
                KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => {
                    Action::Update(Mode::List { cursor: bus })
                }
                _ => Action::Update(Mode::Bus { bus, cursor, error }),
            }
        }

        Mode::Bitrate { bus, mut cursor, .. } => match key {
            KeyCode::Up => {
                cursor = cursor.saturating_sub(1);
                Action::Update(Mode::Bitrate { bus, cursor, error: None })
            }
            KeyCode::Down => {
                if cursor + 1 < BITRATES.len() {
                    cursor += 1;
                }
                Action::Update(Mode::Bitrate { bus, cursor, error: None })
            }
            KeyCode::Enter => {
                let bitrate = BITRATES[cursor];
                match set_bitrate(&buses[bus], bitrate) {
                    Ok(()) => {
                        buses[bus].refresh();
                        Action::Update(back_from_picker(buses, bus, Row::Bitrate))
                    }
                    Err(e) => {
                        // What was stored before the failure is what counts.
                        buses[bus].refresh();
                        Action::Update(Mode::Bitrate { bus, cursor, error: Some(e.to_string()) })
                    }
                }
            }
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => {
                Action::Update(back_from_picker(buses, bus, Row::Bitrate))
            }
            _ => Action::Update(Mode::Bitrate { bus, cursor, error: None }),
        },

        Mode::DataBitrate { bus, mut cursor, .. } => {
            let rates = data_bitrates(&buses[bus]);
            match key {
                KeyCode::Up => {
                    cursor = cursor.saturating_sub(1);
                    Action::Update(Mode::DataBitrate { bus, cursor, error: None })
                }
                KeyCode::Down => {
                    if cursor + 1 < rates.len() {
                        cursor += 1;
                    }
                    Action::Update(Mode::DataBitrate { bus, cursor, error: None })
                }
                KeyCode::Enter if !rates.is_empty() => {
                    let rate = rates[cursor.min(rates.len() - 1)];
                    let name = buses[bus].name.clone();
                    let outcome = cli::handle_set(&name, "data-bitrate", &rate.to_string(), true);
                    buses[bus].refresh();
                    match outcome {
                        Ok(()) => Action::Update(back_from_picker(buses, bus, Row::DataBitrate)),
                        Err(e) => Action::Update(Mode::DataBitrate {
                            bus,
                            cursor,
                            error: Some(e.to_string()),
                        }),
                    }
                }
                KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => {
                    Action::Update(back_from_picker(buses, bus, Row::DataBitrate))
                }
                _ => Action::Update(Mode::DataBitrate { bus, cursor, error: None }),
            }
        }
    }
}

/// Store and apply a nominal bitrate.
///
/// On a CAN FD bus the data phase may not be slower than the arbitration
/// phase, and the kernel refuses the pair. Raising the nominal bitrate past
/// the data bitrate therefore takes the data bitrate along, rather than
/// leaving a bus that will not come up.
fn set_bitrate(bus: &Bus, bitrate: u32) -> Result<(), Error> {
    if bus.fd && bus.data_bitrate.unwrap_or(DEFAULT_DATA_BITRATE) < bitrate {
        let mut cfg = config::load_or_default(&bus.name);
        cfg.data_bitrate = Some(bitrate);
        config::save(&bus.name, &cfg)?;
    }
    cli::handle_set(&bus.name, "bitrate", &bitrate.to_string(), true)
}

fn draw(
    stdout: &mut impl Write,
    buses: &[Bus],
    mode: &Mode,
    baseboard: &str,
) -> Result<(), Error> {
    queue!(stdout, cursor::MoveTo(0, 0), terminal::Clear(ClearType::All))?;

    // Header
    queue!(
        stdout,
        SetForegroundColor(Color::Rgb { r: 255, g: 102, b: 0 }),
        Print(concat!("  GOcontroll Moduline CAN tool  v", env!("CARGO_PKG_VERSION"), "\r\n")),
        ResetColor,
    )?;
    queue!(stdout, Print(SEP))?;
    queue!(stdout, Print(format!("  Controller: Moduline {}\r\n", baseboard)))?;
    queue!(stdout, Print(SEP))?;

    // Content
    match mode {
        Mode::List { cursor } => draw_list(stdout, buses, *cursor)?,
        Mode::Bus { bus, cursor, error } => {
            draw_bus(stdout, &buses[*bus], *cursor, error.as_deref())?
        }
        Mode::Bitrate { bus, cursor, error } => draw_picker(
            stdout,
            &format!("{}  \u{2014}  Select Bitrate", buses[*bus].name),
            BITRATES,
            buses[*bus].bitrate,
            *cursor,
            None,
            error.as_deref(),
        )?,
        Mode::DataBitrate { bus, cursor, error } => {
            let bus = &buses[*bus];
            let note = match (bus.caps.clock, bus.caps.data_const.is_some()) {
                (Some(clock), true) => Some(format!(
                    "Listed: at least the bitrate, and exact for this controller ({} MHz clock).",
                    clock / 1_000_000
                )),
                _ => None,
            };
            draw_picker(
                stdout,
                &format!("{}  \u{2014}  Select Data Bitrate (CAN FD)", bus.name),
                &data_bitrates(bus),
                bus.data_bitrate,
                *cursor,
                note.as_deref(),
                error.as_deref(),
            )?
        }
    }

    stdout.flush()?;
    Ok(())
}

fn caret(stdout: &mut impl Write, selected: bool) -> Result<(), Error> {
    if selected {
        queue!(stdout, SetForegroundColor(Color::Cyan), Print("  \u{25ba} "))?;
    } else {
        queue!(stdout, ResetColor, Print("    "))?;
    }
    Ok(())
}

/// A value: plain on the selected row, green when it is in force, grey when
/// there is nothing to show.
fn value(stdout: &mut impl Write, text: &str, selected: bool, set: bool) -> Result<(), Error> {
    if selected {
        queue!(stdout, Print(text))?;
    } else if set {
        queue!(stdout, SetForegroundColor(Color::Green), Print(text), ResetColor)?;
    } else {
        queue!(stdout, SetForegroundColor(Color::DarkGrey), Print(text), ResetColor)?;
    }
    Ok(())
}

fn hint(stdout: &mut impl Write, text: &str) -> Result<(), Error> {
    queue!(
        stdout,
        Print(SEP),
        SetForegroundColor(Color::DarkGrey),
        Print(format!("  {text}\r\n")),
        ResetColor,
    )?;
    Ok(())
}

fn draw_error(stdout: &mut impl Write, error: Option<&str>) -> Result<(), Error> {
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

fn draw_list(stdout: &mut impl Write, buses: &[Bus], cursor: usize) -> Result<(), Error> {
    for (i, bus) in buses.iter().enumerate() {
        let selected = i == cursor;
        caret(stdout, selected)?;

        queue!(stdout, Print(format!("{:<10}", bus.name)))?;

        let br = bus.bitrate.map(fmt_bitrate).unwrap_or_else(|| "unconfigured".into());
        value(stdout, &format!("{br:<14}"), selected, bus.bitrate.is_some())?;

        // The data bitrate of a CAN FD bus; empty for a classic one, so a
        // controller without CAN FD looks as it always did.
        let fd = if bus.fd {
            format!("FD {}", fmt_bitrate(bus.data_bitrate.unwrap_or(DEFAULT_DATA_BITRATE)))
        } else {
            String::new()
        };
        value(stdout, &format!("{fd:<16}"), selected, true)?;

        let status = if bus.up { "up" } else { "down" };
        queue!(stdout, ResetColor, Print(format!("{}\r\n", status)))?;
    }

    hint(stdout, "\u{2191}/\u{2193} navigate   Enter select   q quit")?;
    Ok(())
}

fn draw_bus(
    stdout: &mut impl Write,
    bus: &Bus,
    cursor: usize,
    error: Option<&str>,
) -> Result<(), Error> {
    queue!(stdout, Print(format!("  {}  \u{2014}  Settings\r\n\r\n", bus.name)))?;

    for (i, row) in bus.rows().iter().enumerate() {
        let selected = i == cursor;
        caret(stdout, selected)?;
        match row {
            Row::Bitrate => {
                queue!(stdout, Print(format!("{:<16}", "Bitrate")))?;
                let br = bus.bitrate.map(fmt_bitrate).unwrap_or_else(|| "unconfigured".into());
                value(stdout, &br, selected, bus.bitrate.is_some())?;
            }
            Row::Fd => {
                queue!(stdout, Print(format!("{:<16}", "CAN FD")))?;
                value(stdout, &format!("{:<12}", if bus.fd { "on" } else { "off" }), selected, bus.fd)?;
                if bus.module {
                    queue!(
                        stdout,
                        SetForegroundColor(Color::DarkGrey),
                        Print("chosen in go-multibus"),
                        ResetColor,
                    )?;
                }
            }
            Row::DataBitrate => {
                queue!(stdout, Print(format!("{:<16}", "Data bitrate")))?;
                let rate = bus.data_bitrate.unwrap_or(DEFAULT_DATA_BITRATE);
                value(stdout, &fmt_bitrate(rate), selected, true)?;
            }
        }
        queue!(stdout, ResetColor, Print("\r\n"))?;
    }

    hint(stdout, "\u{2191}/\u{2193} navigate   Enter select   \u{2190}/Esc back")?;

    if bus.module {
        queue!(
            stdout,
            Print("\r\n"),
            SetForegroundColor(Color::DarkGrey),
            Print(
                "  A Multibus bus: CAN or CAN FD is the interface mode, set with\r\n  \
                 go-multibus. The bitrates are set here.\r\n"
            ),
            ResetColor,
        )?;
    }

    draw_error(stdout, error)
}

fn draw_picker(
    stdout: &mut impl Write,
    title: &str,
    rates: &[u32],
    current: Option<u32>,
    cursor: usize,
    note: Option<&str>,
    error: Option<&str>,
) -> Result<(), Error> {
    queue!(stdout, Print(format!("  {title}\r\n\r\n")))?;

    for (i, rate) in rates.iter().enumerate() {
        let selected = i == cursor;
        caret(stdout, selected)?;
        let label = fmt_bitrate(*rate);
        if !selected && current == Some(*rate) {
            queue!(stdout, SetForegroundColor(Color::Green), Print(label), ResetColor)?;
        } else {
            queue!(stdout, Print(label))?;
        }
        queue!(stdout, ResetColor, Print("\r\n"))?;
    }

    hint(stdout, "\u{2191}/\u{2193} navigate   Enter apply   \u{2190}/Esc back")?;

    if let Some(note) = note {
        queue!(
            stdout,
            Print("\r\n"),
            SetForegroundColor(Color::DarkGrey),
            Print(format!("  {note}\r\n")),
            ResetColor,
        )?;
    }

    draw_error(stdout, error)
}

fn fmt_bitrate(br: u32) -> String {
    if br % 1000 == 0 {
        format!("{} kbps", br / 1000)
    } else {
        format!("{br} bps")
    }
}

fn index_of(rates: &[u32], current: Option<u32>) -> usize {
    current
        .and_then(|rate| rates.iter().position(|candidate| *candidate == rate))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::netlink::BitTimingConst;

    fn module_bus(bitrate: u32, data_bitrate: u32) -> Bus {
        Bus {
            name: "mb_can1".into(),
            bitrate: Some(bitrate),
            fd: true,
            data_bitrate: Some(data_bitrate),
            up: true,
            caps: LinkCaps {
                fd: true,
                fd_active: true,
                data_bitrate: Some(data_bitrate),
                clock: Some(170_000_000),
                data_const: Some(BitTimingConst {
                    tseg1_min: 1, tseg1_max: 32, tseg2_min: 1, tseg2_max: 16,
                    brp_min: 1, brp_max: 32, brp_inc: 1,
                }),
            },
            module: true,
        }
    }

    #[test]
    fn a_module_bus_is_offered_what_its_clock_makes_and_not_below_the_bitrate() {
        assert_eq!(
            data_bitrates(&module_bus(500_000, 2_000_000)),
            vec![500_000, 1_000_000, 2_000_000, 5_000_000]
        );
        assert_eq!(
            data_bitrates(&module_bus(1_000_000, 2_000_000)),
            vec![1_000_000, 2_000_000, 5_000_000]
        );
    }

    #[test]
    fn the_data_bitrate_in_force_is_listed_even_when_it_is_not_exact() {
        assert_eq!(
            data_bitrates(&module_bus(500_000, 4_000_000)),
            vec![500_000, 1_000_000, 2_000_000, 4_000_000, 5_000_000]
        );
    }

    #[test]
    fn the_data_bitrate_row_only_shows_on_a_can_fd_bus() {
        let mut bus = module_bus(500_000, 2_000_000);
        assert!(bus.rows().contains(&Row::DataBitrate));
        bus.fd = false;
        assert!(!bus.rows().contains(&Row::DataBitrate));
    }

    #[test]
    fn bitrates_read_in_kbps() {
        assert_eq!(fmt_bitrate(125_000), "125 kbps");
        assert_eq!(fmt_bitrate(2_000_000), "2000 kbps");
        assert_eq!(fmt_bitrate(83_333), "83333 bps");
    }
}
