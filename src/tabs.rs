use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::block::Blocker;
use crate::dom;
use crate::layout::{self, Document, Focus};
use crate::load::Fetched;

pub const DEFAULT_SEARCH: &str = "https://lite.duckduckgo.com/lite/?q={q}";

#[derive(Clone, Debug)]
pub enum Mode {
    Normal,
    Open(String),
    Command(String),
    Find(String),
    Edit(String),
    Hint { new_tab: bool },
}

pub struct Job {
    pub tab: usize,
    pub generation: u64,
    pub url: String,
    pub source: String,
    pub post: bool,
    pub body: Option<String>,
}

struct Entry {
    url: String,
    scroll: u16,
}

pub enum Body {
    Loaded { html: String, doc: Document },
    Unloaded,
}

struct Tab {
    history: Vec<Entry>,
    index: usize,
    body: Body,
    focus: Option<usize>,
    cursor: u16,
    blocked: u32,
    hidden: u32,
    error: Option<String>,
    generation: u64,
    loading: bool,
    blurred: Option<Instant>,
}

pub struct App {
    tabs: Vec<Tab>,
    current: usize,
    width: usize,
    view_rows: usize,
    pub quit: bool,
    pub mode: Mode,
    pub message: String,
    pub find_query: String,
    find_at: Option<usize>,
    ttl_minutes: u64,
    search: String,
    config_path: PathBuf,
    pub jobs: Vec<Job>,
    pub update: bool,
    pub overlay: Option<Overlay>,
}

impl App {
    pub fn new(width: usize, config_path: PathBuf) -> Self {
        let (ttl_minutes, search) = load_settings(&config_path);
        Self {
            tabs: vec![Tab::blank()],
            current: 0,
            width: width.max(1),
            view_rows: 20,
            quit: false,
            mode: Mode::Normal,
            message: String::new(),
            find_query: String::new(),
            find_at: None,
            ttl_minutes,
            search,
            config_path,
            jobs: Vec::new(),
            update: false,
            overlay: None,
        }
    }

    pub fn open_cli(&mut self, input: &str) {
        if let Some(url) = destination(input, &self.search) {
            self.follow(url);
        }
    }

    pub fn set_viewport(&mut self, width: usize, rows: usize, blocker: &Blocker) {
        self.view_rows = rows.max(1);
        if width == 0 || width == self.width {
            return;
        }
        self.width = width;
        let loaded: Vec<(usize, String, String)> = self
            .tabs
            .iter()
            .enumerate()
            .filter_map(|(index, tab)| match &tab.body {
                Body::Loaded { html, .. } => Some((index, html.clone(), tab.url())),
                Body::Unloaded => None,
            })
            .collect();
        for (index, html, url) in loaded {
            let rendered = dom::render(&html, &url, width, blocker);
            let tab = &mut self.tabs[index];
            tab.hidden = rendered.hidden;
            if tab
                .focus
                .is_some_and(|focus| focus >= rendered.document.focus.len())
            {
                tab.focus = None;
            }
            let max = rendered.document.lines.len().saturating_sub(self.view_rows) as u16;
            tab.history[tab.index].scroll = tab.history[tab.index].scroll.min(max);
            let last = rendered.document.lines.len().saturating_sub(1) as u16;
            tab.cursor = tab.cursor.min(last);
            tab.body = Body::Loaded {
                html,
                doc: rendered.document,
            };
        }
    }

    pub fn expire(&mut self) {
        if self.ttl_minutes == 0 {
            return;
        }
        let limit = Duration::from_secs(self.ttl_minutes.saturating_mul(60));
        let now = Instant::now();
        let current = self.current;
        for (index, tab) in self.tabs.iter_mut().enumerate() {
            if index == current {
                continue;
            }
            let due = tab
                .blurred
                .is_some_and(|blurred| now.saturating_duration_since(blurred) >= limit);
            if due && matches!(tab.body, Body::Loaded { .. }) {
                tab.body = Body::Unloaded;
            }
        }
    }

    pub fn finish(
        &mut self,
        tab: usize,
        generation: u64,
        result: Result<Fetched, String>,
        blocker: &Blocker,
    ) {
        let Some(slot) = self.tabs.get(tab) else {
            return;
        };
        if slot.generation != generation {
            return;
        }
        let mut reveal = false;
        match result {
            Ok(page) => {
                let rendered = dom::render(&page.html, &page.final_url, self.width, blocker);
                let slot = &mut self.tabs[tab];
                slot.loading = false;
                slot.hidden = rendered.hidden;
                slot.blocked = page.blocked;
                slot.error = None;
                let max = rendered.document.lines.len().saturating_sub(self.view_rows) as u16;
                slot.history[slot.index].url = page.final_url;
                slot.history[slot.index].scroll = slot.history[slot.index].scroll.min(max);
                let last = rendered.document.lines.len().saturating_sub(1) as u16;
                slot.cursor = slot.history[slot.index].scroll.min(last);
                slot.body = Body::Loaded {
                    html: page.html,
                    doc: rendered.document,
                };
                if tab == self.current {
                    self.message.clear();
                    reveal = true;
                }
            }
            Err(err) => {
                let slot = &mut self.tabs[tab];
                slot.loading = false;
                if err == "blocked" {
                    slot.blocked = 1;
                }
                slot.error = Some(err);
            }
        }
        if reveal {
            self.focus_on_cursor();
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
            self.quit = true;
            return;
        }
        if self.overlay.is_some() {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
                if key.code == KeyCode::Char('q') {
                    self.quit = true;
                }
                self.overlay = None;
            }
            return;
        }
        let mode = self.mode.clone();
        match mode {
            Mode::Normal => self.normal_key(key),
            Mode::Open(buffer) => self.prompt_key(key, buffer, Prompt::Open),
            Mode::Command(buffer) => self.prompt_key(key, buffer, Prompt::Command),
            Mode::Find(buffer) => self.prompt_key(key, buffer, Prompt::Find),
            Mode::Edit(buffer) => self.prompt_key(key, buffer, Prompt::Edit),
            Mode::Hint { new_tab } => self.hint_key(key, new_tab),
        }
    }

    pub fn paste(&mut self, text: &str) {
        let extra = text
            .chars()
            .filter(|ch| !ch.is_control())
            .collect::<String>();
        match &mut self.mode {
            Mode::Open(buffer)
            | Mode::Command(buffer)
            | Mode::Find(buffer)
            | Mode::Edit(buffer) => buffer.push_str(&extra),
            Mode::Normal | Mode::Hint { .. } => {}
        }
    }

    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    pub fn current(&self) -> usize {
        self.current
    }

    pub fn tab_label(&self, index: usize) -> String {
        let tab = &self.tabs[index];
        let mut name = match &tab.body {
            Body::Loaded { doc, .. } if !doc.title.is_empty() => doc.title.clone(),
            _ if tab.url().is_empty() => "new".into(),
            _ => tab.url(),
        };
        if name.chars().count() > 18 {
            name = name.chars().take(18).collect();
        }
        if matches!(tab.body, Body::Unloaded) && !tab.url().is_empty() {
            name.push('*');
        }
        format!("{} {name}", index + 1)
    }

    pub fn url_line(&self) -> String {
        match &self.mode {
            Mode::Open(buffer) => format!("open {buffer}"),
            Mode::Command(buffer) => format!(":{buffer}"),
            Mode::Find(buffer) => format!("/{buffer}"),
            Mode::Edit(buffer) => format!("edit {buffer}"),
            Mode::Hint { new_tab } => {
                if *new_tab {
                    "hint new tab".into()
                } else {
                    "hint".into()
                }
            }
            Mode::Normal => {
                let url = self.tabs[self.current].url();
                if url.is_empty() { "o open".into() } else { url }
            }
        }
    }

    pub fn status_line(&self, updating: bool) -> String {
        let tab = &self.tabs[self.current];
        let mut parts = Vec::new();
        if tab.loading {
            parts.push("loading".into());
        }
        if updating {
            parts.push("updating filters".into());
        }
        if let Some(error) = &tab.error {
            parts.push(error.clone());
        }
        if !self.message.is_empty() {
            parts.push(self.message.clone());
        }
        if matches!(tab.body, Body::Unloaded) && !tab.url().is_empty() {
            parts.push("unloaded".into());
        }
        if let Body::Loaded { doc, .. } = &tab.body
            && doc.text_chars == 0
            && tab.error.is_none()
            && !tab.loading
        {
            parts.push("no text content".into());
        }
        if matches!(self.mode, Mode::Normal)
            && let Body::Loaded { doc, .. } = &tab.body
            && !doc.title.is_empty()
        {
            parts.push(doc.title.clone());
        }
        if !self.find_query.is_empty() {
            parts.push(format!("/{}", self.find_query));
        }
        if let Body::Loaded { doc, .. } = &tab.body {
            let on_link = tab
                .focus
                .and_then(|index| doc.focus.get(index))
                .is_some_and(|item| matches!(item, Focus::Link { .. }));
            if on_link {
                parts.push("Enter follow  T new tab".into());
            } else if tab
                .focus
                .is_some_and(|index| matches!(doc.focus.get(index), Some(Focus::Field { .. })))
            {
                parts.push("Enter edit".into());
            } else if tab
                .focus
                .is_some_and(|index| matches!(doc.focus.get(index), Some(Focus::Submit { .. })))
            {
                parts.push("Enter submit".into());
            }
        }
        parts.push(format!("hidden {}", tab.hidden));
        if tab.blocked > 0 {
            parts.push(format!("blocked {}", tab.blocked));
        }
        parts.join("  ")
    }

    pub fn viewport_lines(&self) -> Vec<ViewportLine> {
        let tab = &self.tabs[self.current];
        let Body::Loaded { doc, .. } = &tab.body else {
            return Vec::new();
        };
        let labels = hint_labels(doc);
        let hinting = matches!(self.mode, Mode::Hint { .. });
        let cursor = tab.cursor as usize;
        let start = tab.history[tab.index].scroll as usize;
        let end = (start + self.view_rows).min(doc.lines.len());
        doc.lines[start..end]
            .iter()
            .enumerate()
            .map(|(offset, line)| {
                let index = start + offset;
                let mut seen_hint = Vec::new();
                ViewportLine {
                    pieces: line
                        .spans
                        .iter()
                        .map(|piece| {
                            let hint = piece.focus.and_then(|focus| {
                                labels
                                    .iter()
                                    .find(|(index, _)| *index == focus)
                                    .map(|(_, ch)| *ch)
                            });
                            let show = hinting
                                && hint.is_some_and(|label| {
                                    if seen_hint.contains(&label) {
                                        false
                                    } else {
                                        seen_hint.push(label);
                                        true
                                    }
                                });
                            DrawnPiece {
                                text: piece.text.clone(),
                                style: piece.style,
                                hint: show.then_some(hint).flatten(),
                            }
                        })
                        .collect(),
                    current_match: self.find_at == Some(index),
                    cursor: index == cursor,
                }
            })
            .collect()
    }

    fn normal_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('?') => self.overlay = Some(Overlay::Help),
            KeyCode::Char('o') => self.mode = Mode::Open(String::new()),
            KeyCode::Char(':') => self.mode = Mode::Command(String::new()),
            KeyCode::Char('/') => self.mode = Mode::Find(self.find_query.clone()),
            KeyCode::Char('t') => self.new_tab(),
            KeyCode::Char('W') => self.close_tab(),
            KeyCode::Char(']') => self.cycle_tab(1),
            KeyCode::Char('[') => self.cycle_tab(-1),
            KeyCode::Tab if key.modifiers.contains(KeyModifiers::CONTROL) => self.cycle_tab(1),
            KeyCode::BackTab if key.modifiers.contains(KeyModifiers::CONTROL) => self.cycle_tab(-1),
            KeyCode::Char(ch @ '1'..='9') if key.modifiers.is_empty() => {
                self.focus_tab((ch as u8 - b'1') as usize)
            }
            KeyCode::Char('j') | KeyCode::Down => self.move_cursor(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_cursor(-1),
            KeyCode::PageDown => self.move_cursor(self.view_rows as i32),
            KeyCode::PageUp => self.move_cursor(-(self.view_rows as i32)),
            KeyCode::Char('g') => self.scroll_to(true),
            KeyCode::Char('G') => self.scroll_to(false),
            KeyCode::Tab => self.move_focus(1),
            KeyCode::BackTab => self.move_focus(-1),
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => self.activate(true),
            KeyCode::Enter => self.activate(false),
            KeyCode::Char('T') => self.activate(true),
            KeyCode::Char('f') => self.mode = Mode::Hint { new_tab: false },
            KeyCode::Char('F') => self.mode = Mode::Hint { new_tab: true },
            KeyCode::Char('b') | KeyCode::Backspace => self.back(),
            KeyCode::Char('r') => self.reload(),
            KeyCode::Char('n') => self.jump_find(1),
            KeyCode::Char('N') => self.jump_find(-1),
            _ => {}
        }
    }

    fn prompt_key(&mut self, key: KeyEvent, mut buffer: String, prompt: Prompt) {
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Enter => {
                self.mode = Mode::Normal;
                match prompt {
                    Prompt::Open => self.follow_input(buffer),
                    Prompt::Command => self.command(&buffer),
                    Prompt::Find => self.commit_find(buffer),
                    Prompt::Edit => self.commit_edit(buffer),
                }
            }
            KeyCode::Backspace => {
                buffer.pop();
                self.mode = prompt.with(buffer);
            }
            KeyCode::Char(ch) => {
                buffer.push(ch);
                self.mode = prompt.with(buffer);
            }
            _ => self.mode = prompt.with(buffer),
        }
    }

    fn hint_key(&mut self, key: KeyEvent, new_tab: bool) {
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Char(ch @ 'a'..='z') => {
                self.mode = Mode::Normal;
                if let Some(index) = self.hint_index(ch) {
                    self.tabs[self.current].focus = Some(index);
                    self.activate(new_tab);
                }
            }
            _ => {}
        }
    }

    fn follow_input(&mut self, input: String) {
        if let Some(url) = destination(&input, &self.search) {
            self.follow(url);
        }
    }

    fn follow(&mut self, url: String) {
        let source = self.current_url();
        self.push_history(url.clone());
        let tab = self.current;
        self.begin(tab, url, source, false, None);
    }

    fn command(&mut self, raw: &str) {
        let mut parts = raw.split_whitespace();
        match parts.next().unwrap_or("") {
            "" => {}
            "unload" => self.tabs[self.current].body = Body::Unloaded,
            "unloadothers" => {
                let current = self.current;
                for (index, tab) in self.tabs.iter_mut().enumerate() {
                    if index != current {
                        tab.body = Body::Unloaded;
                    }
                }
            }
            "ttl" => {
                if let Some(minutes) = parts.next().and_then(|value| value.parse::<u64>().ok()) {
                    self.ttl_minutes = minutes;
                    self.save_settings();
                    self.message = format!("tab_ttl={minutes}");
                } else {
                    self.message = format!("tab_ttl={}", self.ttl_minutes);
                }
            }
            "help" => self.overlay = Some(Overlay::Help),
            "lists" => self.overlay = Some(Overlay::Lists),
            "update" => self.update = true,
            "js" => self.message = "javascript rendering is not built".into(),
            other => self.message = format!("unknown command {other}"),
        }
    }

    fn commit_find(&mut self, query: String) {
        self.find_query = query.trim().to_string();
        self.find_at = None;
        if !self.find_query.is_empty() {
            self.jump_find(1);
        }
    }

    fn jump_find(&mut self, direction: i32) {
        let Some(lines) = self.match_lines() else {
            return;
        };
        if lines.is_empty() {
            self.message = "no matches".into();
            return;
        }
        let next = match self.find_at {
            Some(current) if direction > 0 => lines
                .iter()
                .copied()
                .find(|line| *line > current)
                .unwrap_or(lines[0]),
            Some(current) => lines
                .iter()
                .copied()
                .rev()
                .find(|line| *line < current)
                .unwrap_or(*lines.last().unwrap()),
            None => lines[0],
        };
        self.find_at = Some(next);
        self.message.clear();
        self.tabs[self.current].cursor = next as u16;
        self.keep_cursor_visible();
        self.focus_on_cursor();
    }

    fn match_lines(&self) -> Option<Vec<usize>> {
        if self.find_query.is_empty() {
            return None;
        }
        let Body::Loaded { doc, .. } = &self.tabs[self.current].body else {
            return None;
        };
        let query = self.find_query.to_lowercase();
        Some(
            doc.lines
                .iter()
                .enumerate()
                .filter(|(_, line)| layout::line_text(line).to_lowercase().contains(&query))
                .map(|(index, _)| index)
                .collect(),
        )
    }

    fn commit_edit(&mut self, buffer: String) {
        let Some(focus) = self.tabs[self.current].focus else {
            return;
        };
        let Body::Loaded { doc, .. } = &mut self.tabs[self.current].body else {
            return;
        };
        let Some(Focus::Field { form, field }) = doc.focus.get(focus).cloned() else {
            return;
        };
        doc.forms[form].fields[field].value = buffer.clone();
        let shown = if buffer.is_empty() {
            doc.forms[form].fields[field].name.clone()
        } else {
            buffer
        };
        for line in &mut doc.lines {
            for piece in &mut line.spans {
                if piece.focus == Some(focus) {
                    piece.text = format!("[{shown}]");
                }
            }
        }
    }

    fn activate(&mut self, new_tab: bool) {
        let Some(focus) = self.tabs[self.current].focus else {
            return;
        };
        let Some(item) = self.tabs[self.current]
            .document()
            .and_then(|doc| doc.focus.get(focus).cloned())
        else {
            return;
        };
        match item {
            Focus::Link { url } if new_tab => self.open_tab(Some(url)),
            Focus::Link { url } => self.follow(url),
            Focus::Field { form, field } if !new_tab => {
                let value = self.tabs[self.current].document().unwrap().forms[form].fields[field]
                    .value
                    .clone();
                self.mode = Mode::Edit(value);
            }
            Focus::Submit { form, field } if !new_tab => {
                let Some(doc) = self.tabs[self.current].document() else {
                    return;
                };
                let Some(submission) = layout::submission(doc, form, Some(field)) else {
                    return;
                };
                let source = self.current_url();
                self.push_history(submission.url.clone());
                let tab = self.current;
                self.begin(
                    tab,
                    submission.url,
                    source,
                    submission.post,
                    submission.body,
                );
            }
            _ => {}
        }
    }

    fn new_tab(&mut self) {
        self.open_tab(None);
    }

    fn open_tab(&mut self, url: Option<String>) {
        let at = self.current + 1;
        self.tabs.insert(at, Tab::blank());
        self.focus_tab(at);
        if let Some(url) = url {
            self.follow(url);
        }
    }

    fn close_tab(&mut self) {
        if self.tabs.len() == 1 {
            self.tabs[0] = Tab::blank();
            return;
        }
        self.tabs.remove(self.current);
        if self.current >= self.tabs.len() {
            self.current = self.tabs.len() - 1;
        }
        self.tabs[self.current].blurred = None;
        self.load_if_unloaded(self.current);
    }

    fn cycle_tab(&mut self, delta: isize) {
        let len = self.tabs.len() as isize;
        let next = (self.current as isize + delta).rem_euclid(len) as usize;
        self.focus_tab(next);
    }

    fn focus_tab(&mut self, index: usize) {
        if index >= self.tabs.len() || index == self.current {
            return;
        }
        self.tabs[self.current].blurred = Some(Instant::now());
        self.current = index;
        self.tabs[index].blurred = None;
        self.load_if_unloaded(index);
    }

    fn load_if_unloaded(&mut self, index: usize) {
        let unloaded = matches!(self.tabs[index].body, Body::Unloaded);
        let url = self.tabs[index].url();
        if unloaded && !url.is_empty() && !self.tabs[index].loading {
            self.begin(index, url.clone(), url, false, None);
        }
    }

    fn reload(&mut self) {
        let url = self.current_url();
        if url.is_empty() {
            return;
        }
        let tab = self.current;
        self.begin(tab, url.clone(), url, false, None);
    }

    fn back(&mut self) {
        if self.tabs[self.current].index == 0 {
            return;
        }
        self.tabs[self.current].index -= 1;
        self.tabs[self.current].focus = None;
        let url = self.tabs[self.current].url();
        if url.is_empty() {
            return;
        }
        let tab = self.current;
        self.begin(tab, url.clone(), url, false, None);
    }

    fn move_focus(&mut self, delta: isize) {
        let cursor = self.tabs[self.current].cursor as usize;
        let Some(doc) = self.tabs[self.current].document() else {
            return;
        };
        let Some(line) = doc.lines.get(cursor) else {
            return;
        };
        let mut ids = Vec::new();
        for piece in &line.spans {
            if let Some(focus) = piece.focus
                && !ids.contains(&focus)
            {
                ids.push(focus);
            }
        }
        if ids.is_empty() {
            return;
        }
        let next = match self.tabs[self.current]
            .focus
            .and_then(|focus| ids.iter().position(|id| *id == focus))
        {
            Some(index) => (index as isize + delta).rem_euclid(ids.len() as isize) as usize,
            None if delta < 0 => ids.len() - 1,
            None => 0,
        };
        self.tabs[self.current].focus = Some(ids[next]);
    }

    fn move_cursor(&mut self, delta: i32) {
        let lines = self.tabs[self.current].line_count();
        if lines == 0 {
            return;
        }
        let next = (self.tabs[self.current].cursor as i32 + delta).clamp(0, (lines - 1) as i32);
        self.tabs[self.current].cursor = next as u16;
        self.keep_cursor_visible();
        self.focus_on_cursor();
    }

    fn scroll_to(&mut self, top: bool) {
        let lines = self.tabs[self.current].line_count();
        if lines == 0 {
            return;
        }
        self.tabs[self.current].cursor = if top { 0 } else { (lines - 1) as u16 };
        self.keep_cursor_visible();
        self.focus_on_cursor();
    }

    fn keep_cursor_visible(&mut self) {
        let cursor = self.tabs[self.current].cursor as usize;
        let view = self.view_rows.max(1);
        let index = self.tabs[self.current].index;
        let scroll = self.tabs[self.current].history[index].scroll as usize;
        let next = if cursor < scroll {
            cursor
        } else if cursor >= scroll + view {
            cursor + 1 - view
        } else {
            scroll
        };
        self.tabs[self.current].history[index].scroll = next as u16;
    }

    fn focus_on_cursor(&mut self) {
        let cursor = self.tabs[self.current].cursor as usize;
        let focus = self.tabs[self.current].document().and_then(|doc| {
            doc.lines
                .get(cursor)
                .and_then(|line| line.spans.iter().find_map(|piece| piece.focus))
        });
        self.tabs[self.current].focus = focus;
    }

    fn push_history(&mut self, url: String) {
        let tab = &mut self.tabs[self.current];
        if tab.history[tab.index].url.is_empty() {
            tab.history[tab.index].url = url;
            tab.history[tab.index].scroll = 0;
            tab.cursor = 0;
            tab.focus = None;
            return;
        }
        tab.history.truncate(tab.index + 1);
        tab.history.push(Entry { url, scroll: 0 });
        tab.index = tab.history.len() - 1;
        tab.cursor = 0;
        tab.focus = None;
    }

    fn begin(&mut self, tab: usize, url: String, source: String, post: bool, body: Option<String>) {
        let slot = &mut self.tabs[tab];
        slot.generation += 1;
        slot.loading = true;
        slot.error = None;
        let generation = slot.generation;
        self.jobs.push(Job {
            tab,
            generation,
            url,
            source,
            post,
            body,
        });
    }

    fn hint_index(&self, ch: char) -> Option<usize> {
        let doc = self.tabs[self.current].document()?;
        hint_labels(doc)
            .into_iter()
            .find(|(_, label)| *label == ch)
            .map(|(index, _)| index)
    }

    fn current_url(&self) -> String {
        self.tabs[self.current].url()
    }

    fn save_settings(&self) {
        if let Some(parent) = self.config_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let text = format!("tab_ttl={}\nsearch={}\n", self.ttl_minutes, self.search);
        let _ = fs::write(&self.config_path, text);
    }
}

impl Tab {
    fn blank() -> Self {
        Self {
            history: vec![Entry {
                url: String::new(),
                scroll: 0,
            }],
            index: 0,
            body: Body::Unloaded,
            focus: None,
            cursor: 0,
            blocked: 0,
            hidden: 0,
            error: None,
            generation: 0,
            loading: false,
            blurred: None,
        }
    }

    fn url(&self) -> String {
        self.history[self.index].url.clone()
    }

    fn document(&self) -> Option<&Document> {
        match &self.body {
            Body::Loaded { doc, .. } => Some(doc),
            Body::Unloaded => None,
        }
    }

    fn line_count(&self) -> usize {
        self.document().map(|doc| doc.lines.len()).unwrap_or(0)
    }
}

// shortcut: hint letters cover the first 26 links, upgrade if a page needs labels past z
fn hint_labels(doc: &Document) -> Vec<(usize, char)> {
    let mut count = 0;
    let mut labels = Vec::new();
    for (index, item) in doc.focus.iter().enumerate() {
        if matches!(item, Focus::Link { .. }) {
            if count < 26 {
                labels.push((index, (b'a' + count as u8) as char));
            }
            count += 1;
        }
    }
    labels
}

#[derive(Clone, Copy)]
enum Prompt {
    Open,
    Command,
    Find,
    Edit,
}

impl Prompt {
    fn with(self, buffer: String) -> Mode {
        match self {
            Prompt::Open => Mode::Open(buffer),
            Prompt::Command => Mode::Command(buffer),
            Prompt::Find => Mode::Find(buffer),
            Prompt::Edit => Mode::Edit(buffer),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Overlay {
    Help,
    Lists,
}

pub fn help_lines() -> &'static [&'static str] {
    &[
        "j k          move the line cursor",
        "PgUp PgDn    page",
        "g G          top and bottom",
        "Enter        open the link on this line",
        "T            open that link in a new tab",
        "Tab          other link on this line",
        "o            open a url or search",
        "/ n N        find",
        "t W          new tab, close tab",
        "] [  1-9     switch tab, jump to a tab",
        "b            back",
        "r            reload",
        "f F          hint, hint in a new tab",
        ":help        this list",
        ":lists       filter lists",
        ":update      download filter lists",
        ":unload      drop this tab's page",
        ":unloadothers",
        ":ttl N       unload background tabs after N minutes (0 off)",
        "q            quit",
        "Esc          close this",
    ]
}

pub struct ViewportLine {
    pub pieces: Vec<DrawnPiece>,
    pub current_match: bool,
    pub cursor: bool,
}

pub struct DrawnPiece {
    pub text: String,
    pub style: layout::StyleKind,
    pub hint: Option<char>,
}

pub fn destination(input: &str, template: &str) -> Option<String> {
    let text = input.trim();
    if text.is_empty() {
        return None;
    }
    if text.contains("://") {
        return Some(text.to_string());
    }
    if text.starts_with("localhost") || (text.contains('.') && !text.contains(' ')) {
        return Some(format!("https://{text}"));
    }
    let query: String = url::form_urlencoded::byte_serialize(text.as_bytes()).collect();
    Some(template.replace("{q}", &query))
}

fn load_settings(path: &Path) -> (u64, String) {
    let mut ttl = 0u64;
    let mut search = DEFAULT_SEARCH.to_string();
    if let Ok(text) = fs::read_to_string(path) {
        for line in text.lines() {
            if let Some(value) = line.strip_prefix("tab_ttl=") {
                ttl = value.trim().parse().unwrap_or(0);
            } else if let Some(value) = line.strip_prefix("search=")
                && value.contains("{q}")
            {
                search = value.trim().to_string();
            }
        }
    }
    (ttl, search)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    fn key(ch: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE)
    }

    fn blank() -> App {
        App::new(40, std::env::temp_dir().join("tbw-test-config-absent"))
    }

    #[test]
    fn quit_and_open_are_distinct() {
        let mut app = blank();
        app.on_key(key('q'));
        assert!(app.quit);
        let mut app = blank();
        app.on_key(key('o'));
        app.on_key(key('e'));
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.jobs.len(), 1);
        assert!(app.jobs[0].url.starts_with("https://"));
    }

    #[test]
    fn destination_routes_search_and_urls() {
        let template = DEFAULT_SEARCH;
        assert_eq!(
            destination("https://example.org/a", template).as_deref(),
            Some("https://example.org/a")
        );
        assert_eq!(
            destination("example.org", template).as_deref(),
            Some("https://example.org")
        );
        let search = destination("rust docs", template).unwrap();
        assert!(search.contains("lite.duckduckgo.com"), "{search}");
        assert!(
            search.contains("rust+docs") || search.contains("rust%20docs"),
            "{search}"
        );
    }

    #[test]
    fn t_opens_the_focused_link_in_a_new_tab() {
        let blocker = crate::block::Blocker::from_rules("");
        let mut app = blank();
        app.finish(
            0,
            0,
            Ok(crate::load::Fetched {
                final_url: "https://example.com/".into(),
                html: "<p><a href=\"/next\">next</a></p>".into(),
                blocked: 0,
            }),
            &blocker,
        );
        app.on_key(key('T'));
        assert_eq!(app.tab_count(), 2);
        assert_eq!(app.current(), 1);
        assert_eq!(app.jobs.last().unwrap().url, "https://example.com/next");
    }

    #[test]
    fn enter_follows_the_link_on_the_cursor_line() {
        let blocker = crate::block::Blocker::from_rules("");
        let mut app = blank();
        let mut html = String::new();
        for n in 0..25 {
            html.push_str(&format!("<p>line {n}</p>"));
        }
        html.push_str("<p><a href=\"/late\">late</a></p>");
        app.finish(
            0,
            0,
            Ok(crate::load::Fetched {
                final_url: "https://example.com/".into(),
                html,
                blocked: 0,
            }),
            &blocker,
        );
        for _ in 0..40 {
            if app.status_line(false).contains("Enter follow") {
                break;
            }
            app.on_key(key('j'));
        }
        assert!(app.status_line(false).contains("Enter follow"));
        let scroll = app.tabs[0].history[0].scroll;
        assert!(scroll > 0, "{scroll}");
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.tabs[0].history[0].scroll, scroll);
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.jobs.last().unwrap().url, "https://example.com/late");
    }

    #[test]
    fn tabs_switch_and_unload_keeps_the_url() {
        let mut app = blank();
        app.tabs[0].history[0].url = "https://one.test/".into();
        app.tabs[0].body = Body::Loaded {
            html: "<p>one</p>".into(),
            doc: layout::layout(
                &scraper::Html::parse_document("<p>one</p>"),
                "https://one.test/",
                40,
                &std::collections::HashSet::new(),
            )
            .0,
        };
        app.on_key(key('t'));
        assert_eq!(app.tab_count(), 2);
        assert_eq!(app.current(), 1);
        app.on_key(KeyEvent::new(KeyCode::Char('['), KeyModifiers::NONE));
        assert_eq!(app.current(), 0);
        app.on_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::NONE));
        for ch in "unload".chars() {
            app.on_key(key(ch));
        }
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.tabs[0].body, Body::Unloaded));
        assert_eq!(app.tabs[0].url(), "https://one.test/");
        app.on_key(key('t'));
        app.tabs[0].blurred = Some(Instant::now() - Duration::from_secs(120));
        app.ttl_minutes = 1;
        app.tabs[0].body = Body::Loaded {
            html: "<p>one</p>".into(),
            doc: layout::layout(
                &scraper::Html::parse_document("<p>one</p>"),
                "https://one.test/",
                40,
                &std::collections::HashSet::new(),
            )
            .0,
        };
        app.tabs[0].history[0].scroll = 4;
        app.expire();
        assert!(matches!(app.tabs[0].body, Body::Unloaded));
        assert_eq!(app.tabs[0].url(), "https://one.test/");
        app.on_key(key('1'));
        assert_eq!(app.current(), 0);
        assert_eq!(app.jobs.len(), 1);
        assert_eq!(app.jobs[0].url, "https://one.test/");
        assert_eq!(app.tabs[0].history[0].scroll, 4);
        let generation = app.tabs[0].generation;
        app.finish(
            0,
            generation,
            Ok(crate::load::Fetched {
                final_url: "https://one.test/".into(),
                html: "<p>one</p>".into(),
                blocked: 0,
            }),
            &crate::block::Blocker::from_rules(""),
        );
        assert!(matches!(app.tabs[0].body, Body::Loaded { .. }));
        app.on_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::NONE));
        for ch in "unloadothers".chars() {
            app.on_key(key(ch));
        }
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.tabs[0].body, Body::Loaded { .. }));
        assert!(
            app.tabs
                .iter()
                .skip(1)
                .all(|tab| matches!(tab.body, Body::Unloaded))
        );
    }

    #[test]
    fn empty_page_says_so_and_find_jumps() {
        let blocker = crate::block::Blocker::from_rules("");
        let mut app = blank();
        app.finish(
            0,
            0,
            Ok(crate::load::Fetched {
                final_url: "https://example.com/".into(),
                html: "<div id=\"app\"></div>".into(),
                blocked: 0,
            }),
            &blocker,
        );
        assert!(app.status_line(false).contains("no text content"));

        app.finish(
            0,
            0,
            Ok(crate::load::Fetched {
                final_url: "https://example.com/".into(),
                html: "<p>alpha beta</p><p>gamma</p>".into(),
                blocked: 0,
            }),
            &blocker,
        );
        app.on_key(key('/'));
        for ch in "gamma".chars() {
            app.on_key(key(ch));
        }
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.viewport_lines().iter().any(|line| line.current_match));
    }

    #[test]
    fn submit_queues_a_post() {
        let blocker = crate::block::Blocker::from_rules("");
        let mut app = blank();
        app.finish(
            0,
            0,
            Ok(crate::load::Fetched {
                final_url: "https://example.com/".into(),
                html: "<form method=\"post\" action=\"/login\"><input name=\"user\" value=\"a\"><input type=\"submit\" value=\"go\"></form>".into(),
                blocked: 0,
            }),
            &blocker,
        );
        if app.status_line(false).contains("Enter edit") {
            app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        }
        if !app.status_line(false).contains("Enter submit") {
            app.on_key(key('j'));
        }
        assert!(app.status_line(false).contains("Enter submit"));
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.jobs.len(), 1);
        assert!(app.jobs[0].post, "{:?}", app.jobs[0].url);
        assert!(app.jobs[0].body.as_deref().unwrap_or("").contains("user=a"));
    }

    #[test]
    fn follow_back_and_reload_queue_the_right_url() {
        let blocker = crate::block::Blocker::from_rules("");
        let mut app = blank();
        app.finish(
            0,
            0,
            Ok(crate::load::Fetched {
                final_url: "https://example.com/".into(),
                html: "<p><a href=\"/next\">next</a></p>".into(),
                blocked: 0,
            }),
            &blocker,
        );
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.jobs.last().unwrap().url, "https://example.com/next");
        app.jobs.clear();
        app.on_key(key('b'));
        assert_eq!(app.jobs.last().unwrap().url, "https://example.com/");
        app.jobs.clear();
        app.on_key(key('r'));
        assert_eq!(app.jobs.last().unwrap().url, "https://example.com/");
    }

    #[test]
    fn lists_command_opens_the_overlay() {
        let mut app = blank();
        app.on_key(key(':'));
        for ch in "lists".chars() {
            app.on_key(key(ch));
        }
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.overlay, Some(Overlay::Lists));
    }

    #[test]
    fn help_command_lists_the_keys() {
        let mut app = blank();
        app.on_key(key('?'));
        assert_eq!(app.overlay, Some(Overlay::Help));
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.overlay, None);
        app.on_key(key(':'));
        for ch in "help".chars() {
            app.on_key(key(ch));
        }
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.overlay, Some(Overlay::Help));
        let text = help_lines().join("\n");
        assert!(text.contains("Enter"));
        assert!(text.contains(":help"));
    }
}
