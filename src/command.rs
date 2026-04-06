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
    pub packages_folders: Vec<PathBuf>,
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

    if !(first_in_chain == "script" || first_in_chain == "game") {
        bail!("require expression does not start with 'script' or 'game', cannot determine starting point");
    }

    let mut node_path = if first_in_chain == "script" {
        find_node(root, path.canonicalize()?)
            .with_context(|| format!("Linker node '{}' not found in sourcemap", path.display()))?
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
    let parsed_code = match full_moon::parse(&std::fs::read_to_string(path)?) {
        Ok(parsed_code) => parsed_code,
        Err(errors) => bail!(errors
            .iter()
            .map(|err| err.to_string())
            .collect::<Vec<_>>()
            .join("\n")),
    };

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

        info!(
            "Require expression converted to path: '{}'",
            path_components.join("/")
        );

        let file_path = file_path_from_components(path, root, path_components)
            .context("Could not convert require expression to file path")?;
        let pass_through_contents =
            std::fs::read_to_string(file_path).context("Failed to read linked file")?;
        let returns = r#return.returns().clone();
        let new_link_contents = mutate_link(parsed_code, returns, &pass_through_contents)
            .context("Failed to create new link contents")?;

        match new_link_contents {
            MutateLinkResult::Changed(new_ast) => {
                info!("Exported types found, writing new linker file");
                std::fs::write(path, new_ast.to_string())?
            }
            MutateLinkResult::Unchanged => {
                info!("No exported types, leaving unchanged");
            }
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
        if !package_entry
            .file_type()
            .map(|t| t.is_dir())
            .unwrap_or(false)
        {
            continue;
        }
        for thunk in std::fs::read_dir(package_entry.path())?.flatten() {
            if thunk.file_type().unwrap().is_file() {
                success &= handled_mutate_thunk(&thunk.path(), root);
            }
        }
    }

    Ok(success)
}

fn handle_packages_folder(path: &Path, sourcemap: &SourcemapNode) -> Result<bool> {
    let mut success = true;

    for entry in std::fs::read_dir(path)
        .context("Failed to read packages folder")?
        .flatten()
    {
        if entry.file_name() == "_Index" {
            match handle_index_directory(&entry.path(), sourcemap) {
                Ok(index_success) => success &= index_success,
                Err(err) => {
                    error!("{:#}", err);
                    success = false;
                }
            }
            continue;
        }

        success &= handled_mutate_thunk(&entry.path(), sourcemap)
    }

    Ok(success)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Creates the full directory structure for an integration test.
    /// Returns the temp directory path.
    fn setup_transitive_dep_test() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "wally-package-types-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        // Create directory structure mirroring what wally install creates:
        //
        // Packages/
        //   DirectDep.lua          <- root thunk for direct dep A
        //   _Index/
        //     scope_a@1.0.0/       <- package A's versioned directory
        //       TransitiveDep.lua  <- thunk for A's sub-dep B (this is what the bug is about)
        //       a_name/
        //         init.lua         <- A's actual code
        //     scope_b@1.0.0/       <- package B's versioned directory
        //       b_name/
        //         init.lua         <- B's actual code with exported types

        let index_dir = dir.join("Packages/_Index");
        let pkg_a_dir = index_dir.join("scope_a@1.0.0");
        let pkg_a_content_dir = pkg_a_dir.join("a_name");
        let pkg_b_content_dir = index_dir.join("scope_b@1.0.0/b_name");

        fs::create_dir_all(&pkg_a_content_dir).unwrap();
        fs::create_dir_all(&pkg_b_content_dir).unwrap();

        // Root link file for direct dep A
        fs::write(
            dir.join("Packages/DirectDep.lua"),
            "return require(script.Parent._Index[\"scope_a@1.0.0\"][\"a_name\"])\n",
        )
        .unwrap();

        // Transitive dep thunk: A's link to B (inside _Index/scope_a@1.0.0/)
        // This uses link_sibling_same_index pattern: script.Parent.Parent[...][...]
        fs::write(
            pkg_a_dir.join("TransitiveDep.lua"),
            "return require(script.Parent.Parent[\"scope_b@1.0.0\"][\"b_name\"])\n",
        )
        .unwrap();

        // A's actual code - exports a type that references B
        fs::write(
            pkg_a_content_dir.join("init.lua"),
            "export type Foo = string\nreturn {}\n",
        )
        .unwrap();

        // B's actual code - exports types that should propagate through the chain
        fs::write(
            pkg_b_content_dir.join("init.lua"),
            "export type Bar = number\nreturn {}\n",
        )
        .unwrap();

        dir
    }

    fn create_sourcemap(dir: &std::path::Path) -> std::path::PathBuf {
        let packages_dir = dir.join("Packages");
        let direct_dep_path = packages_dir
            .join("DirectDep.lua")
            .canonicalize()
            .unwrap();
        let transitive_thunk_path = packages_dir
            .join("_Index/scope_a@1.0.0/TransitiveDep.lua")
            .canonicalize()
            .unwrap();
        let pkg_a_path = packages_dir
            .join("_Index/scope_a@1.0.0/a_name/init.lua")
            .canonicalize()
            .unwrap();
        let pkg_b_path = packages_dir
            .join("_Index/scope_b@1.0.0/b_name/init.lua")
            .canonicalize()
            .unwrap();

        let sourcemap = serde_json::json!({
            "name": "Game",
            "className": "DataModel",
            "children": [{
                "name": "Packages",
                "className": "Folder",
                "filePaths": [],
                "children": [
                    {
                        "name": "DirectDep",
                        "className": "ModuleScript",
                        "filePaths": [direct_dep_path.to_str().unwrap()]
                    },
                    {
                        "name": "_Index",
                        "className": "Folder",
                        "filePaths": [],
                        "children": [
                            {
                                "name": "scope_a@1.0.0",
                                "className": "Folder",
                                "filePaths": [],
                                "children": [
                                    {
                                        "name": "TransitiveDep",
                                        "className": "ModuleScript",
                                        "filePaths": [transitive_thunk_path.to_str().unwrap()]
                                    },
                                    {
                                        "name": "a_name",
                                        "className": "ModuleScript",
                                        "filePaths": [pkg_a_path.to_str().unwrap()]
                                    }
                                ]
                            },
                            {
                                "name": "scope_b@1.0.0",
                                "className": "Folder",
                                "filePaths": [],
                                "children": [
                                    {
                                        "name": "b_name",
                                        "className": "ModuleScript",
                                        "filePaths": [pkg_b_path.to_str().unwrap()]
                                    }
                                ]
                            }
                        ]
                    }
                ]
            }]
        });

        let sourcemap_path = dir.join("sourcemap.json");
        fs::write(&sourcemap_path, sourcemap.to_string()).unwrap();
        sourcemap_path
    }

    #[test]
    fn transitive_dep_thunk_is_mutated_with_types() {
        let dir = setup_transitive_dep_test();
        let sourcemap_path = create_sourcemap(&dir);

        let cmd = Command {
            sourcemap: sourcemap_path,
            packages_folders: vec![dir.join("Packages")],
        };

        let result = cmd.run();
        assert!(result.is_ok(), "Command failed: {:?}", result);

        let transitive_thunk = fs::read_to_string(
            dir.join("Packages/_Index/scope_a@1.0.0/TransitiveDep.lua"),
        )
        .unwrap();

        // The transitive dep thunk should have been mutated to re-export types from B
        assert!(
            transitive_thunk.contains("export type"),
            "Transitive dep thunk was not mutated with type exports:\n{}",
            transitive_thunk
        );
        assert!(
            transitive_thunk.contains("Bar"),
            "Transitive dep thunk doesn't re-export 'Bar' type from package B:\n{}",
            transitive_thunk
        );

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn direct_dep_thunk_is_mutated_with_types() {
        let dir = setup_transitive_dep_test();
        let sourcemap_path = create_sourcemap(&dir);

        let cmd = Command {
            sourcemap: sourcemap_path,
            packages_folders: vec![dir.join("Packages")],
        };

        let result = cmd.run();
        assert!(result.is_ok(), "Command failed: {:?}", result);

        let direct_thunk =
            fs::read_to_string(dir.join("Packages/DirectDep.lua")).unwrap();

        // The direct dep thunk should have been mutated to re-export types from A
        assert!(
            direct_thunk.contains("export type"),
            "Direct dep thunk was not mutated with type exports:\n{}",
            direct_thunk
        );
        assert!(
            direct_thunk.contains("Foo"),
            "Direct dep thunk doesn't re-export 'Foo' type from package A:\n{}",
            direct_thunk
        );

        fs::remove_dir_all(&dir).unwrap();
    }

    /// Regression test: when _Index/ contains a stray file (not a package directory),
    /// handle_index_directory must not bail out — it should skip the file and continue
    /// processing all package directories, including their transitive dep thunks.
    #[test]
    fn orphan_file_in_index_does_not_prevent_transitive_dep_mutation() {
        let dir = setup_transitive_dep_test();

        // Place a stray .lua file directly inside _Index/ — this can happen in the wild
        // when Wally leaves orphaned files after package updates.
        // Because filesystem readdir ordering is not guaranteed, we name it "!orphan.lua"
        // (ASCII '!' sorts before letters) to ensure it appears before the package dirs
        // in any typical sorted traversal, maximising the chance it triggers the bug.
        fs::write(
            dir.join("Packages/_Index/!orphan.lua"),
            "-- stray file\n",
        )
        .unwrap();

        let sourcemap_path = create_sourcemap(&dir);

        let cmd = Command {
            sourcemap: sourcemap_path,
            packages_folders: vec![dir.join("Packages")],
        };

        let result = cmd.run();
        assert!(result.is_ok(), "Command failed: {:?}", result);

        let transitive_thunk = fs::read_to_string(
            dir.join("Packages/_Index/scope_a@1.0.0/TransitiveDep.lua"),
        )
        .unwrap();

        assert!(
            transitive_thunk.contains("export type"),
            "Transitive dep thunk was not mutated despite orphan file in _Index/:\n{}",
            transitive_thunk
        );
        assert!(
            transitive_thunk.contains("Bar"),
            "Transitive dep thunk doesn't re-export 'Bar' type:\n{}",
            transitive_thunk
        );

        fs::remove_dir_all(&dir).unwrap();
    }
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

        let mut failures = 0;
        let total = self.packages_folders.len();

        for path in &self.packages_folders {
            if handle_packages_folder(path, &sourcemap)? {
                info!(
                    "Mutation completed successfully for path '{}'",
                    path.display()
                );
            } else {
                failures += 1;
                error!("Mutation failed for path '{}'", path.display());
            }
        }

        if failures == 0 {
            info!("Mutation completed successfully for all paths");
        } else {
            bail!("Mutation failed for {} out of {} paths", failures, total);
        }

        Ok(())
    }
}
