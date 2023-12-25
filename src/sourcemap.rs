use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SourcemapNode {
    pub name: String,
    pub class_name: String,
    #[serde(default)]
    pub file_paths: Vec<PathBuf>,
    #[serde(default)]
    pub children: Vec<SourcemapNode>,
}

impl SourcemapNode {
    pub fn find_child(&self, name: String) -> Option<&SourcemapNode> {
        self.children.iter().find(|child| child.name == name)
    }
}

/// Updates all file paths in the sourcemap into canonical form, to allow matching later
pub fn mutate_sourcemap(node: &mut SourcemapNode) -> Result<()> {
    node.file_paths = node
        .file_paths
        .iter()
        .map(|path| {
            path.canonicalize()
                .context(format!("Failed to canonicalize '{}'", path.display()))
        })
        .collect::<Result<Vec<_>, _>>()?;

    for child in &mut node.children {
        mutate_sourcemap(child)?;
    }

    Ok(())
}
