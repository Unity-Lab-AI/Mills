//! The control app in a terminal (ratatui + crossterm, so Windows terminals too): everything in one place, no files to edit.
//!   first run (no ai-node.json): the setup wizard; then the dashboard:
//!   [s] start [x] stop [r] restart [p] pause/resume [u] update now [a] providers [c] settings [d] doctor
//!   [b] start at boot [i] install / update this copy [l] log [?] help [q] quit   (Shift+X / Shift+R: at once)
//! Anything that may prompt (sudo, the Windows administrator prompt) runs with the terminal given back first.
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::DefaultTerminal;
use std::time::{Duration, Instant};

use crate::control;
use crate::files::Folder;
use crate::install::{self, Copies};
use vr_core::settings::HostSettings;

const DEFAULT_HUB: &str = "https://portal.unityailab.com";

struct Field { label: String, value: String, secret: bool }
#[derive(Clone)]
enum Kind { Setup, AddEndpoint, Setting(String) }
struct Form { title: String, intro: Vec<String>, fields: Vec<Field>, cur: usize, kind: Kind }
impl Form {
    fn one(title: &str, intro: &str, label: &str, value: &str, secret: bool, kind: Kind) -> Form { Form { title: title.into(), intro: vec![intro.into()], fields: vec![Field { label: label.into(), value: value.into(), secret }], cur: 0, kind } }
    fn setup(paste: bool) -> Form {
        let intro = vec!["Where the values come from: VibeRoller → Settings → My AI hosts → make a host, then \"show config\" (or download its package: it holds an ai-node.json).".to_string(),
            if paste { "Paste the ai-node.json text here, or type the path to a downloaded ai-node.json. (F2: type the three values instead)".to_string() } else { "Type the three values. (F2: paste the ai-node.json text or give its path instead)".to_string() }];
        let fields = if paste { vec![Field { label: "ai-node.json text or path".into(), value: String::new(), secret: false }] } else { vec![
            Field { label: "Hub address".into(), value: DEFAULT_HUB.into(), secret: false }, Field { label: "Host id".into(), value: String::new(), secret: false }, Field { label: "Token (hidden)".into(), value: String::new(), secret: true }] };
        Form { title: " Set up this AI host ".into(), intro, fields, cur: 0, kind: Kind::Setup }
    }
    fn add_endpoint() -> Form {
        let f = |l: &str, s: bool| Field { label: l.into(), value: String::new(), secret: s };
        Form { title: " Add an OpenAI-compatible endpoint (LM Studio, vLLM, a cloud API …) ".into(), intro: vec!["The id is a short name (letters, digits, dashes). The base URL ends in /v1. The API key is optional.".into()],
            fields: vec![f("Id", false), f("Label", false), f("Base URL (e.g. http://127.0.0.1:1234/v1)", false), f("API key (optional, hidden)", true), f("Models (comma list)", false)], cur: 0, kind: Kind::AddEndpoint }
    }
}
/// None = keep editing, Some(false) = cancelled, Some(true) = submit.
fn form_key(form: &mut Form, code: KeyCode) -> Option<bool> {
    let n = form.fields.len();
    match code {
        KeyCode::Esc => return Some(false),
        KeyCode::Tab | KeyCode::Down => form.cur = (form.cur + 1).min(n - 1),
        KeyCode::BackTab | KeyCode::Up => form.cur = form.cur.saturating_sub(1),
        KeyCode::Backspace => { form.fields[form.cur].value.pop(); }
        KeyCode::Char(c) => form.fields[form.cur].value.push(c),
        KeyCode::Enter => { if form.cur + 1 < n { form.cur += 1; } else { return Some(true); } }
        _ => {}
    }
    None
}

enum Screen {
    Setup { form: Form, paste: bool },
    SetupDone,
    Dash,
    Providers { sel: usize, confirm: Option<String>, form: Option<Form> },
    Settings { sel: usize, form: Option<Form> },
    Doctor { sel: usize, checks: Vec<control::Check> },
    Boot { sel: usize },
    Log { lines: Vec<String>, scroll: usize },
    Help,
}

struct App { screen: Screen, snap: control::Snapshot, copies: Copies, msg: String, ollama: Option<(String, Vec<String>)>, watch: Option<u64>, view_h: usize }

fn tone(t: &str) -> Style { match t { "ok" => Style::default().fg(Color::Green), "warn" => Style::default().fg(Color::Yellow), "bad" => Style::default().fg(Color::Red), _ => Style::default() } }
fn sel_style() -> Style { Style::default().add_modifier(Modifier::REVERSED) }
fn block(title: &str) -> Block<'_> { Block::default().borders(Borders::ALL).title(title.to_string()) }

/// Gives the terminal back for something that may prompt, then takes it again.
fn suspended<T>(term: &mut DefaultTerminal, work: impl FnOnce() -> T) -> T { ratatui::restore(); let r = work(); *term = ratatui::init(); r }

fn applied(f: &Folder) -> &'static str { if f.host_pid().is_some() { " The host applies it at its next quiet moment." } else { "" } }
/// Loads the settings, lets `edit` change them, saves; the message to show.
fn change(f: &Folder, what: &str, edit: impl FnOnce(&mut HostSettings) -> pgm_core_internal::Result<()>) -> String {
    match HostSettings::load(&f.config).and_then(|mut c| { edit(&mut c)?; c.save()?; Ok(()) }) { Ok(()) => format!("Saved {what}.{}", applied(f)), Err(e) => e.to_string() }
}
fn truthy(v: &str) -> bool { matches!(v, "on" | "ON" | "yes" | "true") }

// ---- the providers list: one flat list, a header per provider followed by its fields ----
enum PRow { Header(String), Field { id: String, key: String, switch: bool }, Detected }
fn provider_rows(cfg: &HostSettings, ollama: &Option<(String, Vec<String>)>) -> Vec<(PRow, String, Style, String)> {
    let mut v = vec![];
    for p in cfg.all_providers() {
        let login = if p.enabled && p.is_relay() { Some(pgm_ai_internal::host::relay::login_problem(&p)) } else { None };
        let mark = match &login { Some(None) => "  ✓ login found".to_string(), Some(Some(pr)) => format!("  ✗ {}", control::short(pr)), None => String::new() };
        let style = match &login { Some(Some(_)) => Style::default().fg(Color::Yellow), _ => Style::default().add_modifier(Modifier::BOLD) };
        let name = if p.label.is_empty() { p.id.clone() } else { p.label.clone() };
        let detail = format!("{} · kind {}{} · models: {}{}", p.id, p.kind, if p.base_url.is_empty() { String::new() } else { format!(" · {}", p.base_url) }, if p.models.is_empty() { "(any)".to_string() } else { p.models.join(", ") },
            match login { Some(Some(pr)) => format!("\nLogin: {pr}"), _ => String::new() });
        v.push((PRow::Header(p.id.clone()), format!("▸ {name} [{}]{}{mark}", p.kind, if p.enabled { "" } else { "  (off)" }), style, detail));
        let key = |k: &str| format!("provider.{}.{k}", p.id);
        let mut field = |k: &str, text: String, switch: bool| { v.push((PRow::Field { id: p.id.clone(), key: key(k), switch }, text, Style::default(), String::new())); };
        field("enabled", format!("    [{}] offered to the hub", if p.enabled { "x" } else { " " }), true);
        field("shareWithGroup", format!("    [{}] shared with the team group", if p.share_with_group { "x" } else { " " }), true);
        field("allowedUsers", format!("    allowed users: {}", if p.allowed_users.is_empty() { "(none)".into() } else { p.allowed_users.join(", ") }), false);
        field("label", format!("    label: {}", p.label), false);
        field("models", format!("    models: {}", p.models.join(", ")), false);
    }
    let have = cfg.all_providers().iter().any(|p| p.id == "ollama" || (p.kind == "openai" && p.base_url.contains(":11434")));
    if let Some((url, models)) = ollama {
        if !have { v.push((PRow::Detected, format!("▸ Ollama at {url}: {} model(s)  — found on this machine   [o] adopt it so it can be shared", models.len()), Style::default().fg(Color::Cyan),
            format!("Ollama answers here. The running host already offers it privately; adopting writes it into ai-node.json as the provider \"ollama\" so you can share it with the group or users.\nModels: {}", models.join(", ")))); }
    }
    v
}

fn field_explain(key: &str) -> String {
    let e = HostSettings::explain(key);
    if !e.is_empty() { return e.to_string(); }
    if key.ends_with(".label") { "The name shown for this provider.".into() } else if key.ends_with(".models") { "The models this provider offers (comma list). Empty = whatever it serves.".into() } else { String::new() }
}

pub fn run(f: &Folder) -> i32 {
    let mut term = ratatui::init();
    let snap = control::snapshot(f);
    let first = !f.config.exists();
    let mut app = App { screen: if first { Screen::Setup { form: Form::setup(false), paste: false } } else { Screen::Dash }, snap, copies: install::copies(), ollama: None, watch: None, view_h: 20,
        msg: if first { "Welcome. Fill in the three values (or F2 to paste your ai-node.json); nothing is written until you press Enter on the last one.".into() } else { "Press ? for every key. Shift+X / Shift+R stop / restart at once.".into() } };
    let (mut last, mut last_copies) = (Instant::now(), Instant::now());
    loop {
        if last.elapsed() > Duration::from_secs(2) {
            app.snap = control::snapshot(f); last = Instant::now();
            if let Some(prior) = app.watch { let at = app.snap.st["lastUpdateAt"].as_u64().unwrap_or(0); if at != prior { app.msg = format!("Update: {}", app.snap.st["lastUpdate"].as_str().unwrap_or("")); app.watch = None; } }
        }
        if last_copies.elapsed() > Duration::from_secs(15) { app.copies = install::copies(); last_copies = Instant::now(); }
        let _ = term.draw(|fr| draw(fr, f, &mut app));
        if !event::poll(Duration::from_millis(500)).unwrap_or(false) { continue; }
        let Ok(Event::Key(KeyEvent { code, modifiers, kind: KeyEventKind::Press, .. })) = event::read() else { continue };
        if code == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL) { break; }
        if key(&mut term, f, &mut app, code, modifiers) { break; }
        app.snap = control::snapshot(f); last = Instant::now();
    }
    ratatui::restore();
    0
}

fn draw(fr: &mut ratatui::Frame, f: &Folder, app: &mut App) {
    let chunks = Layout::default().direction(Direction::Vertical).constraints([Constraint::Min(5), Constraint::Length(4), Constraint::Length(3), Constraint::Length(3)]).split(fr.area());
    app.view_h = (chunks[0].height as usize).saturating_sub(2);
    let title = Line::from(vec![Span::styled(" ♥ ", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)), Span::styled("VibeRoller AI Host · heart & knife ", Style::default().add_modifier(Modifier::BOLD)), Span::styled(format!("{} · {} ", app.snap.version, f.dir.display()), Style::default().fg(Color::DarkGray))]);
    let cfg = HostSettings::load(&f.config).ok();
    let mut detail = String::new();
    let keys: String;
    match &app.screen {
        Screen::Setup { form, .. } => { draw_form(fr, chunks[0], form); keys = "Tab / Enter next field · Enter on the last one saves · F2 paste instead · Esc quit".into(); }
        Screen::SetupDone => {
            fr.render_widget(Paragraph::new(vec![Line::from("Saved ai-node.json (readable by you only)."), Line::from(""), Line::from("[Enter] install this program (PATH, updates, start at boot) and start the host"), Line::from("[s]     just start the host from here"), Line::from("[Esc]   later")]).block(block(" Set up ")), chunks[0]);
            keys = "Enter install + start at boot · s just start · Esc later".into();
        }
        Screen::Dash => {
            let mut rows: Vec<Line> = vec![];
            if !app.copies.this_installed {
                rows.push(Line::styled(match &app.copies.installed {
                    Some(i) => format!("This is a download copy (version {}). The installed host in {} {} version {}. [i] install / update it from this copy", crate::runner::VERSION, i.dir.display(), if app.copies.installed_running { "runs" } else { "has" }, app.copies.installed_version),
                    None => format!("This program is not installed (version {}). [i] install it: vr-host on your PATH, updates, start at boot", crate::runner::VERSION) }, Style::default().fg(Color::Black).bg(Color::Yellow)));
                rows.push(Line::from(""));
            }
            rows.extend(control::rows(&app.snap, f).into_iter().map(|(l, t, k)| Line::from(vec![Span::styled(format!("{l:<10} "), Style::default().fg(Color::Cyan)), Span::styled(t, tone(k))])));
            fr.render_widget(Paragraph::new(rows).wrap(Wrap { trim: false }).block(Block::default().borders(Borders::ALL).title(title)), chunks[0]);
            keys = format!("[s]tart [x]stop [r]estart [p]{} [u]pdate [a]providers [c]settings [d]octor [b]oot {}[l]og [?] [q]uit", if app.snap.paused { "resume" } else { "ause" }, if app.copies.this_installed { "" } else { "[i]nstall " });
        }
        Screen::Providers { sel, confirm, form } => {
            let rows = cfg.as_ref().map(|c| provider_rows(c, &app.ollama)).unwrap_or_default();
            if let Some(form) = form { draw_form(fr, chunks[0], form); keys = "Tab / Enter next · Enter on the last saves · Esc cancel".into(); }
            else {
                let items: Vec<Line> = rows.iter().enumerate().map(|(i, (_, t, st, _))| Line::styled(t.clone(), if i == *sel { sel_style() } else { *st })).collect();
                let items = if items.is_empty() { vec![Line::from("No providers yet. [n] adds an OpenAI-compatible endpoint; a login (Claude, Codex) is added in ai-node.json.")] } else { items };
                fr.render_widget(Paragraph::new(items).scroll((sel.saturating_sub(app.view_h.saturating_sub(1)) as u16, 0)).block(block(" Providers (what this host lends) ")), chunks[0]);
                if let Some((row, _, _, d)) = rows.get(*sel) {
                    detail = match row { PRow::Field { key, .. } => field_explain(key), _ => d.clone() };
                    if let (Some(id), PRow::Header(h)) = (confirm, row) { if id == h { detail = format!("Press x again to remove \"{h}\" from ai-node.json."); } }
                }
                keys = "↑ ↓ choose · Space/Enter switch or edit · n add endpoint · o adopt Ollama · x remove (twice) · Esc back".into();
            }
        }
        Screen::Settings { sel, form } => {
            if let Some(form) = form { draw_form(fr, chunks[0], form); keys = "Enter save · Esc cancel".into(); }
            else {
                let list = cfg.as_ref().map(|c| c.list());
                let lines: Vec<Line> = match &list { Some(l) => l.iter().enumerate().map(|(i, (k, v, lab))| {
                    let shown = if HostSettings::is_switch(k) { format!("[{}] {v}", if truthy(v) { "x" } else { " " }) } else { v.clone() };
                    let base = if lab.starts_with("SECURITY") { Style::default().fg(Color::Yellow) } else { Style::default() };
                    if i == *sel { Line::styled(format!("{lab:<72} {shown}"), sel_style()) } else { Line::from(vec![Span::styled(format!("{lab:<72} "), base), Span::styled(shown, Style::default().fg(Color::Cyan))]) } }).collect(),
                    None => vec![Line::from("No ai-node.json here yet.")] };
                fr.render_widget(Paragraph::new(lines).scroll((sel.saturating_sub(app.view_h.saturating_sub(1)) as u16, 0)).block(block(" Settings (written to ai-node.json) ")), chunks[0]);
                if let Some(l) = &list { if let Some((k, _, _)) = l.get(*sel) { detail = HostSettings::explain(k).to_string(); } }
                keys = "↑ ↓ choose · Space/Enter switch or edit · L lock the config file down · Esc back".into();
            }
        }
        Screen::Doctor { sel, checks } => {
            let lines: Vec<Line> = checks.iter().enumerate().map(|(i, c)| {
                let colour = match c.ok { Some(true) => Color::Green, Some(false) => Color::Red, None => Color::Cyan };
                let l = Line::from(vec![Span::styled(c.tag(), Style::default().fg(colour).add_modifier(Modifier::BOLD)), Span::raw(" "), Span::raw(c.text.clone()), Span::styled(c.fix.map(|_| "   ← Enter fixes this").unwrap_or(""), Style::default().fg(Color::Yellow))]);
                if i == *sel { l.style(sel_style()) } else { l } }).collect();
            fr.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).block(block(" Doctor ")), chunks[0]);
            keys = "↑ ↓ choose · Enter fix the selected FIX row · R run the checks again · Esc back".into();
        }
        Screen::Boot { sel } => {
            let mut lines = vec![Line::from(format!("Now: {}", control::boot_label(app.snap.boot))), Line::from("")];
            for (i, m) in control::boot_modes().iter().enumerate() { lines.push(Line::styled(format!("{} {}. {}", if i == *sel { "›" } else { " " }, i + 1, control::boot_label(m)), if i == *sel { sel_style() } else { Style::default() })); }
            fr.render_widget(Paragraph::new(lines).block(block(" Start at boot ")), chunks[0]);
            keys = "↑ ↓ choose · Enter set it (may ask for sudo / administrator) · Esc back".into();
        }
        Screen::Log { lines, scroll } => {
            fr.render_widget(Paragraph::new(lines.iter().map(|l| Line::from(l.as_str())).collect::<Vec<_>>()).scroll((*scroll as u16, 0)).block(block(" Log (the last lines) ")), chunks[0]);
            keys = "↑ ↓ PgUp PgDn scroll · End newest line · Home oldest · Esc back".into();
        }
        Screen::Help => {
            let h = ["Dashboard   s start · x stop · r restart (Shift: at once) · p pause / resume · u update now · a providers · c settings · d doctor",
                "            b start at boot · i install / update this copy (download copies) · l log · ? help · q quit",
                "Setup       Tab / Enter next field · F2 paste ai-node.json or its path · Esc quit",
                "Providers   ↑ ↓ choose · Space / Enter switch or edit · n add endpoint · o adopt the found Ollama · x remove (press twice) · Esc",
                "Settings    ↑ ↓ choose · Space / Enter switch or edit · L lock the config down · Esc",
                "Doctor      ↑ ↓ choose · Enter fixes a FIX row · R checks again · Esc",
                "Boot        ↑ ↓ choose · Enter set · Esc",
                "Log         ↑ ↓ PgUp PgDn scroll · End newest · Home oldest · Esc",
                "Anywhere    Ctrl+C quits"];
            fr.render_widget(Paragraph::new(h.iter().map(|l| Line::from(*l)).collect::<Vec<_>>()).block(block(" Keys ")), chunks[0]);
            keys = "any key back".into();
        }
    }
    fr.render_widget(Paragraph::new(detail).wrap(Wrap { trim: true }).block(Block::default().borders(Borders::ALL)), chunks[1]);
    fr.render_widget(Paragraph::new(keys).wrap(Wrap { trim: true }).block(Block::default().borders(Borders::ALL)), chunks[2]);
    fr.render_widget(Paragraph::new(app.msg.as_str()).wrap(Wrap { trim: true }).block(Block::default().borders(Borders::ALL)), chunks[3]);
}

fn draw_form(fr: &mut ratatui::Frame, area: ratatui::layout::Rect, form: &Form) {
    let mut lines: Vec<Line> = form.intro.iter().map(|t| Line::styled(t.clone(), Style::default().fg(Color::DarkGray))).collect();
    lines.push(Line::from(""));
    for (i, fld) in form.fields.iter().enumerate() {
        let shown = if fld.secret { "•".repeat(fld.value.chars().count()) } else { fld.value.clone() };
        lines.push(Line::from(vec![Span::styled(format!("{:<44} ", fld.label), Style::default().fg(Color::Cyan)), Span::styled(format!("{shown}{}", if i == form.cur { "█" } else { "" }), if i == form.cur { Style::default().add_modifier(Modifier::BOLD) } else { Style::default() })]));
    }
    fr.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).block(block(&form.title)), area);
}

fn open_log(app: &mut App, f: &Folder) { let lines = control::log_tail(f, 400); let scroll = lines.len().saturating_sub(app.view_h); app.screen = Screen::Log { lines, scroll }; }

/// One key press. True = quit.
fn key(term: &mut DefaultTerminal, f: &Folder, app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> bool {
    let shift = modifiers.contains(KeyModifiers::SHIFT);
    let screen = std::mem::replace(&mut app.screen, Screen::Dash);
    match screen {
        Screen::Setup { mut form, paste } => {
            if code == KeyCode::F(2) { app.screen = Screen::Setup { form: Form::setup(!paste), paste: !paste }; return false; }
            match form_key(&mut form, code) {
                Some(false) => return true,
                Some(true) => match submit_setup(f, &form, paste) {
                    Ok(()) => { app.msg = "Saved.".into(); app.screen = Screen::SetupDone; }
                    // a pasted JSON text can contain line breaks: Enter inside an unfinished paste is a space, not "save"
                    Err(e) => { if paste && form.fields[0].value.trim_start().starts_with('{') && serde_json::from_str::<serde_json::Value>(form.fields[0].value.trim()).is_err() && HostSettings::from_text(&f.config, form.fields[0].value.trim()).is_err() { form.fields[0].value.push(' '); app.msg = "Keep pasting; Enter again when the whole text is in.".into(); } else { app.msg = e; } app.screen = Screen::Setup { form, paste }; }
                },
                None => app.screen = Screen::Setup { form, paste },
            }
        }
        Screen::SetupDone => match code {
            KeyCode::Enter => { let mut lines = vec![]; suspended(term, || { install::install_to(&["install"], &mut |l| { println!("{l}"); lines.push(l); }); std::thread::sleep(Duration::from_millis(800)); }); app.msg = lines.join(" "); app.copies = install::copies(); }
            KeyCode::Char('s') => app.msg = control::start(f),
            KeyCode::Esc => app.msg = "Later: [s] starts it, [i] installs it.".into(),
            _ => app.screen = Screen::SetupDone,
        },
        Screen::Dash => match code {
            KeyCode::Char('q') | KeyCode::Esc => return true,
            KeyCode::Char('s') => app.msg = control::start(f),
            KeyCode::Char('x') | KeyCode::Char('X') => app.msg = control::stop(f, shift || code == KeyCode::Char('X')),
            KeyCode::Char('r') | KeyCode::Char('R') => app.msg = control::restart(f, shift || code == KeyCode::Char('R')),
            KeyCode::Char('p') => app.msg = if f.off().exists() { control::resume(f) } else { control::pause(f) },
            KeyCode::Char('u') => { app.watch = Some(app.snap.st["lastUpdateAt"].as_u64().unwrap_or(0)); app.msg = format!("{} Watching for the result …", control::update_now(f)); }
            KeyCode::Char('l') => open_log(app, f),
            KeyCode::Char('c') => app.screen = Screen::Settings { sel: 0, form: None },
            KeyCode::Char('a') => { app.ollama = control::probe_ollama(); app.screen = Screen::Providers { sel: 0, confirm: None, form: None }; }
            KeyCode::Char('d') => { app.msg = "Checking …".into(); app.screen = Screen::Doctor { sel: 0, checks: control::doctor(f) }; app.msg = "Enter on a FIX row repairs it.".into(); }
            KeyCode::Char('b') => app.screen = Screen::Boot { sel: 0 },
            KeyCode::Char('i') if !app.copies.this_installed => {
                let mut lines = vec![];
                suspended(term, || { install::install_to(&["install"], &mut |l| { println!("{l}"); lines.push(l); }); std::thread::sleep(Duration::from_millis(800)); });
                app.msg = format!("{} From now on run `vr-host` (it is on your PATH).", lines.join(" ")); app.copies = install::copies();
            }
            KeyCode::Char('?') => app.screen = Screen::Help,
            _ => {}
        },
        Screen::Help => {}
        Screen::Settings { mut sel, mut form } => {
            let cfg = HostSettings::load(&f.config).ok();
            let list = cfg.as_ref().map(|c| c.list()).unwrap_or_default();
            if let Some(mut fm) = form.take() {
                match form_key(&mut fm, code) {
                    Some(true) => { if let Kind::Setting(k) = fm.kind.clone() { let v = fm.fields[0].value.clone(); app.msg = change(f, &k, |c| c.set(&k, &v)); } }
                    Some(false) => app.msg = "Cancelled.".into(),
                    None => form = Some(fm),
                }
            } else {
                match code {
                    KeyCode::Esc | KeyCode::Char('q') => { return false; }
                    KeyCode::Down => sel = (sel + 1).min(list.len().saturating_sub(1)),
                    KeyCode::Up => sel = sel.saturating_sub(1),
                    KeyCode::Char('l') | KeyCode::Char('L') => app.msg = control::lockdown(f),
                    KeyCode::Enter | KeyCode::Char(' ') if !list.is_empty() => {
                        let (k, v, lab) = list[sel.min(list.len() - 1)].clone();
                        if HostSettings::is_switch(&k) { let new = if truthy(&v) { "off" } else { "on" }; app.msg = change(f, &k, |c| c.set(&k, new)); }
                        else {
                            let pre = if k == "token" || v == "off" || v == "(none)" || v == "(not set)" { String::new() } else { v.clone() };
                            form = Some(Form::one(&format!(" {lab} "), HostSettings::explain(&k), "New value", &pre, k == "token", Kind::Setting(k.clone())));
                        }
                    }
                    _ => {}
                }
            }
            app.screen = Screen::Settings { sel, form };
            return false;
        }
        Screen::Providers { mut sel, mut confirm, mut form } => {
            let cfg = HostSettings::load(&f.config).ok();
            let rows = cfg.as_ref().map(|c| provider_rows(c, &app.ollama)).unwrap_or_default();
            if let Some(mut fm) = form.take() {
                match form_key(&mut fm, code) {
                    Some(true) => {
                        let v: Vec<String> = fm.fields.iter().map(|x| x.value.clone()).collect();
                        match fm.kind.clone() {
                            Kind::AddEndpoint => { app.msg = change(f, &format!("endpoint \"{}\"", v[0].trim()), |c| c.add_endpoint(&v[0], &v[1], &v[2], &v[3], &v[4])); }
                            Kind::Setting(k) => { app.msg = change(f, &k, |c| c.set(&k, &v[0])); }
                            Kind::Setup => {}
                        }
                    }
                    Some(false) => app.msg = "Cancelled.".into(),
                    None => form = Some(fm),
                }
            } else {
                let was_confirm = confirm.take();
                match code {
                    KeyCode::Esc | KeyCode::Char('q') => { return false; }
                    KeyCode::Down => sel = (sel + 1).min(rows.len().saturating_sub(1)),
                    KeyCode::Up => sel = sel.saturating_sub(1),
                    KeyCode::Char('n') => form = Some(Form::add_endpoint()),
                    KeyCode::Char('o') => {
                        if let (Some(PRow::Detected), Some((url, models))) = (rows.get(sel).map(|r| &r.0), app.ollama.clone()) { app.msg = change(f, "the Ollama provider", |c| c.adopt_ollama(&url, &models)); }
                        else { app.msg = "[o] adopts the Ollama row (the one marked found on this machine).".into(); }
                    }
                    KeyCode::Char('x') | KeyCode::Delete => {
                        let id = match rows.get(sel).map(|r| &r.0) { Some(PRow::Header(i)) => Some(i.clone()), Some(PRow::Field { id, .. }) => Some(id.clone()), _ => None };
                        match id {
                            Some(id) if was_confirm.as_deref() == Some(id.as_str()) => { app.msg = change(f, &format!("removal of \"{id}\""), |c| { c.remove_provider(&id); Ok(()) }); sel = sel.saturating_sub(1); }
                            Some(id) => { app.msg = format!("Press x again to remove \"{id}\" from ai-node.json."); confirm = Some(id); }
                            None => {}
                        }
                    }
                    KeyCode::Enter | KeyCode::Char(' ') => match rows.get(sel).map(|r| &r.0) {
                        Some(PRow::Field { id, key, switch }) => {
                            let p = cfg.as_ref().and_then(|c| c.all_providers().into_iter().find(|p| p.id == *id));
                            if *switch { let cur = p.map(|p| if key.ends_with(".enabled") { p.enabled } else { p.share_with_group }).unwrap_or(false); let new = if cur { "off" } else { "on" }; app.msg = change(f, key, |c| c.set(key, new)); }
                            else {
                                let pre = p.map(|p| if key.ends_with(".allowedUsers") { p.allowed_users.join(", ") } else if key.ends_with(".label") { p.label } else { p.models.join(", ") }).unwrap_or_default();
                                form = Some(Form::one(&format!(" {key} "), &field_explain(key), "New value", &pre, false, Kind::Setting(key.clone())));
                            }
                        }
                        Some(PRow::Detected) => app.msg = "Press o to adopt it into ai-node.json.".into(),
                        _ => {}
                    },
                    _ => {}
                }
            }
            app.screen = Screen::Providers { sel, confirm, form };
            return false;
        }
        Screen::Doctor { mut sel, mut checks } => {
            match code {
                KeyCode::Esc | KeyCode::Char('q') => return false,
                KeyCode::Down => sel = (sel + 1).min(checks.len().saturating_sub(1)),
                KeyCode::Up => sel = sel.saturating_sub(1),
                KeyCode::Char('r') | KeyCode::Char('R') => { checks = control::doctor(f); app.msg = "Checked again.".into(); }
                KeyCode::Enter => match checks.get(sel).cloned() {
                    Some(c) if c.ok == Some(false) && c.fix.is_some() => {
                        let id = c.fix.unwrap_or("");
                        let out = suspended(term, || { let s = control::fix(f, id); println!("{s}"); std::thread::sleep(Duration::from_millis(600)); s });
                        app.msg = out; app.copies = install::copies(); checks = control::doctor(f);
                    }
                    Some(c) => app.msg = c.text,
                    None => {}
                },
                _ => {}
            }
            app.screen = Screen::Doctor { sel, checks };
            return false;
        }
        Screen::Boot { mut sel } => {
            match code {
                KeyCode::Esc | KeyCode::Char('q') => return false,
                KeyCode::Down => sel = (sel + 1).min(control::boot_modes().len() - 1),
                KeyCode::Up => sel = sel.saturating_sub(1),
                KeyCode::Enter => {
                    let m = control::boot_modes()[sel];
                    let lines = suspended(term, || { let l = control::set_boot(f, m); for x in &l { println!("{x}"); } l });
                    app.msg = lines.join(" ");
                    return false;
                }
                _ => {}
            }
            app.screen = Screen::Boot { sel };
            return false;
        }
        Screen::Log { lines, mut scroll } => {
            let max = lines.len();
            match code {
                KeyCode::Esc | KeyCode::Char('q') => return false,
                KeyCode::Down => scroll = (scroll + 1).min(max),
                KeyCode::Up => scroll = scroll.saturating_sub(1),
                KeyCode::PageDown => scroll = (scroll + 20).min(max),
                KeyCode::PageUp => scroll = scroll.saturating_sub(20),
                KeyCode::End => scroll = max.saturating_sub(app.view_h),
                KeyCode::Home => scroll = 0,
                _ => {}
            }
            app.screen = Screen::Log { lines, scroll };
            return false;
        }
    }
    false
}

/// Checks and writes the first-run settings (a pasted ai-node.json, its path, or the three values). Never echoes the token.
fn submit_setup(f: &Folder, form: &Form, paste: bool) -> Result<(), String> {
    let cfg = if paste {
        let t = form.fields[0].value.trim();
        let text = if t.starts_with('{') { t.to_string() } else {
            let p = t.trim_matches(|c| c == '"' || c == '\'');
            std::fs::read_to_string(p).map_err(|e| format!("That is neither JSON text nor a readable file ({p}): {e}"))?
        };
        HostSettings::from_text(&f.config, &text).map_err(|e| e.to_string())?
    } else { HostSettings::from_parts(&f.config, &form.fields[0].value, &form.fields[1].value, &form.fields[2].value).map_err(|e| e.to_string())? };
    cfg.check().map_err(|e| e.to_string())?;
    cfg.save().map_err(|e| e.to_string())
}
