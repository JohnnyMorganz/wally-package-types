use anyhow::{bail, Result};
use full_moon::{
    ast::{Call, Expression, FunctionArgs, Index, Suffix, Var},
    tokenizer::TokenType,
};

/// Decomposes a VarExpression into a list of string components
pub fn expression_to_components(expression: &Expression) -> Result<Vec<String>> {
    let mut components = Vec::new();

    let Expression::Var(Var::Expression(var_expression)) = expression else {
        bail!("require expression not supported: expression must contain components of form `.value` or `['value']`")
    };

    components.push(var_expression.prefix().to_string().trim().to_string());

    for suffix in var_expression.suffixes() {
        let Suffix::Index(index) = suffix else {
            bail!("require expression not supported: expression must contain components of form `.value` or `['value']`")
        };

        match index {
            Index::Dot { name, .. } => {
                components.push(name.to_string().trim().to_string());
            }
            Index::Brackets { expression, .. } => {
                let Expression::String(name) = expression else {
                    bail!("require expression not supported: expression contains brackets component not of the form ['value']")
                };
                let TokenType::StringLiteral { literal, .. } = name.token_type() else {
                    bail!("require expression not supported: expression contains brackets component not of the form ['value']")
                };
                components.push(literal.trim().to_string());
            }
            _ => unreachable!(),
        }
    }

    Ok(components)
}

pub fn match_require(expression: &Expression) -> Result<Vec<String>> {
    let Expression::FunctionCall(call) = expression else {
        bail!("'{}' is not a function call", expression.to_string().trim());
    };

    if call.prefix().to_string().trim() == "require" && call.suffixes().count() == 1 {
        if let Suffix::Call(Call::AnonymousCall(FunctionArgs::Parentheses { arguments, .. })) =
            call.suffixes().next().unwrap()
        {
            if arguments.len() == 1 {
                return expression_to_components(arguments.iter().next().unwrap());
            }
        }
    } else {
        bail!("unknown require expression '{}'", call.to_string().trim());
    }

    bail!(
        "'{}' is not a require function call",
        call.to_string().trim()
    )
}

#[cfg(test)]
mod tests {
    use full_moon::ast::Stmt;

    use super::*;

    fn require_expression(code: &str) -> Expression {
        let parsed_ast = full_moon::parse(code).unwrap();
        let stmt = parsed_ast.nodes().stmts().next().unwrap();
        let Stmt::FunctionCall(expression) = stmt else {
            unreachable!()
        };
        Expression::FunctionCall(expression.clone())
    }

    fn expression_into_components(code: &str, components: Vec<&str>) -> bool {
        match_require(&require_expression(code)).unwrap() == components
    }

    #[test]
    fn simple_require() {
        assert!(expression_into_components(
            "require(script.Parent.Example)",
            vec!["script", "Parent", "Example"]
        ))
    }

    #[test]
    fn require_with_brackets() {
        assert!(expression_into_components(
            "require(script.Parent['Example'])",
            vec!["script", "Parent", "Example"]
        ))
    }

    #[test]
    fn unhandled_require() {
        assert!(match_require(&require_expression("require('string')")).is_err())
    }
}
