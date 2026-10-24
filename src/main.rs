mod block;
mod dom;
mod layout;
mod load;
mod tabs;
mod ui;

use std::io::Result;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{Event, EventStream};
use directories::ProjectDirs;
use futures_util::StreamExt;
use tokio::sync::mpsc;

use crate::block::Blocker;
use crate::load::{CookieJar, client};
use crate::tabs::App;

enum Msg {
    Page {
        tab: usize,
        generation: u64,
        result: std::result::Result<load::Fetched, String>,
    },
    Filters(std::result::Result<(), String>),
}

#[tokio::main]
async fn main() -> Result<()> {
    let (config_dir, data_dir) = dirs();
    let _ = std::fs::create_dir_all(&config_dir);
    let _ = std::fs::create_dir_all(&data_dir);
    let (blocker, cached) = Blocker::open(data_dir);
    let blocker = Arc::new(blocker);
    let jar = CookieJar::new();
    let http = client();
    let mut app = App::new(80, config_dir.join("config"));
    if let Some(input) = std::env::args().nth(1) {
        app.open_cli(&input);
    }

    let mut terminal = ratatui::try_init()?;
    let size = terminal.size()?;
    app.set_viewport(
        size.width as usize,
        size.height.saturating_sub(3) as usize,
        &blocker,
    );
    let restore = Restore;
    let mut events = EventStream::new();
    let (tx, mut rx) = mpsc::channel(32);
    if !cached {
        spawn_update(blocker.clone(), http.clone(), tx.clone());
    }

    loop {
        let size = terminal.size()?;
        app.set_viewport(
            size.width as usize,
            size.height.saturating_sub(3) as usize,
            &blocker,
        );
        terminal.draw(|frame| ui::draw(frame, &app, &blocker))?;
        spawn_jobs(&mut app, &http, &blocker, &jar, &tx);
        if app.update && !blocker.is_updating() {
            app.update = false;
            spawn_update(blocker.clone(), http.clone(), tx.clone());
        }
        tokio::select! {
            event = events.next() => {
                match event {
                    Some(Ok(Event::Key(key))) => app.on_key(key),
                    Some(Ok(Event::Paste(text))) => app.paste(&text),
                    Some(Ok(Event::Resize(width, height))) => {
                        app.set_viewport(width as usize, height.saturating_sub(3) as usize, &blocker);
                    }
                    Some(Err(err)) => {
                        drop(restore);
                        return Err(err);
                    }
                    None => break,
                    _ => {}
                }
            }
            msg = rx.recv() => {
                match msg {
                    Some(Msg::Page { tab, generation, result }) => app.finish(tab, generation, result, &blocker),
                    Some(Msg::Filters(Ok(()))) => app.message.clear(),
                    Some(Msg::Filters(Err(err))) => app.message = err,
                    None => break,
                }
            }
            // shortcut: background tabs unload within 30s of their ttl, upgrade if the minute must be exact
            _ = tokio::time::sleep(Duration::from_secs(30)) => app.expire(),
        }
        app.expire();
        if app.quit {
            break;
        }
    }
    drop(restore);
    Ok(())
}

fn spawn_jobs(
    app: &mut App,
    http: &reqwest::Client,
    blocker: &Arc<Blocker>,
    jar: &CookieJar,
    tx: &mpsc::Sender<Msg>,
) {
    for job in app.jobs.drain(..) {
        let tx = tx.clone();
        let http = http.clone();
        let blocker = blocker.clone();
        let jar = jar.clone();
        tokio::spawn(async move {
            let result = load::fetch(
                &http,
                &blocker,
                &jar,
                &job.url,
                &job.source,
                job.post,
                job.body,
            )
            .await;
            let _ = tx
                .send(Msg::Page {
                    tab: job.tab,
                    generation: job.generation,
                    result,
                })
                .await;
        });
    }
}

fn spawn_update(blocker: Arc<Blocker>, http: reqwest::Client, tx: mpsc::Sender<Msg>) {
    tokio::spawn(async move {
        let result = blocker.update(&http).await;
        let _ = tx.send(Msg::Filters(result)).await;
    });
}

fn dirs() -> (std::path::PathBuf, std::path::PathBuf) {
    if let Some(dirs) = ProjectDirs::from("dev", "tbw", "tbw") {
        (
            dirs.config_dir().to_path_buf(),
            dirs.data_dir().to_path_buf(),
        )
    } else {
        let dir = std::env::temp_dir().join("tbw");
        (dir.clone(), dir)
    }
}

struct Restore;

impl Drop for Restore {
    fn drop(&mut self) {
        ratatui::restore();
    }
}
