//! Checked arithmetic. SQLite turns an INTEGER `+`, `-`, `*` or `/` that
//! leaves `i64` into a REAL; Rocky evaluates them with the `checked_*`
//! functions instead, which fail with SQLite's own "integer overflow" error.
//! Replayed statements and assertions are rewritten to call them; replayed
//! table declarations, which SQLite evaluates as written, must call them.
use rusqlite::{
    Connection,
    functions::{Context, FunctionFlags},
    types::{Value as Engine, ValueRef},
};
use sqlparser::ast::*;
use std::ops::ControlFlow;

/// The message of every overflow, as SQLite's `sum` and `abs` raise it.
pub const OVERFLOW: &str = "integer overflow";

#[derive(Clone, Copy)]
enum Operation {
    Add,
    Subtract,
    Multiply,
    Divide,
}
impl Operation {
    const ALL: [(&'static str, Self); 4] = [
        ("checked_add", Self::Add),
        ("checked_sub", Self::Subtract),
        ("checked_mul", Self::Multiply),
        ("checked_div", Self::Divide),
    ];
    /// SQLite's result for numeric operands, except that an INTEGER result
    /// outside `i64` is an error. Division by zero is NULL, as in SQLite.
    fn apply(self, left: ValueRef<'_>, right: ValueRef<'_>) -> Result<Engine, &'static str> {
        let real = |value: ValueRef<'_>| match value {
            ValueRef::Integer(value) => Ok(value as f64),
            ValueRef::Real(value) => Ok(value),
            _ => Err("checked arithmetic needs INTEGER or REAL operands"),
        };
        Ok(match (left, right) {
            (ValueRef::Null, _) | (_, ValueRef::Null) => Engine::Null,
            (ValueRef::Integer(left), ValueRef::Integer(right)) => {
                let result = match self {
                    Self::Add => left.checked_add(right),
                    Self::Subtract => left.checked_sub(right),
                    Self::Multiply => left.checked_mul(right),
                    Self::Divide if right == 0 => return Ok(Engine::Null),
                    Self::Divide => left.checked_div(right),
                };
                Engine::Integer(result.ok_or(OVERFLOW)?)
            }
            (left, right) => {
                let (left, right) = (real(left)?, real(right)?);
                let result = match self {
                    Self::Add => left + right,
                    Self::Subtract => left - right,
                    Self::Multiply => left * right,
                    Self::Divide if right == 0.0 => return Ok(Engine::Null),
                    Self::Divide => left / right,
                };
                if result.is_nan() {
                    Engine::Null
                } else {
                    Engine::Real(result)
                }
            }
        })
    }
}

/// Registers the `checked_*` functions on a connection; Rocky's own
/// connections have them, a plain one replaying intents needs them.
pub fn register(connection: &Connection) -> rusqlite::Result<()> {
    let flags = FunctionFlags::SQLITE_UTF8
        | FunctionFlags::SQLITE_DETERMINISTIC
        | FunctionFlags::SQLITE_INNOCUOUS;
    for (name, operation) in Operation::ALL {
        connection.create_scalar_function(name, 2, flags, move |context: &Context<'_>| {
            operation
                .apply(context.get_raw(0), context.get_raw(1))
                .map_err(|reason| rusqlite::Error::UserFunctionError(reason.into()))
        })?;
    }
    Ok(())
}

/// Rewrites INTEGER-promoting operators into `checked_*` calls. A negated
/// number stays a literal.
pub(crate) struct Checked;
impl VisitorMut for Checked {
    type Break = ();
    fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
        let call = |name: &str, left: Expr, right: Expr| {
            let arg = |expr| FunctionArg::Unnamed(FunctionArgExpr::Expr(expr));
            Expr::Function(Function {
                name: ObjectName::from(vec![Ident::new(name)]),
                uses_odbc_syntax: false,
                parameters: FunctionArguments::None,
                args: FunctionArguments::List(FunctionArgumentList {
                    duplicate_treatment: None,
                    args: vec![arg(left), arg(right)],
                    clauses: Vec::new(),
                }),
                within_group: Vec::new(),
                filter: None,
                null_treatment: None,
                over: None,
            })
        };
        let taken = std::mem::replace(expr, Expr::value(Value::Null));
        *expr = match taken {
            Expr::BinaryOp { left, op, right } => {
                let name = match op {
                    BinaryOperator::Plus => "checked_add",
                    BinaryOperator::Minus => "checked_sub",
                    BinaryOperator::Multiply => "checked_mul",
                    BinaryOperator::Divide => "checked_div",
                    op => {
                        *expr = Expr::BinaryOp { left, op, right };
                        return ControlFlow::Continue(());
                    }
                };
                call(name, *left, *right)
            }
            Expr::UnaryOp {
                op: UnaryOperator::Minus,
                expr: operand,
            } if !matches!(*operand, Expr::Value(_)) => {
                call("checked_sub", Expr::value(number("0")), *operand)
            }
            other => other,
        };
        ControlFlow::Continue(())
    }
}
fn number(digits: &str) -> Value {
    Value::Number(digits.into(), false)
}

/// `sql` with its arithmetic checked, for the author's own run of a
/// statement whose replay `split_intent` rewrites the same way.
pub fn checked_statement(sql: &str) -> Result<String, String> {
    let parsed = crate::parsed::parse(sql)?;
    let [statement] = &*parsed else {
        return Err("one statement required".into());
    };
    let mut statement = statement.clone();
    let _ = VisitMut::visit(&mut statement, &mut Checked);
    Ok(statement.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        register(&connection).unwrap();
        connection
    }
    fn evaluate(connection: &Connection, sql: &str) -> rusqlite::Result<Engine> {
        connection.query_row(&format!("SELECT {sql}"), [], |row| row.get(0))
    }

    #[test]
    fn every_allowed_function_exists_in_this_sqlite_build() {
        let connection = connection();
        for function in rocky_core::DETERMINISTIC_FUNCTIONS {
            // Arity and window misuse fail too; only a missing function matters.
            if let Err(error) = connection.prepare(&format!("SELECT {function}()")) {
                assert!(
                    !error.to_string().contains("no such function"),
                    "{function}: {error}"
                );
            }
        }
    }

    #[test]
    fn checked_functions_match_sqlite_except_on_overflow() {
        let connection = connection();
        let max = i64::MAX;
        let min = i64::MIN;
        // Results SQLite's operators compute exactly stay identical.
        for (left, right) in [
            ("7", "2"),
            ("-7", "2"),
            ("7", "-2"),
            ("7", "0"),
            ("0", "0"),
            ("NULL", "3"),
            ("3", "NULL"),
            ("1.5", "2"),
            ("2", "0.5"),
            ("1e308", "1e308"),
            ("1.5", "0.0"),
            (&format!("{max}"), "1.5"),
            (&format!("{max}"), "0"),
            (&format!("({min})"), "0"),
        ] {
            for (function, operator) in [
                ("checked_add", "+"),
                ("checked_sub", "-"),
                ("checked_mul", "*"),
                ("checked_div", "/"),
            ] {
                let native = evaluate(&connection, &format!("{left} {operator} {right}")).unwrap();
                let checked =
                    evaluate(&connection, &format!("{function}({left}, {right})")).unwrap();
                assert_eq!(checked, native, "{left} {operator} {right}");
            }
        }
        // Where SQLite would have promoted to REAL, the checked call fails.
        for sql in [
            format!("checked_add({max}, 1)"),
            format!("checked_sub(({min}), 1)"),
            format!("checked_mul({max}, 2)"),
            format!("checked_div(({min}), -1)"),
        ] {
            let error = evaluate(&connection, &sql).unwrap_err();
            assert!(error.to_string().contains(OVERFLOW), "{sql}: {error}");
        }
        assert_eq!(
            evaluate(&connection, &format!("({min}) - 1")).unwrap(),
            Engine::Real(min as f64),
            "SQLite's own operator promotes"
        );
        let text = evaluate(&connection, "checked_add('1', 1)").unwrap_err();
        assert!(text.to_string().contains("INTEGER or REAL"), "{text}");
    }

    #[test]
    fn operators_become_checked_calls_and_negative_numbers_stay_literals() {
        assert_eq!(
            checked_statement(
                "UPDATE t SET a = a + ? * -b, c = -1 WHERE d / 2 > e - 1 AND f % 2 = 0"
            )
            .unwrap(),
            "UPDATE t SET a = checked_add(a, checked_mul(?, checked_sub(0, b))), c = -1 \
             WHERE checked_div(d, 2) > checked_sub(e, 1) AND f % 2 = 0"
        );
        let connection = connection();
        let checked = checked_statement(&format!("SELECT {} - 1", i64::MIN + 1)).unwrap();
        assert!(evaluate(&connection, checked.trim_start_matches("SELECT ")).is_ok());
        let checked = checked_statement(&format!("SELECT -({}) - 2", i64::MAX)).unwrap();
        let error = evaluate(&connection, checked.trim_start_matches("SELECT ")).unwrap_err();
        assert!(error.to_string().contains(OVERFLOW), "{checked}: {error}");
    }
}
