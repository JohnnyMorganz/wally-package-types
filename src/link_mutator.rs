use anyhow::{bail, Result};
use full_moon::{
    ast::{
        luau::{
            ExportedTypeDeclaration, ExportedTypeFunction, GenericDeclaration,
            GenericDeclarationParameter, GenericParameterInfo, IndexedTypeInfo, TypeDeclaration,
            TypeInfo,
        },
        punctuated::{Pair, Punctuated},
        span::ContainedSpan,
        Ast, Expression, LastStmt, LocalAssignment, Parameter, Return, Stmt,
    },
    tokenizer::{Token, TokenReference, TokenType},
};

pub enum ExportedTypeKind {
    Declaration(ExportedTypeDeclaration),
    Function(ExportedTypeFunction),
}

/// Finds all exported types from a give source file
pub fn type_exports_from_source(code: &str) -> Result<Vec<ExportedTypeKind>> {
    let parsed_module = match full_moon::parse(code) {
        Ok(parsed_code) => parsed_code,
        Err(errors) => bail!(errors
            .iter()
            .map(|err| err.to_string())
            .collect::<Vec<_>>()
            .join("\n")),
    };

    Ok(parsed_module
        .nodes()
        .stmts()
        .filter_map(|stmt| match stmt {
            Stmt::ExportedTypeDeclaration(stmt) => {
                Some(ExportedTypeKind::Declaration(stmt.clone()))
            }
            Stmt::ExportedTypeFunction(stmt) => Some(ExportedTypeKind::Function(stmt.clone())),
            _ => None,
        })
        .collect())
}

fn should_keep_default_type(type_info: &TypeInfo, resolved_types: &[String]) -> bool {
    // TODO: we could be more clever here, but for now we keep it simple
    match type_info {
        TypeInfo::Basic(name) => resolved_types.contains(&name.token().to_string()),
        TypeInfo::Boolean(_) => true,
        _ => false,
    }
}

fn will_strip_default(decl: &GenericDeclarationParameter, resolved_types: &[String]) -> bool {
    match decl.default_type() {
        Some(type_info) => !should_keep_default_type(type_info, resolved_types),
        None => true,
    }
}

fn strip_unknown_default_generics(
    generics: &GenericDeclaration,
    resolved_types: &[String],
) -> Punctuated<GenericDeclarationParameter> {
    let pairs: Vec<_> = generics.generics().pairs().collect();

    // Luau requires that once a parameter has a default, all subsequent ones must too.
    // Find the last parameter that will end up without a default (originally lacked one, or
    // has an unknown default that gets stripped). All parameters before it must also lose defaults.
    let last_no_default_idx = pairs
        .iter()
        .rposition(|pair| will_strip_default(pair.value(), resolved_types));

    pairs
        .into_iter()
        .enumerate()
        .map(|(i, pair)| {
            pair.clone().map(|decl| {
                if last_no_default_idx.map_or(false, |last| i <= last) {
                    decl.with_default(None)
                } else {
                    decl
                }
            })
        })
        .collect()
}

pub fn create_new_type_declaration(
    stmt: &ExportedTypeDeclaration,
    known_type_names: Vec<String>,
) -> ExportedTypeDeclaration {
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
                        GenericParameterInfo::Variadic { name, ellipsis } => {
                            TypeInfo::GenericPack {
                                name: name.clone(),
                                ellipsis: ellipsis.clone(),
                            }
                        }
                        _ => unreachable!(),
                    })
                })
                .collect::<Punctuated<_>>(),
        },
        None => IndexedTypeInfo::Basic(stmt.type_declaration().type_name().clone()),
    };

    // Modify the original type declaration to remove the default generics, if they are not resolvable
    let mut resolved_types = stmt
        .type_declaration()
        .generics()
        .map_or(vec![], |generics| {
            generics
                .generics()
                .iter()
                .map(|generic| match generic.parameter() {
                    GenericParameterInfo::Name(name) => name.token().to_string(),
                    GenericParameterInfo::Variadic { name, .. } => name.token().to_string(),
                    other => unreachable!("unknown node: {:?}", other),
                })
                .collect()
        });

    resolved_types.extend(
        [
            "any", "boolean", "buffer", "never", "number", "string", "thread", "unknown",
        ]
        .into_iter()
        .map(String::from)
        .chain(known_type_names),
    );

    let original_type_declaration = match stmt.type_declaration().generics() {
        Some(generics) => stmt.type_declaration().clone().with_generics(Some(
            generics
                .clone()
                .with_generics(strip_unknown_default_generics(generics, &resolved_types)),
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

pub fn create_new_type_function_declaration(
    stmt: &ExportedTypeFunction,
) -> ExportedTypeDeclaration {
    let mut lhs_generics = Punctuated::new();
    let mut rhs_generics = Punctuated::new();

    for (index, pair) in stmt
        .type_function()
        .function_body()
        .parameters()
        .pairs()
        .enumerate()
    {
        let (parameter, punctuation) = pair.clone().into_tuple();

        let token = match parameter {
            Parameter::Name(token) => token.with_token(Token::new(TokenType::Identifier {
                identifier: format!("T{}", index).into(),
            })),

            // Vararg type function parameters cannot resolve to a generic
            Parameter::Ellipsis(_) => {
                continue;
            }
            _ => unreachable!(),
        };

        lhs_generics.push(Pair::new(
            GenericDeclarationParameter::new(GenericParameterInfo::Name(token.clone())),
            punctuation.clone(),
        ));
        rhs_generics.push(Pair::new(TypeInfo::Basic(token), punctuation));
    }

    if let (Some(last_lhs), Some(last_rhs)) = (lhs_generics.pop(), rhs_generics.pop()) {
        lhs_generics.push(Pair::new(last_lhs.into_value(), None));
        rhs_generics.push(Pair::new(last_rhs.into_value(), None));
    }

    let type_info = IndexedTypeInfo::Generic {
        base: stmt.type_function().function_name().clone(),
        arrows: ContainedSpan::new(
            TokenReference::symbol("<").unwrap(),
            TokenReference::symbol(">").unwrap(),
        ),
        generics: rhs_generics,
    };

    let type_declaration = TypeDeclaration::new(
        stmt.type_function().function_name().clone(),
        TypeInfo::Module {
            module: TokenReference::new(
                vec![],
                Token::new(TokenType::Identifier {
                    identifier: "REQUIRED_MODULE".into(),
                }),
                vec![],
            ),
            punctuation: TokenReference::symbol(".").unwrap(),
            type_info: Box::new(type_info),
        },
    );

    ExportedTypeDeclaration::new(
        type_declaration.with_generics(Some(GenericDeclaration::new().with_generics(lhs_generics))),
    )
}

// Creates a list of re-exported types from the types found in the source file
fn re_export_types(stmts: Vec<ExportedTypeKind>) -> Vec<(Stmt, Option<TokenReference>)> {
    let known_type_names: Vec<String> = stmts
        .iter()
        .map(|stmt| match stmt {
            ExportedTypeKind::Declaration(stmt) => {
                stmt.type_declaration().type_name().token().to_string()
            }
            ExportedTypeKind::Function(stmt) => {
                stmt.type_function().function_name().token().to_string()
            }
        })
        .collect();

    stmts
        .iter()
        .map(|stmt| match stmt {
            ExportedTypeKind::Declaration(decl) => (
                Stmt::ExportedTypeDeclaration(create_new_type_declaration(
                    decl,
                    known_type_names.clone(),
                )),
                Some(TokenReference::new(
                    vec![],
                    Token::new(TokenType::Whitespace {
                        characters: "\n".into(),
                    }),
                    vec![],
                )),
            ),
            ExportedTypeKind::Function(func) => (
                Stmt::ExportedTypeDeclaration(create_new_type_function_declaration(func)),
                Some(TokenReference::new(
                    vec![],
                    Token::new(TokenType::Whitespace {
                        characters: "\n".into(),
                    }),
                    vec![],
                )),
            ),
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

#[allow(clippy::large_enum_variant)]
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
    let type_exports = type_exports_from_source(contents)?;

    if type_exports.is_empty() {
        return Ok(MutateLinkResult::Unchanged);
    }

    let new_nodes = parsed_code
        .nodes()
        .clone()
        .with_stmts(
            std::iter::once(extract_require_into_local_stmt(return_expressions))
                .chain(re_export_types(type_exports))
                .collect(),
        )
        .with_last_stmt(Some(create_return_require_variable()));
    Ok(MutateLinkResult::Changed(parsed_code.with_nodes(new_nodes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exported_types_with_const_are_parsed() {
        let code = r"
            const VALUE = 5
            export type Foo = Types.Foo
        ";

        assert!(full_moon::parse(code).is_ok());
    }

    #[test]
    fn re_exports_generic_defaults_if_they_are_part_of_the_type() {
        let code = r"
            export type Value<T, S = T> = Types.Value<T, S>
        ";

        let type_declarations = type_exports_from_source(code).unwrap();
        assert_eq!(type_declarations.len(), 1);

        let reexported_type_declarations = re_export_types(type_declarations);
        assert_eq!(reexported_type_declarations.len(), 1);

        assert_eq!(
            reexported_type_declarations[0].0.to_string(),
            "export type Value<T, S = T> = REQUIRED_MODULE.Value<T, S >"
        );
    }

    #[test]
    fn does_not_re_export_unknown_default_generics() {
        let code = r"
            export type Value<T, S = Object> = Types.Value<T, S>
        ";

        let type_declarations = type_exports_from_source(code).unwrap();
        assert_eq!(type_declarations.len(), 1);

        let reexported_type_declarations = re_export_types(type_declarations);
        assert_eq!(reexported_type_declarations.len(), 1);

        assert_eq!(
            reexported_type_declarations[0].0.to_string(),
            "export type Value<T, S > = REQUIRED_MODULE.Value<T, S >"
        );
    }

    #[test]
    fn re_exports_generic_defaults_if_they_are_builtin_types() {
        let code = r"
            export type Value<T, S = unknown> = Types.Value<T, S>
        ";

        let type_declarations = type_exports_from_source(code).unwrap();
        assert_eq!(type_declarations.len(), 1);

        let reexported_type_declarations = re_export_types(type_declarations);
        assert_eq!(reexported_type_declarations.len(), 1);

        assert_eq!(
            reexported_type_declarations[0].0.to_string(),
            "export type Value<T, S = unknown> = REQUIRED_MODULE.Value<T, S >"
        );
    }

    #[test]
    fn re_exports_generic_defaults_if_they_are_defined_earlier() {
        let code = r"
            export type Action = string
            export type Value<T, S = Action> = Types.Value<T, S>
        ";

        let type_declarations = type_exports_from_source(code).unwrap();
        assert_eq!(type_declarations.len(), 2);

        let reexported_type_declarations = re_export_types(type_declarations);
        assert_eq!(reexported_type_declarations.len(), 2);

        assert_eq!(
            reexported_type_declarations[1].0.to_string(),
            "export type Value<T, S = Action> = REQUIRED_MODULE.Value<T, S >"
        );
    }

    #[test]
    fn strips_earlier_defaults_when_later_param_has_unknown_default() {
        let code = r"
            export type Producer<State = any, Dispatchers = ExternalType> = Types.Producer<State, Dispatchers>
        ";

        let type_declarations = type_exports_from_source(code).unwrap();
        assert_eq!(type_declarations.len(), 1);

        let reexported_type_declarations = re_export_types(type_declarations);
        assert_eq!(reexported_type_declarations.len(), 1);

        assert_eq!(
            reexported_type_declarations[0].0.to_string(),
            "export type Producer<State , Dispatchers > = REQUIRED_MODULE.Producer<State , Dispatchers >"
        );
    }

    #[test]
    fn re_exports_type_functions() {
        let code = "
            export type function keyof(a, b)
            
            end
        ";

        let type_exports = type_exports_from_source(code).unwrap();
        assert_eq!(type_exports.len(), 1);

        let reexported_types = re_export_types(type_exports);
        assert_eq!(reexported_types.len(), 1);

        assert_eq!(
            reexported_types[0].0.to_string(),
            "export type keyof<T0, T1> = REQUIRED_MODULE.keyof<T0, T1>"
        );
    }

    #[test]
    fn re_exports_type_functions_with_ellipsis_parameter() {
        let code = "
            export type function keyof(a, b, ...)
            
            end
        ";

        let type_exports = type_exports_from_source(code).unwrap();
        assert_eq!(type_exports.len(), 1);

        let reexported_types = re_export_types(type_exports);
        assert_eq!(reexported_types.len(), 1);

        assert_eq!(
            reexported_types[0].0.to_string(),
            "export type keyof<T0, T1> = REQUIRED_MODULE.keyof<T0, T1>"
        );
    }

    #[test]
    fn re_exports_type_functions_with_only_ellipsis_parameter() {
        let code = "
            export type function keyof(...)
            
            end
        ";

        let type_exports = type_exports_from_source(code).unwrap();
        assert_eq!(type_exports.len(), 1);

        let reexported_types = re_export_types(type_exports);
        assert_eq!(reexported_types.len(), 1);

        assert_eq!(
            reexported_types[0].0.to_string(),
            "export type keyof<> = REQUIRED_MODULE.keyof<>"
        );
    }
}
