//! The heart & knife dashboard in a terminal (ratatui): the live status, every action, the settings, start at boot,
//! the log. Same keys as the JavaScript control app:
//!   [s] start  [x] stop  [r] restart  [p] pause / resume  [u] update now  [l] log  [c] settings  [b] start at boot  [q] quit
//!   Shift+X / Shift+R: stop / restart at once (otherwise they wait until nothing is in flight).
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use std::time::{Duration, Instant};

use crate::control;
use crate::files::Folder;
use vr_core::settings::HostSettings;

enum Screen { Dash, Settings { sel: ListState, edit: Option<String>, note: String }, Boot { sel: usize }, Log { lines: Vec<String>, scroll: u16 } }

fn tone(t: &str) -> Style { match t { "ok" => Style::default().fg(Color::Green), "warn" => Style::default().fg(Color::Yellow), "bad" => Style::default().fg(Color::Red), _ => Style::default() } }

pub fn run(f: &Folder) -> i32 {
    let mut term = ratatui::init();
    let mut screen = Screen::Dash;
    let mut msg = if f.config.exists() { "Shift+X / Shift+R stop / restart at once (otherwise they wait until nothing is in flight).".to_string() } else { "No ai-node.json here yet: [c] settings explains, or run the VibeRoller AI Host package to set one up.".to_string() };
    let mut snap = control::snapshot(f);
    let mut last = Instant::now();
    loop {
        if last.elapsed() > Duration::from_secs(2) { snap = control::snapshot(f); last = Instant::now(); }
        let _ = term.draw(|fr| {
            let area = fr.area();
            let title = Line::from(vec![Span::styled(" ♥ ", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)), Span::styled("VibeRoller AI Host · heart & knife ", Style::default().add_modifier(Modifier::BOLD)), Span::styled(format!("{} · {} ", snap.version, f.dir.display()), Style::default().fg(Color::DarkGray))]);
            let chunks = Layout::default().direction(Direction::Vertical).constraints([Constraint::Min(5), Constraint::Length(3), Constraint::Length(3)]).split(area);
            match &mut screen {
                Screen::Dash => {
                    let rows: Vec<Line> = control::rows(&snap, f).into_iter().map(|(l, t, k)| Line::from(vec![Span::styled(format!("{l:<10} "), Style::default().fg(Color::Cyan)), Span::styled(t, tone(k))])).collect();
                    fr.render_widget(Paragraph::new(rows).wrap(Wrap { trim: false }).block(Block::default().borders(Borders::ALL).title(title)), chunks[0]);
                    let keys = format!("[s] start  [x] stop  [r] restart  [p] {}  [u] update now  [l] log  [c] settings  [b] start at boot  [q] quit", if snap.paused { "resume" } else { "pause" });
                    fr.render_widget(Paragraph::new(keys).block(Block::default().borders(Borders::ALL)), chunks[1]);
                }
                Screen::Settings { sel, edit, note } => {
                    let cfg = HostSettings::load(&f.config).ok();
                    let items: Vec<ListItem> = match &cfg { Some(c) => c.list().into_iter().map(|(_, v, l)| ListItem::new(Line::from(vec![Span::styled(format!("{l:<70} "), if l.starts_with("SECURITY") { Style::default().fg(Color::Yellow) } else { Style::default() }), Span::styled(v, Style::default().fg(Color::Cyan))]))).collect(), None => vec![ListItem::new("No ai-node.json here yet: download your own host's package (VibeRoller → Settings → My AI hosts), or copy ai-node.example.json to ai-node.json.")] };
                    fr.render_stateful_widget(List::new(items).highlight_style(Style::default().add_modifier(Modifier::REVERSED)).block(Block::default().borders(Borders::ALL).title(" Settings (written to ai-node.json; the host applies them at its next quiet moment) ")), chunks[0], sel);
                    let help = match edit { Some(t) => format!("New value: {t}█   (Enter = save, Esc = cancel; on / off for switches)"), None => "↑ ↓ choose · Enter change · L lock the config file down · Esc back".into() };
                    fr.render_widget(Paragraph::new(help).block(Block::default().borders(Borders::ALL)), chunks[1]);
                    if !note.is_empty() { msg = note.clone(); }
                }
                Screen::Boot { sel } => {
                    let now = control::boot_label(snap.boot);
                    let mut lines = vec![Line::from(format!("Now: {now}")), Line::from("")];
                    for (i, m) in control::boot_modes().iter().enumerate() { lines.push(Line::styled(format!("{} {}. {}", if i == *sel { "›" } else { " " }, i + 1, control::boot_label(m)), if i == *sel { Style::default().add_modifier(Modifier::REVERSED) } else { Style::default() })); }
                    fr.render_widget(Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" Start at boot ")), chunks[0]);
                    fr.render_widget(Paragraph::new("↑ ↓ choose · Enter set it · Esc back").block(Block::default().borders(Borders::ALL)), chunks[1]);
                }
                Screen::Log { lines, scroll } => {
                    fr.render_widget(Paragraph::new(lines.iter().map(|l| Line::from(l.as_str())).collect::<Vec<_>>()).scroll((*scroll, 0)).block(Block::default().borders(Borders::ALL).title(" Log (the last lines) ")), chunks[0]);
                    fr.render_widget(Paragraph::new("↑ ↓ scroll · Esc back").block(Block::default().borders(Borders::ALL)), chunks[1]);
                }
            }
            fr.render_widget(Paragraph::new(msg.as_str()).wrap(Wrap { trim: true }).block(Block::default().borders(Borders::ALL)), chunks[2]);
        });
        if !event::poll(Duration::from_millis(500)).unwrap_or(false) { continue; }
        let Ok(Event::Key(KeyEvent { code, modifiers, kind: KeyEventKind::Press, .. })) = event::read() else { continue };
        let shift = modifiers.contains(KeyModifiers::SHIFT);
        let mut refresh = true;
        match &mut screen {
            Screen::Dash => match code {
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => break,
                KeyCode::Char('s') => msg = control::start(f),
                KeyCode::Char('x') | KeyCode::Char('X') => msg = control::stop(f, shift || code == KeyCode::Char('X')),
                KeyCode::Char('r') | KeyCode::Char('R') => msg = control::restart(f, shift || code == KeyCode::Char('R')),
                KeyCode::Char('p') => msg = if f.off().exists() { control::resume(f) } else { control::pause(f) },
                KeyCode::Char('u') => msg = control::update_now(f),
                KeyCode::Char('l') => { let lines = control::log_tail(f, 400); let scroll = lines.len().saturating_sub(20) as u16; screen = Screen::Log { lines, scroll }; }
                KeyCode::Char('c') => { let mut sel = ListState::default(); sel.select(Some(0)); screen = Screen::Settings { sel, edit: None, note: String::new() }; }
                KeyCode::Char('b') => screen = Screen::Boot { sel: 0 },
                _ => refresh = false,
            },
            Screen::Settings { sel, edit, note } => {
                let cfg = HostSettings::load(&f.config).ok();
                let n = cfg.as_ref().map(|c| c.list().len()).unwrap_or(0);
                match edit {
                    Some(t) => match code {
                        KeyCode::Esc => *edit = None,
                        KeyCode::Backspace => { t.pop(); }
                        KeyCode::Char(ch) => t.push(ch),
                        KeyCode::Enter => {
                            let key = cfg.as_ref().and_then(|c| c.list().get(sel.selected().unwrap_or(0)).map(|x| x.0.clone())).unwrap_or_default();
                            let v = t.clone();
                            *note = match HostSettings::load(&f.config).and_then(|mut c| { c.set(&key, &v)?; c.save()?; Ok(()) }) { Ok(()) => format!("Saved {key}. {}", if f.host_pid().is_some() { "The host applies it at its next quiet moment." } else { "" }), Err(e) => e.to_string() };
                            *edit = None;
                        }
                        _ => {}
                    },
                    None => match code {
                        KeyCode::Esc | KeyCode::Char('q') => { screen = Screen::Dash; }
                        KeyCode::Down => { let i = sel.selected().unwrap_or(0); sel.select(Some(if n == 0 { 0 } else { (i + 1).min(n - 1) })); }
                        KeyCode::Up => { let i = sel.selected().unwrap_or(0); sel.select(Some(i.saturating_sub(1))); }
                        KeyCode::Char('l') | KeyCode::Char('L') => *note = control::lockdown(f),
                        KeyCode::Enter if n > 0 => {
                            let label = cfg.as_ref().and_then(|c| c.list().get(sel.selected().unwrap_or(0)).map(|x| x.2.clone())).unwrap_or_default();
                            *note = if label.starts_with("SECURITY") { "This is a security setting: it changes what VibeRoller may do on this machine.".into() } else { String::new() };
                            *edit = Some(String::new());
                        }
                        _ => {}
                    },
                }
            }
            Screen::Boot { sel } => match code {
                KeyCode::Esc | KeyCode::Char('q') => screen = Screen::Dash,
                KeyCode::Down => *sel = (*sel + 1).min(control::boot_modes().len() - 1),
                KeyCode::Up => *sel = sel.saturating_sub(1),
                KeyCode::Enter => {
                    let m = control::boot_modes()[*sel];
                    // sudo / the Windows prompt need the plain terminal for a moment
                    ratatui::restore();
                    let lines = control::set_boot(f, m);
                    term = ratatui::init();
                    msg = lines.join(" ");
                    screen = Screen::Dash;
                }
                _ => {}
            },
            Screen::Log { lines, scroll } => match code {
                KeyCode::Esc | KeyCode::Char('q') => screen = Screen::Dash,
                KeyCode::Down => *scroll = (*scroll + 1).min(lines.len() as u16),
                KeyCode::Up => *scroll = scroll.saturating_sub(1),
                KeyCode::PageDown => *scroll = (*scroll + 20).min(lines.len() as u16),
                KeyCode::PageUp => *scroll = scroll.saturating_sub(20),
                _ => {}
            },
        }
        if refresh { snap = control::snapshot(f); last = Instant::now(); }
    }
    ratatui::restore();
    0
}
