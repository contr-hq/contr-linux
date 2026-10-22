//! The update notice: whether the daemon found a newer Contr, and what Preferences says about the latest check.

use std::time::SystemTime;

use contr_core::product;
use contr_daemon_ui::system_time;
use contr_sdk::api::updates::v1::UpdateStatus;
use contr_ui_common::format;

/// A newer release the banner announces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Notice {
    pub(crate) version: String,
    pub(crate) url: String,
}

/// The release to announce: none while `check` is off or after the user opened this version's release page.
pub(crate) fn notice(status: &UpdateStatus, check: bool, dismissed: Option<&str>) -> Option<Notice> {
    let release = status.available.as_ref().filter(|_| check)?;
    if release.version.is_empty() || dismissed == Some(release.version.as_str()) {
        return None;
    }
    Some(Notice {
        version: release.version.clone(),
        url: release.url.clone(),
    })
}

/// `Contr 0.2.0 is available · checked 2 hours ago`, or why the latest check failed.
pub(crate) fn last_check(status: &UpdateStatus, now: SystemTime) -> String {
    if !status.error.is_empty() {
        return format!("The latest check failed: {}", status.error);
    }
    let Some(checked) = system_time(status.checked_at.as_ref()) else {
        return "Not checked yet".to_owned();
    };
    let checked = format::ago(checked, now);
    match &status.available {
        Some(release) => format!(
            "{} {} is available · checked {checked}",
            product::DISPLAY_NAME,
            release.version
        ),
        None => format!(
            "{} {} is the latest · checked {checked}",
            product::DISPLAY_NAME,
            status.current_version
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use contr_sdk::api::updates::v1::Release;
    use prost_types::Timestamp;

    use super::*;

    fn status(available: Option<&str>, error: &str) -> UpdateStatus {
        UpdateStatus {
            current_version: "0.1.0".to_owned(),
            available: available.map(|version| Release {
                version: version.to_owned(),
                url: format!("https://example.com/{version}"),
                ..Release::default()
            }),
            checked_at: Some(Timestamp {
                seconds: 1_000,
                nanos: 0,
            }),
            error: error.to_owned(),
        }
    }

    #[test]
    fn the_banner_follows_the_setting_and_a_dismissed_version() {
        let found = status(Some("0.2.0"), "");
        assert_eq!(
            notice(&found, true, None).map(|notice| notice.version).as_deref(),
            Some("0.2.0")
        );
        assert_eq!(notice(&found, false, None), None);
        assert_eq!(notice(&found, true, Some("0.2.0")), None);
        assert_eq!(notice(&status(None, ""), true, None), None);
    }

    #[test]
    fn the_latest_check_reads_plainly() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000 + 7_200);
        assert_eq!(
            last_check(&status(None, ""), now),
            format!("{} 0.1.0 is the latest · checked 2 hours ago", product::DISPLAY_NAME)
        );
        assert_eq!(
            last_check(&status(Some("0.2.0"), "rate limited"), now),
            "The latest check failed: rate limited"
        );
        let never = UpdateStatus {
            checked_at: None,
            ..status(None, "")
        };
        assert_eq!(last_check(&never, now), "Not checked yet");
    }
}
