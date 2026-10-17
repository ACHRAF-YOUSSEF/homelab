//! Display metadata never grants access or changes root identities. Host mount
//! sources contribute only a validated Windows drive letter, never a host path.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const MOUNTS: [(&str, &str, &str); 4] = [
    ("/movies", "Movies", "CONVERTER_MOVIES_SOURCE"),
    ("/movies-C", "Movies", "CONVERTER_MOVIES_C_SOURCE"),
    ("/tv", "TV shows", "CONVERTER_TV_SOURCE"),
    ("/tv-C", "TV shows", "CONVERTER_TV_C_SOURCE"),
];

struct MountDisplay {
    path: PathBuf,
    name: &'static str,
    drive: Option<char>,
}

pub(crate) struct RootDisplay {
    mounts: Vec<MountDisplay>,
}

fn windows_drive(source: &str) -> Option<char> {
    let mut chars = source.chars();
    let drive = chars.next()?;
    (drive.is_ascii_alphabetic()
        && chars.next() == Some(':')
        && matches!(chars.next(), Some('/' | '\\')))
    .then(|| drive.to_ascii_uppercase())
}

fn opaque_id(identity: &str) -> String {
    format!("v{:x}", Sha256::digest(identity.as_bytes()))
}

#[cfg(unix)]
fn filesystem_identity(root: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(root)
        .ok()
        .map(|metadata| format!("unix-device:{}", metadata.dev()))
}

#[cfg(not(unix))]
fn filesystem_identity(root: &Path) -> Option<String> {
    // On Windows, the absolute path's drive is evidence; arbitrary directory
    // suffixes such as movies-C are not.
    windows_drive(&root.to_string_lossy()).map(|drive| format!("windows-drive:{drive}"))
}

impl RootDisplay {
    pub(crate) fn from_env() -> Self {
        Self::from_sources(|key| std::env::var(key).ok())
    }

    fn from_sources(mut source: impl FnMut(&str) -> Option<String>) -> Self {
        Self {
            mounts: MOUNTS
                .iter()
                .map(|(path, name, key)| MountDisplay {
                    path: PathBuf::from(path),
                    name,
                    drive: source(key).as_deref().and_then(windows_drive),
                })
                .collect(),
        }
    }

    pub(crate) fn describe(&self, root: &Path, mounts: &[PathBuf], available: bool) -> Value {
        let configured = self
            .mounts
            .iter()
            .find(|mount| root.starts_with(&mount.path));
        let containing = mounts
            .iter()
            .filter(|mount| root.starts_with(mount))
            .max_by_key(|mount| mount.components().count());
        let drive = configured.and_then(|mount| mount.drive);
        let identity = drive
            .map(|drive| format!("windows-drive:{drive}"))
            .or_else(|| available.then(|| filesystem_identity(root)).flatten())
            .unwrap_or_else(|| {
                format!(
                    "unknown-mount:{}",
                    containing.map_or(root, PathBuf::as_path).display()
                )
            });
        let volume_id = opaque_id(&identity);
        let volume_label =
            drive.map_or_else(|| "Storage volume".into(), |drive| format!("{drive}:"));
        let name = configured
            .filter(|mount| mount.path == root)
            .map(|mount| mount.name.to_owned())
            .unwrap_or_else(|| {
                root.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Media".into())
            });
        let label = match drive {
            Some(drive) => format!("{name} ({drive}:)"),
            None => name,
        };
        json!({"label":label,"volume_id":volume_id,"volume_label":volume_label})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_labels_require_explicit_source_and_never_expose_host_paths() {
        let display = RootDisplay::from_sources(|key| match key {
            "CONVERTER_MOVIES_SOURCE" => Some("d:/private/films".into()),
            "CONVERTER_TV_C_SOURCE" => Some("C:\\private\\shows".into()),
            _ => None,
        });
        let mounts = [PathBuf::from("/movies"), PathBuf::from("/tv-C")];
        let movies = display.describe(Path::new("/movies"), &mounts, false);
        let shows = display.describe(Path::new("/tv-C"), &mounts, false);
        assert_eq!(movies["label"], "Movies (D:)");
        assert_eq!(shows["label"], "TV shows (C:)");
        assert_ne!(movies["volume_id"], shows["volume_id"]);
        assert!(!movies.to_string().contains("private"));
        assert_eq!(
            display.describe(Path::new("/movies/Archive"), &mounts, false)["label"],
            "Archive (D:)"
        );
    }

    #[test]
    fn same_configured_drive_groups_media_without_using_directory_suffixes() {
        let display = RootDisplay::from_sources(|key| match key {
            "CONVERTER_MOVIES_SOURCE" | "CONVERTER_TV_SOURCE" => Some("C:/media".into()),
            "CONVERTER_MOVIES_C_SOURCE" => Some("/srv/media".into()),
            _ => None,
        });
        let movies = display.describe(Path::new("/movies"), &[], false);
        let shows = display.describe(Path::new("/tv"), &[], false);
        let generic = display.describe(Path::new("/movies-C"), &[], false);
        assert_eq!(movies["volume_id"], shows["volume_id"]);
        assert_ne!(movies["volume_id"], generic["volume_id"]);
        assert_ne!(generic["volume_label"], "C:");
        for invalid in ["C:relative", "/c/media", "movies-C", "https://server/path"] {
            assert_eq!(windows_drive(invalid), None);
        }
    }

    #[test]
    fn directories_share_actual_filesystem_identity_with_generic_labels() {
        let fixture = tempfile::tempdir().unwrap();
        let movies = fixture.path().join("movies-C");
        let shows = fixture.path().join("shows");
        std::fs::create_dir(&movies).unwrap();
        std::fs::create_dir(&shows).unwrap();
        let display = RootDisplay::from_sources(|_| None);
        let mounts = [movies.clone(), shows.clone()];
        let left = display.describe(&movies, &mounts, true);
        let right = display.describe(&shows, &mounts, true);
        assert_eq!(left["volume_id"], right["volume_id"]);
        assert_eq!(left["volume_label"], "Storage volume");
        assert_eq!(left["label"], "movies-C");
        assert_ne!(left["volume_label"], "C:");
    }

    #[test]
    fn unknown_volumes_remain_separate_without_filesystem_evidence() {
        let display = RootDisplay::from_sources(|_| None);
        let mounts = [PathBuf::from("/movies"), PathBuf::from("/movies-C")];
        let first = display.describe(&mounts[0], &mounts, false);
        let second = display.describe(&mounts[1], &mounts, false);
        assert_ne!(first["volume_id"], second["volume_id"]);
        assert_eq!(
            display.describe(Path::new("/movies/Archive"), &mounts, false)["volume_id"],
            first["volume_id"]
        );
    }
}
