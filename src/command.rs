use std::path::{Path, PathBuf};

use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use clap::Parser;
use full_moon::ast::LastStmt;
use log::error;
use log::info;
use log::warn;

use crate::link_mutator::*;
use crate::require_parser::*;
use crate::sourcemap::*;

#[derive(Parser, Debug)]
#[clap(author, version, about)]
pub struct Command {
    /// Path to sourcemap
    #[clap(short, long, value_parser)]
    pub sourcemap: PathBuf,

    /// Path to packages
    #[clap(value_parser)]
    pub packages_folder: PathBuf,
}

fn find_node(root: &SourcemapNode, path: PathBuf) -> Option<Vec<&SourcemapNode>> {
    let mut stack = vec![vec![root]];

    while let Some(node_path) = stack.pop() {
        let node = node_path.last().unwrap();
        if node.file_paths.contains(&path.to_path_buf()) {
            return Some(node_path);
        }

        for child in &node.children {
            let mut path = node_path.clone();
            path.push(child);
            stack.push(path);
        }
    }

    None
}

fn lua_files_filter(path: &&PathBuf) -> bool {
    match path.extension() {
        Some(extension) => extension == "lua" || extension == "luau",
        None => false,
    }
}

/// Given a list of components (e.g., ['script', 'Parent', 'Example']), converts it to a file path
fn file_path_from_components(
    path: &Path,
    root: &SourcemapNode,
    path_components: Vec<String>,
) -> Result<PathBuf> {
    let mut iter = path_components.iter();
    let first_in_chain = iter.next().context("No path components")?;
    assert!(first_in_chain == "script" || first_in_chain == "game");

    let mut node_path = if first_in_chain == "script" {
        find_node(root, path.canonicalize()?).context("Linker node not found in sourcemap")?
    } else {
        vec![root]
    };

    for component in iter {
        if component == "Parent" {
            node_path
                .pop()
                .context("No parent found in linked components")?;
        } else {
            node_path.push(
                node_path
                    .last()
                    .unwrap()
                    .find_child(component.to_string())
                    .with_context(|| {
                        format!(
                            "Child '{component}' not found in '{}'",
                            node_path
                                .iter()
                                .map(|node| node.name.as_str())
                                .collect::<Vec<_>>()
                                .join("/")
                        )
                    })?,
            );
        }
    }

    let current = node_path.last().unwrap();
    let file_path = current
        .file_paths
        .iter()
        .find(lua_files_filter)
        .context("No .lua/.luau file found for linked node")?
        .clone();
    info!(
        "Link require points to {} [{}] @ '{}'",
        current.name,
        current.class_name,
        file_path.display()
    );

    Ok(file_path)
}

enum MutateResult {
    Successful,
    FailedToParseReturnStmt,
}

fn mutate_thunk(path: &Path, root: &SourcemapNode) -> Result<MutateResult> {
    info!("Found link file '{}'", path.display());

    // The entry should be a thunk
    let parsed_code = full_moon::parse(&std::fs::read_to_string(path)?)?;

    if let Some(LastStmt::Return(r#return)) = parsed_code.nodes().last_stmt() {
        let returned_expression = r#return.returns().iter().next().unwrap();

        let path_components = match match_require(returned_expression) {
            Ok(components) => components,
            Err(err) => {
                warn!("Malformed link file, could not parse return expression, skipping. Run `wally install` to regenerate link files");
                error!("{:#}", err);
                return Ok(MutateResult::FailedToParseReturnStmt);
            }
        };

        info!("Path converted to: '{}'", path_components.join("/"));

        let file_path = file_path_from_components(path, root, path_components)
            .context("Could not convert require expression to file path")?;
        let pass_through_contents =
            std::fs::read_to_string(file_path).context("Failed to read linked file")?;
        let returns = r#return.returns().clone();
        let new_link_contents = mutate_link(parsed_code, returns, &pass_through_contents)
            .context("Failed to create new link contents")?;

        match new_link_contents {
            MutateLinkResult::Changed(new_ast) => std::fs::write(path, full_moon::print(&new_ast))?,
            MutateLinkResult::Unchanged => (),
        };
    } else {
        warn!("Malformed link file, no return statement found, skipping. Run `wally install` to regenerate link files");
        return Ok(MutateResult::FailedToParseReturnStmt);
    }

    Ok(MutateResult::Successful)
}

// Mutate thunk with error handled, to allow continuing
fn handled_mutate_thunk(path: &Path, root: &SourcemapNode) -> bool {
    match mutate_thunk(path, root) {
        Ok(result) => matches!(result, MutateResult::Successful),
        Err(err) => {
            error!("{:#}", err);
            false
        }
    }
}

fn handle_index_directory(path: &Path, root: &SourcemapNode) -> Result<bool> {
    let mut success = true;
    for package_entry in std::fs::read_dir(path)?.flatten() {
        for thunk in std::fs::read_dir(package_entry.path())?.flatten() {
            if thunk.file_type().unwrap().is_file() {
                success &= handled_mutate_thunk(&thunk.path(), root);
            }
        }
    }

    Ok(success)
}

impl Command {
    pub fn run(&self) -> Result<()> {
        let sourcemap_contents =
            std::fs::read_to_string(&self.sourcemap).context("Failed to read sourcemap file")?;
        let mut sourcemap: SourcemapNode =
            serde_json::from_str(&sourcemap_contents).context("Failed to parse sourcemap file")?;

        // Mutate the sourcemap so that all file paths are canonicalized for simplicity
        // And that they contain pointers to their parent
        mutate_sourcemap(&mut sourcemap)?;

        let mut success = true;
        for entry in std::fs::read_dir(&self.packages_folder)
            .context("Failed to read packages folder")?
            .flatten()
        {
            if entry.file_name() == "_Index" {
                match handle_index_directory(&entry.path(), &sourcemap) {
                    Ok(index_success) => success &= index_success,
                    Err(err) => {
                        error!("{:#}", err);
                        success = false;
                    }
                }
                continue;
            }

            success &= handled_mutate_thunk(&entry.path(), &sourcemap)
        }

        if success {
            Ok(())
        } else {
            bail!("Mutation did not complete successfully");
        }
    }
}
