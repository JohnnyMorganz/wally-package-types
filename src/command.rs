use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Parser;
use full_moon::{
    ast::{
        punctuated::{Pair, Punctuated},
        span::ContainedSpan,
        types::{ExportedTypeDeclaration, GenericParameterInfo, IndexedTypeInfo, TypeInfo},
        Call, Expression, FunctionArgs, Index, LastStmt, LocalAssignment, Return, Stmt, Suffix,
        Var,
    },
    tokenizer::{Token, TokenReference, TokenType},
};

use crate::sourcemap::*;

fn expression_to_components(expression: &Expression) -> Vec<String> {
    let mut components = Vec::new();

    match expression {
        Expression::Var(Var::Expression(var_expression)) => {
            components.push(var_expression.prefix().to_string().trim().to_string());

            for suffix in var_expression.suffixes() {
                match suffix {
                    Suffix::Index(index) => match index {
                        Index::Dot { name, .. } => {
                            components.push(name.to_string().trim().to_string());
                        }
                        Index::Brackets { expression, .. } => match expression {
                            Expression::String(name) => match name.token_type() {
                                TokenType::StringLiteral { literal, .. } => {
                                    components.push(literal.trim().to_string());
                                }
                                _ => panic!("non-string brackets index"),
                            },
                            _ => panic!("non-string brackets index"),
                        },
                        _ => panic!("unknown index"),
                    },
                    _ => panic!("incorrect suffix"),
                }
            }
        }
        _ => panic!("unknown require expression"),
    };

    components
}

fn match_require(expression: &Expression) -> Option<Vec<String>> {
    match expression {
        Expression::FunctionCall(call) => {
            if call.prefix().to_string().trim() == "require" && call.suffixes().count() == 1 {
                if let Suffix::Call(Call::AnonymousCall(FunctionArgs::Parentheses {
                    arguments,
                    ..
                })) = call.suffixes().next().unwrap()
                {
                    if arguments.len() == 1 {
                        return Some(expression_to_components(arguments.iter().next().unwrap()));
                    }
                }
            } else {
                panic!("unknown require expression");
            }
        }
        _ => panic!("unknown require expression"),
    }

    None
}

fn create_new_type_declaration(stmt: &ExportedTypeDeclaration) -> ExportedTypeDeclaration {
    let type_info = match stmt.type_declaration().generics() {
        Some(generics) => IndexedTypeInfo::Generic {
            base: stmt.type_declaration().type_name().clone(),
            arrows: ContainedSpan::new(
                TokenReference::symbol("<").unwrap(),
                TokenReference::symbol(">").unwrap(),
            ),
            generics: generics
                .generics()
                .pairs()
                .map(|pair| {
                    pair.clone().map(|decl| match decl.parameter() {
                        GenericParameterInfo::Name(token) => TypeInfo::Basic(token.clone()),
                        GenericParameterInfo::Variadic { name, ellipse } => TypeInfo::GenericPack {
                            name: name.clone(),
                            ellipse: ellipse.clone(),
                        },
                        _ => unreachable!(),
                    })
                })
                .collect::<Punctuated<_>>(),
        },
        None => IndexedTypeInfo::Basic(stmt.type_declaration().type_name().clone()),
    };

    // Modify the original type declaration to remove the default generics
    let original_type_declaration = match stmt.type_declaration().generics() {
        Some(generics) => stmt.type_declaration().clone().with_generics(Some(
            generics.clone().with_generics(
                generics
                    .generics()
                    .pairs()
                    .map(|pair| pair.clone().map(|decl| decl.with_default(None)))
                    .collect::<Punctuated<_>>(),
            ),
        )),
        None => stmt.type_declaration().clone(),
    };

    // Can't use TypeDeclaration::new(), since it always panics
    let type_declaration = original_type_declaration.with_type_definition(TypeInfo::Module {
        module: TokenReference::new(
            vec![],
            Token::new(TokenType::Identifier {
                identifier: "REQUIRED_MODULE".into(),
            }),
            vec![],
        ),
        punctuation: TokenReference::symbol(".").unwrap(),
        type_info: Box::new(type_info),
    });

    ExportedTypeDeclaration::new(type_declaration)
}

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

fn mutate_thunk(path: &Path, root: &SourcemapNode) -> Result<()> {
    println!("Mutating {}", path.display());

    // The entry should be a thunk
    let parsed_code = full_moon::parse(&std::fs::read_to_string(path)?)?;
    assert!(parsed_code.nodes().last_stmt().is_some());

    let mut new_stmts = Vec::new();
    let mut type_declarations_created = false;

    if let Some(LastStmt::Return(r#return)) = parsed_code.nodes().last_stmt() {
        let returned_expression = r#return.returns().iter().next().unwrap();
        let path_components =
            match_require(returned_expression).expect("could not resolve path for require");

        println!("Found require in format {}", path_components.join("/"));

        let mut iter = path_components.iter();
        let first_in_chain = iter.next().expect("No path components");
        assert!(first_in_chain == "script" || first_in_chain == "game");

        let mut node_path = if first_in_chain == "script" {
            find_node(root, path.canonicalize()?).expect("could not find node path")
        } else {
            vec![root]
        };

        for component in iter {
            if component == "Parent" {
                node_path.pop().expect("No parent available");
            } else {
                node_path.push(
                    node_path
                        .last()
                        .unwrap()
                        .find_child(component.to_string())
                        .expect("unable to find child"),
                );
            }
        }

        let current = node_path.last().unwrap();
        let file_path = current.file_paths.get(0).expect("No file path for require");
        println!(
            "Required file is {} [{}], located at {}",
            current.name,
            current.class_name,
            file_path.display()
        );

        new_stmts.push((
            Stmt::LocalAssignment(
                LocalAssignment::new(
                    std::iter::once(Pair::End(TokenReference::new(
                        vec![],
                        Token::new(TokenType::Identifier {
                            identifier: "REQUIRED_MODULE".into(),
                        }),
                        vec![],
                    )))
                    .collect(),
                )
                .with_equal_token(Some(TokenReference::symbol(" = ").unwrap()))
                .with_expressions(r#return.returns().clone()),
            ),
            None,
        ));

        let parsed_module = full_moon::parse(&std::fs::read_to_string(file_path)?)?;
        for stmt in parsed_module.nodes().stmts() {
            if let Stmt::ExportedTypeDeclaration(stmt) = stmt {
                type_declarations_created = true;
                new_stmts.push((
                    Stmt::ExportedTypeDeclaration(create_new_type_declaration(stmt)),
                    Some(TokenReference::new(
                        vec![],
                        Token::new(TokenType::Whitespace {
                            characters: "\n".into(),
                        }),
                        vec![],
                    )),
                ))
            }
        }
    }

    // Only commit to writing a new file if we created new type declarations
    if type_declarations_created {
        let new_nodes = parsed_code
            .nodes()
            .clone()
            .with_stmts(new_stmts)
            .with_last_stmt(Some((
                LastStmt::Return(
                    Return::new().with_returns(
                        std::iter::once(Pair::End(Expression::Symbol(TokenReference::new(
                            vec![],
                            Token::new(TokenType::Identifier {
                                identifier: "REQUIRED_MODULE".into(),
                            }),
                            vec![Token::new(TokenType::Whitespace {
                                characters: "\n".into(),
                            })],
                        ))))
                        .collect(),
                    ),
                ),
                None,
            )));
        let new_ast = parsed_code.with_nodes(new_nodes);

        std::fs::write(path, full_moon::print(&new_ast))?;
    }
    Ok(())
}

fn handle_index_directory(path: &Path, root: &SourcemapNode) -> Result<()> {
    for package_entry in std::fs::read_dir(path)?.flatten() {
        for thunk in std::fs::read_dir(package_entry.path())?.flatten() {
            if thunk.file_type().unwrap().is_file() {
                mutate_thunk(&thunk.path(), root)?;
            }
        }
    }

    Ok(())
}

impl Command {
    pub fn run(&self) -> Result<()> {
        let sourcemap_contents = std::fs::read_to_string(&self.sourcemap)?;
        let mut sourcemap: SourcemapNode = serde_json::from_str(&sourcemap_contents)?;

        // Mutate the sourcemap so that all file paths are canonicalized for simplicity
        // And that they contain pointers to their parent
        mutate_sourcemap(&mut sourcemap);

        for entry in std::fs::read_dir(&self.packages_folder)?.flatten() {
            if entry.file_name() == "_Index" {
                handle_index_directory(&entry.path(), &sourcemap)?;
                continue;
            }

            mutate_thunk(&entry.path(), &sourcemap)?;
        }

        Ok(())
    }
}
