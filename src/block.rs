use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use adblock::Engine;
use adblock::lists::{FilterSet, ParseOptions};
use adblock::request::Request;

/// One filter list the `:lists` command can show.
#[derive(Clone, Debug)]
pub struct ListStatus {
    pub title: String,
    pub updated: Option<String>,
    pub error: Option<String>,
}

struct Source {
    title: &'static str,
    url: &'static str,
}

/// Brave's default adblock and privacy components, minus the Android-only list.
const SOURCES: &[Source] = &[
    Source {
        title: "uBlock Origin Filters",
        url: "https://raw.githubusercontent.com/uBlockOrigin/uAssets/master/filters/filters.txt",
    },
    Source {
        title: "uBlock Origin 2020 Filters",
        url: "https://raw.githubusercontent.com/uBlockOrigin/uAssets/master/filters/filters-2020.txt",
    },
    Source {
        title: "uBlock Origin 2021 Filters",
        url: "https://raw.githubusercontent.com/uBlockOrigin/uAssets/master/filters/filters-2021.txt",
    },
    Source {
        title: "uBlock Origin 2022 Filters",
        url: "https://raw.githubusercontent.com/uBlockOrigin/uAssets/master/filters/filters-2022.txt",
    },
    Source {
        title: "uBlock Origin 2023 Filters",
        url: "https://raw.githubusercontent.com/uBlockOrigin/uAssets/master/filters/filters-2023.txt",
    },
    Source {
        title: "uBlock Origin 2024 Filters",
        url: "https://raw.githubusercontent.com/uBlockOrigin/uAssets/master/filters/filters-2024.txt",
    },
    Source {
        title: "uBlock Origin 2025 Filters",
        url: "https://raw.githubusercontent.com/uBlockOrigin/uAssets/master/filters/filters-2025.txt",
    },
    Source {
        title: "uBlock Origin 2026 Filters",
        url: "https://raw.githubusercontent.com/uBlockOrigin/uAssets/master/filters/filters-2026.txt",
    },
    Source {
        title: "uBlock Origin filters - General",
        url: "https://raw.githubusercontent.com/uBlockOrigin/uAssets/master/filters/filters-general.txt",
    },
    Source {
        title: "uBlock Origin filters - Badware risks",
        url: "https://raw.githubusercontent.com/uBlockOrigin/uAssets/master/filters/badware.txt",
    },
    Source {
        title: "uBlock Origin filters - Resource abuse",
        url: "https://raw.githubusercontent.com/uBlockOrigin/uAssets/master/filters/resource-abuse.txt",
    },
    Source {
        title: "uBlock Origin filters - Unbreak",
        url: "https://raw.githubusercontent.com/uBlockOrigin/uAssets/master/filters/unbreak.txt",
    },
    Source {
        title: "uBlock Origin filters - Quick Fixes",
        url: "https://raw.githubusercontent.com/uBlockOrigin/uAssets/master/filters/quick-fixes.txt",
    },
    Source {
        title: "uBlock Origin filters - link shorteners",
        url: "https://raw.githubusercontent.com/uBlockOrigin/uAssets/master/filters/ubo-link-shorteners.txt",
    },
    Source {
        title: "EasyList",
        url: "https://easylist.to/easylist/easylist.txt",
    },
    Source {
        title: "URLhaus Malicious URL Blocklist",
        url: "https://malware-filter.gitlab.io/malware-filter/urlhaus-filter-agh-online.txt",
    },
    Source {
        title: "Brave Unbreak",
        url: "https://raw.githubusercontent.com/brave/adblock-lists/master/brave-unbreak.txt",
    },
    Source {
        title: "Brave Specific",
        url: "https://raw.githubusercontent.com/brave/adblock-lists/master/brave-lists/brave-specific.txt",
    },
    Source {
        title: "Brave Social",
        url: "https://raw.githubusercontent.com/brave/adblock-lists/master/brave-lists/brave-social.txt",
    },
    Source {
        title: "Brave Unbreak (lists)",
        url: "https://raw.githubusercontent.com/brave/adblock-lists/master/brave-lists/brave-unbreak.txt",
    },
    Source {
        title: "SugarCoat Rules",
        url: "https://raw.githubusercontent.com/brave/adblock-lists/master/brave-lists/brave-sugarcoat.txt",
    },
    Source {
        title: "EasyPrivacy",
        url: "https://easylist.to/easylist/easyprivacy.txt",
    },
    Source {
        title: "uBlock Origin filters - Privacy",
        url: "https://raw.githubusercontent.com/uBlockOrigin/uAssets/master/filters/privacy.txt",
    },
];

struct Inner {
    engine: Engine,
    lists: Vec<ListStatus>,
    updating: bool,
}

/// Brave's compiled filter engine. Checks are synchronous and short; rebuilding happens off the UI task.
pub struct Blocker {
    inner: Mutex<Inner>,
    dir: PathBuf,
}

impl Blocker {
    /// Load a cached engine from `dir`, or an empty engine when the cache is missing or stale.
    pub fn open(dir: PathBuf) -> (Self, bool) {
        let _ = fs::create_dir_all(&dir);
        let mut engine = Engine::default();
        let mut loaded = false;
        if let Ok(bytes) = fs::read(dir.join("engine.dat"))
            && engine.deserialize(&bytes).is_ok()
        {
            loaded = true;
        }
        let lists = read_meta(&dir).unwrap_or_else(|| placeholder_lists(loaded));
        let blocker = Self {
            inner: Mutex::new(Inner {
                engine,
                lists,
                updating: false,
            }),
            dir,
        };
        (blocker, loaded)
    }

    #[cfg(test)]
    pub fn from_rules(rules: &str) -> Self {
        let engine = compile(&[rules.to_string()]);
        Self {
            inner: Mutex::new(Inner {
                engine,
                lists: Vec::new(),
                updating: false,
            }),
            dir: std::env::temp_dir(),
        }
    }

    pub fn blocked(&self, url: &str, source: &str, kind: &str) -> bool {
        let source = if url::Url::parse(source).is_ok() {
            source
        } else {
            url
        };
        let Ok(request) = Request::new(url, source, kind, "GET") else {
            return false;
        };
        self.lock()
            .engine
            .check_network_request(&request)
            .should_block()
    }

    /// Hostname hide rules plus generic class and id rules that apply to this page.
    pub fn cosmetic_selectors(&self, url: &str, classes: &[String], ids: &[String]) -> Vec<String> {
        let guard = self.lock();
        let resources = guard.engine.url_cosmetic_resources(url);
        let mut out: Vec<String> = resources.hide_selectors.into_iter().collect();
        if !resources.generichide {
            out.extend(
                guard
                    .engine
                    .hidden_class_id_selectors(classes, ids, &resources.exceptions),
            );
        }
        out
    }

    pub fn lists(&self) -> Vec<ListStatus> {
        self.lock().lists.clone()
    }

    pub fn is_updating(&self) -> bool {
        self.lock().updating
    }

    /// Download the Brave lists, compile them, and replace the engine.
    /// A list that fails to download is skipped. The previous engine stays if nothing downloads.
    pub async fn update(&self, client: &reqwest::Client) -> Result<(), String> {
        {
            let mut guard = self.lock();
            if guard.updating {
                return Ok(());
            }
            guard.updating = true;
        }
        let dir = self.dir.clone();
        let result = download_and_compile(client, &dir).await;
        match result {
            Ok((engine, lists)) => {
                let mut guard = self.lock();
                guard.engine = engine;
                guard.lists = lists;
                guard.updating = false;
                Ok(())
            }
            Err(err) => {
                self.lock().updating = false;
                Err(err)
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

fn compile(texts: &[String]) -> Engine {
    let mut set = FilterSet::new(false);
    for text in texts {
        set.add_filter_list(text.clone(), ParseOptions::default());
    }
    Engine::new_with_filter_set(set)
}

async fn download_and_compile(
    client: &reqwest::Client,
    dir: &Path,
) -> Result<(Engine, Vec<ListStatus>), String> {
    let mut jobs = tokio::task::JoinSet::new();
    for (index, source) in SOURCES.iter().enumerate() {
        let client = client.clone();
        let url = source.url;
        let title = source.title;
        jobs.spawn(async move {
            let result = client.get(url).send().await;
            (index, title, result)
        });
    }
    let mut downloaded = Vec::new();
    let mut statuses = vec![
        ListStatus {
            title: String::new(),
            updated: None,
            error: None
        };
        SOURCES.len()
    ];
    while let Some(joined) = jobs.join_next().await {
        let (index, title, result) = joined.map_err(|err| err.to_string())?;
        let status = &mut statuses[index];
        status.title = title.to_string();
        match result {
            Ok(response) if response.status().is_success() => match response.text().await {
                Ok(body) => {
                    downloaded.push(body);
                    status.updated = Some(stamp());
                }
                Err(err) => status.error = Some(err.to_string()),
            },
            Ok(response) => status.error = Some(format!("http {}", response.status())),
            Err(err) => status.error = Some(err.to_string()),
        }
    }
    if downloaded.is_empty() {
        return Err("no filter lists downloaded".into());
    }
    let dir = dir.to_path_buf();
    let statuses_for_disk = statuses.clone();
    let engine = tokio::task::spawn_blocking(move || {
        let engine = compile(&downloaded);
        if let Ok(()) = fs::create_dir_all(&dir) {
            let _ = fs::write(dir.join("engine.dat"), engine.serialize());
            let _ = fs::write(dir.join("lists.txt"), meta_text(&statuses_for_disk));
        }
        engine
    })
    .await
    .map_err(|err| err.to_string())?;
    Ok((engine, statuses))
}

fn placeholder_lists(cached: bool) -> Vec<ListStatus> {
    SOURCES
        .iter()
        .map(|source| ListStatus {
            title: source.title.to_string(),
            updated: cached.then(|| "cached".into()),
            error: None,
        })
        .collect()
}

fn meta_text(lists: &[ListStatus]) -> String {
    let mut out = String::new();
    for list in lists {
        out.push_str(&list.title.replace(['\t', '\n'], " "));
        out.push('\t');
        out.push_str(list.updated.as_deref().unwrap_or("-"));
        out.push('\t');
        out.push_str(
            &list
                .error
                .as_deref()
                .unwrap_or("-")
                .replace(['\t', '\n'], " "),
        );
        out.push('\n');
    }
    out
}

fn read_meta(dir: &Path) -> Option<Vec<ListStatus>> {
    let text = fs::read_to_string(dir.join("lists.txt")).ok()?;
    let mut lists = Vec::new();
    for line in text.lines() {
        let mut parts = line.splitn(3, '\t');
        let title = parts.next().unwrap_or("").to_string();
        if title.is_empty() {
            continue;
        }
        let updated = parts
            .next()
            .filter(|value| *value != "-")
            .map(str::to_string);
        let error = parts
            .next()
            .filter(|value| *value != "-")
            .map(str::to_string);
        lists.push(ListStatus {
            title,
            updated,
            error,
        });
    }
    (!lists.is_empty()).then_some(lists)
}

fn stamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    ymd(secs)
}

/// Days since 1970-01-01 to a UTC calendar date.
fn ymd(secs: u64) -> String {
    let z = (secs / 86_400) as i64;
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_network_and_keeps_a_normal_request() {
        let blocker = Blocker::from_rules("||evil.test^\nexample.com##.ad\n");
        assert!(blocker.blocked(
            "https://evil.test/ad.js",
            "https://example.com/",
            "document"
        ));
        assert!(!blocker.blocked("https://example.com/", "https://example.com/", "document"));
    }

    #[test]
    fn cosmetic_selector_includes_the_class_rule() {
        let blocker = Blocker::from_rules("example.com##.ad\n");
        let selectors = blocker.cosmetic_selectors("https://example.com/page", &[], &[]);
        assert!(
            selectors.iter().any(|selector| selector.contains(".ad")),
            "{selectors:?}"
        );
    }

    #[test]
    fn dat_roundtrip() {
        let blocker = Blocker::from_rules("||evil.test^\n");
        let bytes = blocker.lock().engine.serialize();
        let mut restored = Engine::default();
        restored.deserialize(&bytes).unwrap();
        let request = Request::new(
            "https://evil.test/x",
            "https://example.com/",
            "document",
            "GET",
        )
        .unwrap();
        assert!(restored.check_network_request(&request).should_block());
    }

    #[test]
    fn known_unix_day() {
        assert_eq!(ymd(1_700_000_000), "2023-11-14");
    }

    #[test]
    fn cached_engine_and_list_titles_load() {
        let dir = std::env::temp_dir().join(format!("holonet-cache-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let engine = compile(&["||evil.test^\n".into()]);
        fs::write(dir.join("engine.dat"), engine.serialize()).unwrap();
        fs::write(
            dir.join("lists.txt"),
            "EasyList\t2026-01-02\t-\nEasyPrivacy\t-\thttp 404\n",
        )
        .unwrap();
        let (opened, cached) = Blocker::open(dir.clone());
        assert!(cached);
        let lists = opened.lists();
        assert_eq!(lists[0].title, "EasyList");
        assert_eq!(lists[0].updated.as_deref(), Some("2026-01-02"));
        assert_eq!(lists[1].error.as_deref(), Some("http 404"));
        assert!(opened.blocked("https://evil.test/x", "https://example.com/", "document"));
        let _ = fs::remove_dir_all(dir);
    }
}
