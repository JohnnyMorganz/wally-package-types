use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use full_moon::{
    ast::{
        punctuated::{Pair, Punctuated},
        span::ContainedSpan,
        types::{ExportedTypeDeclaration, GenericParameterInfo, IndexedTypeInfo, TypeInfo},
        Call, Expression, FunctionArgs, Index, LastStmt, LocalAssignment, Return, Stmt, Suffix,
        Value, Var,
    },
    tokenizer::{Token, TokenReference, TokenType},
};

use serde::Deserialize;

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct SourcemapNode {
    name: String,
    class_name: String,
    #[serde(default)]
    file_paths: Vec<PathBuf>,
    #[serde(default)]
    children: Vec<SourcemapNode>,
}

impl SourcemapNode {
    fn find_child(&self, name: String) -> Option<&SourcemapNode> {
        self.children.iter().find(|child| child.name == name)
    }
}

fn find_node_with_parent(
    root: &SourcemapNode,
    path: PathBuf,
) -> (Option<&SourcemapNode>, Option<&SourcemapNode>) {
    let mut stack = vec![(root, None)];

    while let Some((node, parent)) = stack.pop() {
        if node.file_paths.contains(&path.to_path_buf()) {
            return (Some(node), parent);
        }

        for child in &node.children {
            stack.push((child, Some(node)));
        }
    }

    (None, None)
}

fn mutate_sourcemap(node: &mut SourcemapNode) {
    node.file_paths = node
        .file_paths
        .iter()
        .map(|path| {
            path.canonicalize()
                .unwrap_or_else(|_| panic!("failed to canonicalize {}", path.display()))
        })
        .collect();

    for child in &mut node.children {
        mutate_sourcemap(child);
    }
}

fn expression_to_components(expression: &Expression) -> Vec<String> {
    let mut components = Vec::new();

    match expression {
        Expression::Value { value, .. } => match &**value {
            Value::Var(Var::Expression(var_expression)) => {
                components.push(var_expression.prefix().to_string().trim().to_string());

                for suffix in var_expression.suffixes() {
                    match suffix {
                        Suffix::Index(index) => match index {
                            Index::Dot { name, .. } => {
                                components.push(name.to_string().trim().to_string());
                            }
                            Index::Brackets { expression, .. } => match expression {
                                Expression::Value { value, .. } => match &**value {
                                    Value::String(name) => match name.token_type() {
                                        TokenType::StringLiteral { literal, .. } => {
                                            components.push(literal.trim().to_string());
                                        }
                                        _ => panic!("non-string brackets index"),
                                    },
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
        },
        _ => panic!("unknown require expression"),
    };

    components
}

fn match_require(expression: &Expression) -> Option<Vec<String>> {
    match expression {
        Expression::Value { value, .. } => match &**value {
            Value::FunctionCall(call) => {
                if call.prefix().to_string().trim() == "require" && call.suffixes().count() == 1 {
                    if let Suffix::Call(Call::AnonymousCall(FunctionArgs::Parentheses {
                        arguments,
                        ..
                    })) = call.suffixes().next().unwrap()
                    {
                        if arguments.len() == 1 {
                            return Some(expression_to_components(
                                arguments.iter().next().unwrap(),
                            ));
                        }
                    }
                } else {
                    panic!("unknown require expression");
                }
            }
            _ => panic!("unknown require expression"),
        },
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
                        GenericParameterInfo::Variadic { name, ellipse } => TypeInfo::Variadic {
                            ellipse: ellipse.clone(),
                            type_info: Box::new(TypeInfo::Basic(name.clone())),
                        },
                        _ => unreachable!(),
                    })
                })
                .collect::<Punctuated<_>>(),
        },
        None => IndexedTypeInfo::Basic(stmt.type_declaration().type_name().clone()),
    };

    // Can't use TypeDeclaration::new(), since it always panics
    let type_declaration = stmt
        .type_declaration()
        .clone()
        .with_type_definition(TypeInfo::Module {
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

impl Command {
    pub fn run(&self) -> Result<()> {
        let sourcemap_contents = std::fs::read_to_string(&self.sourcemap)?;
        let mut sourcemap: SourcemapNode = serde_json::from_str(&sourcemap_contents)?;

        // Mutate the sourcemap so that all file paths are canonicalized for simplicity
        // And that they contain pointers to their parent
        mutate_sourcemap(&mut sourcemap);

        for entry in std::fs::read_dir(&self.packages_folder)?.flatten() {
            if entry.file_name() == "_Index" {
                continue;
            }

            println!("Mutating {}", entry.path().display());

            // The entry should be a thunk
            let parsed_code = full_moon::parse(&std::fs::read_to_string(entry.path())?)?;
            assert!(parsed_code.nodes().last_stmt().is_some());

            let mut new_stmts = Vec::new();
            let mut type_declarations_created = false;

            if let Some(LastStmt::Return(r#return)) = parsed_code.nodes().last_stmt() {
                let returned_expression = r#return.returns().iter().next().unwrap();
                let path_components =
                    match_require(returned_expression).expect("could not resolve path for require");

                println!("Found require in format {}", path_components.join("/"));

                let mut iter = path_components.iter();

                // HACK: we rely on the first two components to be "script.Parent", so that we can start at the "Packages" folder
                assert!(iter.next().unwrap() == "script");
                assert!(iter.next().unwrap() == "Parent");

                let mut current = find_node_with_parent(&sourcemap, entry.path().canonicalize()?)
                    .1
                    .expect("no parent found");

                for component in iter {
                    current = current
                        .find_child(component.to_string())
                        .expect("unable to find child");
                }

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
                                std::iter::once(Pair::End(Expression::Value {
                                    value: Box::new(Value::Symbol(TokenReference::new(
                                        vec![],
                                        Token::new(TokenType::Identifier {
                                            identifier: "REQUIRED_MODULE".into(),
                                        }),
                                        vec![Token::new(TokenType::Whitespace {
                                            characters: "\n".into(),
                                        })],
                                    ))),
                                    type_assertion: None,
                                }))
                                .collect(),
                            ),
                        ),
                        None,
                    )));
                let new_ast = parsed_code.with_nodes(new_nodes);

                std::fs::write(entry.path(), full_moon::print(&new_ast))?;
            }
        }
        
        Ok(())
    }
}
