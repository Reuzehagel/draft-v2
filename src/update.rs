// Best-effort update check. On startup we spawn a thread that hits the
// GitHub Releases API; the result lands in an atomic that the tray
// tooltip + settings window can read at their leisure. Network failure
// is silent — this is informational, never blocking.
//
// Set RELEASES_URL to your repo's releases endpoint when publishing. An
// empty value short-circuits the check (useful during dev).

use serde::Deserialize;
use std::sync::{Arc, Mutex};

const RELEASES_URL: &str = "";
const USER_AGENT: &str = concat!("Draft/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Clone)]
pub struct UpdateInfo {
    pub latest_version: String,
    /// Release page to open — consumed once the update UI is wired up
    /// (the whole check is dormant until RELEASES_URL is set).
    #[allow(dead_code)]
    pub url: String,
}

#[derive(Default)]
pub struct UpdateState {
    pub checked: bool,
    pub available: Option<UpdateInfo>,
    pub error: Option<String>,
}

pub type SharedUpdateState = Arc<Mutex<UpdateState>>;

pub fn spawn_check() -> SharedUpdateState {
    let state: SharedUpdateState = Arc::new(Mutex::new(UpdateState::default()));
    if RELEASES_URL.is_empty() {
        return state;
    }
    let st = state.clone();
    std::thread::spawn(move || {
        match check() {
            Ok(Some(info)) => {
                tracing::info!(
                    latest = %info.latest_version,
                    current = env!("CARGO_PKG_VERSION"),
                    "update available"
                );
                let mut s = st.lock().unwrap();
                s.checked = true;
                s.available = Some(info);
            }
            Ok(None) => {
                let mut s = st.lock().unwrap();
                s.checked = true;
            }
            Err(e) => {
                tracing::debug!(error = %e, "update check failed (silently ignored)");
                let mut s = st.lock().unwrap();
                s.checked = true;
                s.error = Some(e.to_string());
            }
        }
    });
    state
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
}

fn check() -> anyhow::Result<Option<UpdateInfo>> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent(USER_AGENT)
        .build()?;
    let resp = client.get(RELEASES_URL).send()?.error_for_status()?;
    let release: Release = resp.json()?;
    if release.prerelease || release.draft {
        return Ok(None);
    }
    let latest = release.tag_name.trim_start_matches('v').to_string();
    let current = env!("CARGO_PKG_VERSION");
    if newer_than(&latest, current) {
        Ok(Some(UpdateInfo {
            latest_version: latest,
            url: release.html_url,
        }))
    } else {
        Ok(None)
    }
}

fn newer_than(latest: &str, current: &str) -> bool {
    let parse = |s: &str| -> Vec<u32> {
        s.split('.').filter_map(|p| p.parse().ok()).collect()
    };
    let (l, c) = (parse(latest), parse(current));
    // Compare component-by-component, treating missing trailing components as
    // zero, so "1.2" and "1.2.0" compare equal and "1.3" isn't seen as newer
    // than "1.3.5". A plain Vec compare would mis-rank unequal-length versions.
    let n = l.len().max(c.len());
    for i in 0..n {
        let (lv, cv) = (l.get(i).copied().unwrap_or(0), c.get(i).copied().unwrap_or(0));
        if lv != cv {
            return lv > cv;
        }
    }
    false
}
