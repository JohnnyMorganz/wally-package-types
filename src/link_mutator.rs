use anyhow::Result;
use full_moon::{
    ast::{
        punctuated::{Pair, Punctuated},
        span::ContainedSpan,
        types::{ExportedTypeDeclaration, GenericParameterInfo, IndexedTypeInfo, TypeInfo},
        Ast, Expression, LastStmt, LocalAssignment, Return, Stmt,
    },
    tokenizer::{Token, TokenReference, TokenType},
};

/// Finds all exported type declarations from a give source file
pub fn type_declarations_from_source(code: &str) -> Result<Vec<ExportedTypeDeclaration>> {
    let parsed_module = full_moon::parse(code)?;

    Ok(parsed_module
        .nodes()
        .stmts()
        .filter_map(|stmt| match stmt {
            Stmt::ExportedTypeDeclaration(stmt) => Some(stmt.clone()),
            _ => None,
        })
        .collect())
}

pub fn create_new_type_declaration(stmt: &ExportedTypeDeclaration) -> ExportedTypeDeclaration {
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

// Creates a list of re-exported type declarations from the type declarations found in the source file
fn re_export_type_declarations(
    stmts: Vec<ExportedTypeDeclaration>,
) -> Vec<(Stmt, Option<TokenReference>)> {
    stmts
        .iter()
        .map(|stmt| {
            (
                Stmt::ExportedTypeDeclaration(create_new_type_declaration(stmt)),
                Some(TokenReference::new(
                    vec![],
                    Token::new(TokenType::Whitespace {
                        characters: "\n".into(),
                    }),
                    vec![],
                )),
            )
        })
        .collect()
}

/// Extracts a require expression out into a local variable of form `local REQUIRED_MODULE = ...`
fn extract_require_into_local_stmt(
    return_expressions: Punctuated<Expression>,
) -> (Stmt, Option<TokenReference>) {
    (
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
            .with_expressions(return_expressions),
        ),
        None,
    )
}

/// Creates a `return REQUIRED_MODULE` node
fn create_return_require_variable() -> (LastStmt, Option<TokenReference>) {
    (
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
    )
}

pub enum MutateLinkResult {
    Changed(Ast),
    Unchanged,
}

/// Given an old link and the contents of the file it points to, creates a new link source
pub fn mutate_link(
    parsed_code: Ast,
    return_expressions: Punctuated<Expression>,
    contents: &str,
) -> Result<MutateLinkResult> {
    let type_declarations = type_declarations_from_source(contents)?;

    if type_declarations.is_empty() {
        return Ok(MutateLinkResult::Unchanged);
    }

    let new_nodes = parsed_code
        .nodes()
        .clone()
        .with_stmts(
            std::iter::once(extract_require_into_local_stmt(return_expressions))
                .chain(re_export_type_declarations(type_declarations))
                .collect(),
        )
        .with_last_stmt(Some(create_return_require_variable()));
    Ok(MutateLinkResult::Changed(parsed_code.with_nodes(new_nodes)))
}
