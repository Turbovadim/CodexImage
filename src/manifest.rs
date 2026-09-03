use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FinalOutput {
    pub path: String,
    pub label: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OutputManifest {
    pub summary: String,
    pub complete: bool,
    pub outputs: Vec<FinalOutput>,
}

pub fn schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "summary": {"type":"string", "maxLength":500},
            "complete": {"type":"boolean"},
            "outputs": {
                "type":"array",
                "items": {
                    "type":"object",
                    "properties": {
                        "path": {"type":"string", "minLength":1},
                        "label": {"type":"string", "minLength":1, "maxLength":120}
                    },
                    "required":["path", "label"],
                    "additionalProperties":false
                }
            }
        },
        "required":["summary", "complete", "outputs"],
        "additionalProperties":false
    })
}

pub fn parse(text: &str) -> Result<OutputManifest> {
    let mut manifest: OutputManifest =
        serde_json::from_str(text).context("the final response was not a valid output manifest")?;
    manifest.summary = manifest.summary.trim().to_owned();
    if manifest.summary.chars().count() > 500 {
        bail!("the final summary was too long");
    }
    let mut paths = HashSet::new();
    for output in &mut manifest.outputs {
        output.path = output.path.trim().to_owned();
        output.label = output.label.trim().to_owned();
        if output.path.is_empty() || output.label.is_empty() {
            bail!("an output had an empty path or label");
        }
        if output.label.chars().count() > 120 {
            bail!("an output label was too long");
        }
        if !paths.insert(output.path.clone()) {
            bail!("the same generated image was selected more than once");
        }
    }
    if manifest.complete && manifest.outputs.is_empty() {
        bail!("a complete manifest did not select any outputs");
    }
    Ok(manifest)
}

pub fn absolute_file_path(value: &str) -> Result<PathBuf> {
    let path = if let Some(raw) = value.strip_prefix("file://") {
        file_url_path(percent_decode(raw)?)
    } else {
        PathBuf::from(value)
    };
    if !path.is_absolute() {
        bail!("a selected image path was not absolute");
    }
    Ok(normalize(&path))
}

#[cfg(not(target_os = "windows"))]
fn file_url_path(value: String) -> PathBuf {
    PathBuf::from(value)
}

#[cfg(target_os = "windows")]
fn file_url_path(mut value: String) -> PathBuf {
    let bytes = value.as_bytes();
    if bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':' {
        value.remove(0);
    } else if !value.starts_with('/') && !matches!(bytes.get(1), Some(b':')) {
        // A host in file://server/share denotes a UNC path.
        value.insert_str(0, "//");
    }
    PathBuf::from(value)
}

pub fn is_path_inside(root: &Path, candidate: &Path) -> bool {
    let (Ok(root), Ok(candidate)) = (root.canonicalize(), candidate.canonicalize()) else {
        return false;
    };
    is_canonical_path_inside(&root, &candidate)
}

/// Checks two paths that the caller has already canonicalized. Generation
/// polling uses this to avoid repeating filesystem lookups for every image.
pub(crate) fn is_canonical_path_inside(root: &Path, candidate: &Path) -> bool {
    candidate != root && candidate.starts_with(root)
}

fn normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn percent_decode(value: &str) -> Result<String> {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                bail!("a selected image used an invalid file URL");
            }
            let pair = std::str::from_utf8(&bytes[index + 1..index + 3])?;
            output.push(u8::from_str_radix(pair, 16).context("invalid file URL escape")?);
            index += 3;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(output).context("file URL was not UTF-8")
}

#[cfg(test)]
mod tests {
    use super::absolute_file_path;
    use std::path::Path;

    #[test]
    fn percent_encoded_absolute_file_urls_are_accepted() {
        #[cfg(not(target_os = "windows"))]
        assert_eq!(
            absolute_file_path("file:///tmp/work%20space/image.png").unwrap(),
            Path::new("/tmp/work space/image.png")
        );

        #[cfg(target_os = "windows")]
        assert_eq!(
            absolute_file_path("file:///C:/work%20space/image.png").unwrap(),
            Path::new(r"C:\work space\image.png")
        );
    }
}
