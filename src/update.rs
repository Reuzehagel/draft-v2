// Best-effort update check. On startup we spawn a thread that hits the
// GitHub Releases API; if (and only if) a newer release exists it sends one
// message down a channel the event loop already drains, and the tray tooltip
// picks it up. Network failure is silent — this is informational, never
// blocking, and nothing is sent when we're already current.
//
// Set RELEASES_URL to your repo's releases endpoint when publishing. An
// empty value short-circuits the check (useful during dev) — the channel is
// then simply never written to, so the tooltip stays silent about updates.

use serde::Deserialize;

const RELEASES_URL: &str = "";
const USER_AGENT: &str = concat!("Draft/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Clone)]
pub struct UpdateInfo {
    pub latest_version: String,
    /// Release page to open — consumed once the update UI is wired up
    /// (the tooltip only names the version).
    #[allow(dead_code)]
    pub url: String,
}

/// Start the check. The returned receiver yields at most one `UpdateInfo`, and
/// only when a newer release exists; every other outcome (dormant, offline,
/// already current) leaves it empty forever.
pub fn spawn_check(waker: crate::wake::Waker) -> crossbeam_channel::Receiver<UpdateInfo> {
    let (tx, rx) = crossbeam_channel::bounded(1);
    if RELEASES_URL.is_empty() {
        return rx;
    }
    std::thread::spawn(move || match check() {
        Ok(Some(info)) => {
            tracing::info!(
                latest = %info.latest_version,
                current = env!("CARGO_PKG_VERSION"),
                "update available"
            );
            // A failed send only means the app is shutting down.
            let _ = tx.send(info);
            // The loop is asleep with no timer armed, so the tooltip would
            // otherwise name the new version at whatever the user next did.
            waker.wake();
        }
        Ok(None) => tracing::debug!("update check: already on the latest release"),
        Err(e) => tracing::debug!(error = %e, "update check failed (silently ignored)"),
    });
    rx
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
    let parse = |s: &str| -> Vec<u32> { s.split('.').filter_map(|p| p.parse().ok()).collect() };
    let (l, c) = (parse(latest), parse(current));
    // Compare component-by-component, treating missing trailing components as
    // zero, so "1.2" and "1.2.0" compare equal and "1.3" isn't seen as newer
    // than "1.3.5". A plain Vec compare would mis-rank unequal-length versions.
    let n = l.len().max(c.len());
    for i in 0..n {
        let (lv, cv) = (
            l.get(i).copied().unwrap_or(0),
            c.get(i).copied().unwrap_or(0),
        );
        if lv != cv {
            return lv > cv;
        }
    }
    false
}
