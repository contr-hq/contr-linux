//! What the Images page shows: images split into used and unused, and the progress of pulls.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

use contr_sdk::api::containers::v1::Container;
use contr_sdk::api::images::v1::{Image, LayerProgress};
use contr_ui_common::format;

/// Images listed one by one before "Show more".
pub(crate) const SHOWN: usize = 4;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Sort {
    #[default]
    Name,
    Size,
    Newest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct User {
    pub(crate) id: String,
    pub(crate) name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ImageView {
    pub(crate) id: String,
    /// The tag Run and Remove act on; the short ID for an untagged image.
    pub(crate) reference: String,
    pub(crate) repository: String,
    /// `:tag`, shown dimmed after the repository.
    pub(crate) tag: String,
    pub(crate) size: u64,
    pub(crate) created: Option<SystemTime>,
    pub(crate) users: Vec<User>,
}

impl ImageView {
    /// `435 MB · 1 month ago`, without the users, which the row links.
    pub(crate) fn summary(&self, now: SystemTime) -> String {
        match self.created {
            Some(created) => format!("{} · {}", format::bytes(self.size), format::ago(created, now)),
            None => format::bytes(self.size),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Sections {
    pub(crate) used: Vec<ImageView>,
    pub(crate) unused: Vec<ImageView>,
    pub(crate) untagged: usize,
    pub(crate) untagged_size: u64,
    pub(crate) unused_size: u64,
}

/// `postgres:17.2` as (`postgres`, `:17.2`); the tag is what follows the last colon after the last slash.
pub(crate) fn split_reference(reference: &str) -> (String, String) {
    let slash = reference.rfind('/').map_or(0, |index| index + 1);
    match reference[slash..].rfind(':') {
        Some(colon) => {
            let at = slash + colon;
            (reference[..at].to_owned(), reference[at..].to_owned())
        }
        None => (reference.to_owned(), String::new()),
    }
}

fn tags(image: &Image) -> Vec<&str> {
    image
        .repo_tags
        .iter()
        .map(String::as_str)
        .filter(|tag| *tag != "<none>:<none>")
        .collect()
}

pub(crate) fn sections(
    images: &BTreeMap<String, Image>,
    containers: &BTreeMap<String, Container>,
    created: impl Fn(&Image) -> Option<SystemTime>,
    query: &str,
    sort: Sort,
) -> Sections {
    let query = query.trim().to_lowercase();
    let mut sections = Sections::default();
    for image in images.values() {
        let mut users: Vec<User> = containers
            .values()
            .filter(|container| container.image_id == image.id)
            .map(|container| User {
                id: container.id.clone(),
                name: container
                    .labels
                    .get("com.docker.compose.service")
                    .cloned()
                    .or_else(|| container.names.first().cloned())
                    .unwrap_or_default(),
            })
            .collect();
        users.sort_by(|a, b| a.name.cmp(&b.name));
        let tags = tags(image);
        if tags.is_empty() && users.is_empty() {
            sections.untagged += 1;
            sections.untagged_size += image.size_bytes;
            sections.unused_size += image.size_bytes;
            continue;
        }
        let reference = tags
            .first()
            .map_or_else(|| format::short_id(&image.id).to_owned(), |tag| (*tag).to_owned());
        if !query.is_empty() && !tags.iter().any(|tag| tag.to_lowercase().contains(&query)) {
            continue;
        }
        let (repository, tag) = if tags.is_empty() {
            (reference.clone(), String::new())
        } else {
            split_reference(&reference)
        };
        let view = ImageView {
            id: image.id.clone(),
            reference,
            repository,
            tag,
            size: image.size_bytes,
            created: created(image),
            users,
        };
        if view.users.is_empty() {
            sections.unused_size += view.size;
            sections.unused.push(view);
        } else {
            sections.used.push(view);
        }
    }
    for list in [&mut sections.used, &mut sections.unused] {
        match sort {
            Sort::Name => list.sort_by(|a, b| a.reference.cmp(&b.reference)),
            Sort::Size => list.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.reference.cmp(&b.reference))),
            Sort::Newest => list.sort_by(|a, b| b.created.cmp(&a.created).then_with(|| a.reference.cmp(&b.reference))),
        }
    }
    sections
}

/// How a layer of a pull is doing, as one line of the pull card.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LayerView {
    pub(crate) id: String,
    pub(crate) fraction: f64,
    pub(crate) text: String,
    pub(crate) done: bool,
}

fn downloaded(layer: &LayerProgress) -> u64 {
    match layer.status.as_str() {
        "Downloading" => layer.current_bytes,
        "Verifying Checksum" | "Download complete" | "Extracting" | "Pull complete" => layer.total_bytes,
        _ => 0,
    }
}

pub(crate) fn layer_view(layer: &LayerProgress) -> LayerView {
    #[allow(clippy::cast_precision_loss)]
    let ratio = |current: u64, total: u64| if total == 0 { 0.0 } else { current as f64 / total as f64 };
    let id = format::short_id(&layer.layer_id).to_owned();
    let (fraction, text, done) = match layer.status.as_str() {
        "Pull complete" if layer.total_bytes > 0 => (1.0, format!("Done · {}", format::bytes(layer.total_bytes)), true),
        "Pull complete" => (1.0, "Done".to_owned(), true),
        "Already exists" => (1.0, "Already here".to_owned(), true),
        "Extracting" if layer.total_bytes > 0 => {
            let fraction = ratio(layer.current_bytes, layer.total_bytes);
            (fraction, format!("Extracting · {:.0}%", fraction * 100.0), false)
        }
        "Downloading" if layer.total_bytes > 0 => (
            ratio(layer.current_bytes, layer.total_bytes),
            format::bytes_of(layer.current_bytes, layer.total_bytes),
            false,
        ),
        "Verifying Checksum" | "Download complete" => (1.0, "Downloaded".to_owned(), false),
        "Waiting" | "Pulling fs layer" | "" => (0.0, "Waiting".to_owned(), false),
        other => (0.0, other.to_owned(), false),
    };
    LayerView {
        id,
        fraction: fraction.clamp(0.0, 1.0),
        text,
        done,
    }
}

/// The overall fraction and `87 of 141 MB · about 20 seconds left`, from the bytes downloaded since `elapsed` began.
pub(crate) fn pull_summary(layers: &[LayerProgress], elapsed: Duration) -> (f64, String) {
    let counted: Vec<&LayerProgress> = layers
        .iter()
        .filter(|layer| layer.status != "Already exists" && layer.total_bytes > 0)
        .collect();
    let total: u64 = counted.iter().map(|layer| layer.total_bytes).sum();
    let done: u64 = counted.iter().map(|layer| downloaded(layer)).sum();
    if total == 0 {
        return (0.0, "Resolving layers".to_owned());
    }
    #[allow(clippy::cast_precision_loss)]
    let fraction = done as f64 / total as f64;
    let mut text = format::bytes_of(done, total);
    let seconds = elapsed.as_secs_f64();
    if done > 0 && done < total && seconds >= 2.0 {
        #[allow(clippy::cast_precision_loss)]
        let rate = done as f64 / seconds;
        #[allow(clippy::cast_precision_loss)]
        let left = (total - done) as f64 / rate;
        if left.is_finite() {
            // The estimate is a whole number of seconds well inside u64.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let left = Duration::from_secs(left.round() as u64);
            text = format!("{text} · about {} left", format::span(left));
        }
    }
    (fraction.clamp(0.0, 1.0), text)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn image(id: &str, tags: &[&str], size: u64) -> Image {
        Image {
            id: id.to_owned(),
            repo_tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
            size_bytes: size,
            ..Image::default()
        }
    }

    #[test]
    fn splits_tags_after_registry_ports() {
        assert_eq!(
            split_reference("postgres:17.2"),
            ("postgres".to_owned(), ":17.2".to_owned())
        );
        assert_eq!(
            split_reference("localhost:5000/app:dev"),
            ("localhost:5000/app".to_owned(), ":dev".to_owned())
        );
        assert_eq!(split_reference("alpine"), ("alpine".to_owned(), String::new()));
    }

    #[test]
    fn separates_used_unused_and_untagged_images() {
        let images: BTreeMap<String, Image> = [
            image("sha256:a", &["nginx:alpine"], 100),
            image("sha256:b", &["alpine:3.22"], 10),
            image("sha256:c", &[], 5),
            image("sha256:d", &["<none>:<none>"], 7),
        ]
        .into_iter()
        .map(|image| (image.id.clone(), image))
        .collect();
        let mut labels = HashMap::new();
        labels.insert("com.docker.compose.service".to_owned(), "web".to_owned());
        let web = Container {
            id: "c1".to_owned(),
            image_id: "sha256:a".to_owned(),
            labels,
            ..Container::default()
        };
        let containers = [(web.id.clone(), web)].into_iter().collect();
        let sections = sections(&images, &containers, |_| None, "", Sort::Name);
        assert_eq!(sections.used.len(), 1);
        assert_eq!(sections.used[0].users[0].name, "web");
        assert_eq!(sections.unused.len(), 1);
        assert_eq!(sections.untagged, 2);
        assert_eq!(sections.untagged_size, 12);
        assert_eq!(sections.unused_size, 22);
    }

    #[test]
    fn layers_read_like_the_design() {
        let layer = |status: &str, current, total| LayerProgress {
            layer_id: "6e771e15690e".to_owned(),
            status: status.to_owned(),
            current_bytes: current,
            total_bytes: total,
        };
        assert_eq!(layer_view(&layer("Pull complete", 0, 3_600_000)).text, "Done · 3.6 MB");
        assert_eq!(layer_view(&layer("Extracting", 74, 100)).text, "Extracting · 74%");
        assert_eq!(
            layer_view(&layer("Downloading", 52_000_000, 128_000_000)).text,
            "52 of 128 MB"
        );
        assert_eq!(layer_view(&layer("Waiting", 0, 0)).text, "Waiting");

        let layers = [
            layer("Pull complete", 0, 13_000_000),
            layer("Downloading", 74_000_000, 128_000_000),
        ];
        let (fraction, text) = pull_summary(&layers, Duration::from_secs(10));
        assert!((fraction - 87.0 / 141.0).abs() < 1e-9);
        assert_eq!(text, "87 of 141 MB · about 6 seconds left");
    }
}
