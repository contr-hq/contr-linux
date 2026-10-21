use contr_sdk::api::settings::v1::{GetSettingsRequest, Settings, SettingsState, UpdateSettingsRequest};
use contr_sdk::{Client, Error};

/// A field mask naming `paths`, such as `engine.start_with_daemon`.
pub fn field_mask(paths: &[&str]) -> prost_types::FieldMask {
    prost_types::FieldMask {
        paths: paths.iter().map(|path| (*path).to_owned()).collect(),
    }
}

/// Reads and changes the daemon's settings for the calling user.
#[derive(Clone, Debug)]
pub struct SettingsHandle {
    client: Client,
}

impl SettingsHandle {
    pub fn new(client: Client) -> Self {
        Self { client }
    }

    pub async fn get(&self) -> Result<SettingsState, Error> {
        let response = self.client.settings().get_settings(GetSettingsRequest {}).await?;
        response
            .into_inner()
            .state
            .ok_or(Error::Protocol("settings without a state"))
    }

    /// Changes the fields `paths` names to their values in `settings`; fails with `SettingLocked` for a field an
    /// administrator fixes.
    pub async fn update(&self, settings: Settings, paths: &[&str]) -> Result<SettingsState, Error> {
        let response = self
            .client
            .settings()
            .update_settings(UpdateSettingsRequest {
                settings: Some(settings),
                update_mask: Some(field_mask(paths)),
                etag: String::new(),
            })
            .await?;
        response
            .into_inner()
            .state
            .ok_or(Error::Protocol("settings without a state"))
    }
}
