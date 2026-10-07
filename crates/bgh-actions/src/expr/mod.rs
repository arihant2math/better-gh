//! GitHub Actions `${{ }}` expression engine.
//!
//! Implements the expression language described in GitHub's "Evaluate
//! expressions in workflows and actions": literals, context lookups,
//! property / index / object-filter access, `! < <= > >= == != && ||`, and
//! the built-in functions (`contains`, `startsWith`, `endsWith`, `format`,
//! `join`, `toJSON`, `fromJSON`, `hashFiles`, `success`, `failure`,
//! `always`, `cancelled`).
//!
//! Semantic notes:
//! * Arrays and objects compare by reference in GitHub. Values here are owned
//!   copies without identity, so `==` between two arrays/objects is always
//!   `false` (and `!=` always `true`); `contains(array, item)` therefore never
//!   matches an array/object item.
//! * NaN and ±Infinity have no JSON representation; when such a number is
//!   returned as a raw [`Value`] (via [`evaluate`] / [`evaluate_template`]) it
//!   becomes `null`. String coercion (`interpolate`, `format`, ...) renders
//!   them as `NaN` / `Infinity` like JavaScript.
//! * Integral numbers are returned as JSON integers (`1`, not `1.0`).
//! * Literal keywords (`true`, `false`, `null`, `NaN`, `Infinity`) are
//!   case-sensitive; function names, context names and property names are
//!   case-insensitive.

mod eval;
mod lexer;
mod parser;
#[cfg(test)]
mod tests;

pub use parser::{MAX_AST_DEPTH, MAX_EXPRESSION_LENGTH, MAX_NESTING};
pub use serde_json::Value;

/// Expression syntax tree.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    /// Top-level named context, e.g. `github`.
    Context(String),
    /// `base.name`
    Property(Box<Expr>, String),
    /// `base[index]`
    Index(Box<Expr>, Box<Expr>),
    /// Object filter: `base.*` or `base[*]`.
    Wildcard(Box<Expr>),
    Not(Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Compare(CompareOp, Box<Expr>, Box<Expr>),
    Call(Function, Vec<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// Built-in functions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Function {
    Contains,
    StartsWith,
    EndsWith,
    Format,
    Join,
    ToJson,
    FromJson,
    HashFiles,
    Success,
    Failure,
    Always,
    Cancelled,
}

impl Function {
    pub const ALL: [Function; 12] = [
        Function::Contains,
        Function::StartsWith,
        Function::EndsWith,
        Function::Format,
        Function::Join,
        Function::ToJson,
        Function::FromJson,
        Function::HashFiles,
        Function::Success,
        Function::Failure,
        Function::Always,
        Function::Cancelled,
    ];

    /// Canonical (documented) name.
    pub fn name(self) -> &'static str {
        match self {
            Function::Contains => "contains",
            Function::StartsWith => "startsWith",
            Function::EndsWith => "endsWith",
            Function::Format => "format",
            Function::Join => "join",
            Function::ToJson => "toJSON",
            Function::FromJson => "fromJSON",
            Function::HashFiles => "hashFiles",
            Function::Success => "success",
            Function::Failure => "failure",
            Function::Always => "always",
            Function::Cancelled => "cancelled",
        }
    }

    /// Case-insensitive lookup by name.
    pub fn from_name(name: &str) -> Option<Function> {
        Self::ALL
            .into_iter()
            .find(|f| f.name().eq_ignore_ascii_case(name))
    }

    /// `(min, max)` argument count; `None` max means variadic.
    pub fn arity(self) -> (usize, Option<usize>) {
        match self {
            Function::Contains | Function::StartsWith | Function::EndsWith => (2, Some(2)),
            Function::Format | Function::HashFiles => (1, None),
            Function::Join => (1, Some(2)),
            Function::ToJson | Function::FromJson => (1, Some(1)),
            Function::Success | Function::Failure | Function::Always | Function::Cancelled => {
                (0, Some(0))
            }
        }
    }

    /// Whether this is one of the job status check functions.
    pub fn is_status(self) -> bool {
        matches!(
            self,
            Function::Success | Function::Failure | Function::Always | Function::Cancelled
        )
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ExprError {
    #[error("unexpected character '{ch}' at position {pos} in expression: {expr}")]
    UnexpectedChar { ch: char, pos: usize, expr: String },
    #[error("unterminated string literal starting at position {pos} in expression: {expr}")]
    UnterminatedString { pos: usize, expr: String },
    #[error("invalid number '{token}' at position {pos} in expression: {expr}")]
    InvalidNumber {
        token: String,
        pos: usize,
        expr: String,
    },
    #[error("unexpected token '{token}' at position {pos} in expression: {expr}")]
    UnexpectedToken {
        token: String,
        pos: usize,
        expr: String,
    },
    #[error("unexpected end of expression: {expr}")]
    UnexpectedEnd { expr: String },
    #[error("empty expression: '{expr}'")]
    EmptyExpression { expr: String },
    #[error("unrecognized function '{name}' at position {pos} in expression: {expr}")]
    UnknownFunction {
        name: String,
        pos: usize,
        expr: String,
    },
    #[error("function '{name}' expects {expected} argument(s) but got {got} in expression: {expr}")]
    ArgumentCount {
        name: String,
        expected: String,
        got: usize,
        expr: String,
    },
    #[error("expression exceeds the maximum depth of {max}: {expr}")]
    TooDeep { max: usize, expr: String },
    #[error("expression is {len} bytes long; the maximum is {max}")]
    TooLong { len: usize, max: usize },
    #[error("unclosed expression starting at position {pos} (missing '}}}}') in: {template}")]
    UnclosedExpression { pos: usize, template: String },
    #[error("invalid format string '{format}': {reason}")]
    InvalidFormat { format: String, reason: String },
    #[error(
        "format string '{format}' references argument {{{index}}} but only {count} argument(s) were supplied"
    )]
    FormatArgument {
        format: String,
        index: usize,
        count: usize,
    },
    #[error("fromJSON: invalid JSON '{input}': {message}")]
    InvalidJson { input: String, message: String },
    /// Error raised by a function implementation (e.g. `hashFiles`).
    #[error("{message}")]
    Function { function: String, message: String },
}

/// Runtime status used by success()/failure()/always()/cancelled().
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum JobStatus {
    #[default]
    Success,
    Failure,
    Cancelled,
}

pub trait Context {
    /// Top-level named context (github, env, vars, secrets, matrix, needs,
    /// steps, runner, job, inputs, strategy, ...). Case-insensitive name;
    /// return None if unknown/unavailable (evaluates to null).
    fn lookup(&self, name: &str) -> Option<Value>;

    /// Status of the current job/step chain (default Success).
    fn status(&self) -> JobStatus {
        JobStatus::Success
    }

    /// hashFiles(patterns...) implementation.
    fn hash_files(&self, patterns: &[String]) -> Result<String, ExprError> {
        let _ = patterns;
        Err(ExprError::Function {
            function: "hashFiles".to_string(),
            message: "hashFiles is not available in this context".to_string(),
        })
    }
}

/// Simple map-backed context for tests and server-side evaluation.
#[derive(Debug, Clone, Default)]
pub struct MapContext {
    pub contexts: serde_json::Map<String, Value>,
    pub status: JobStatus,
}

impl MapContext {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, name: &str, v: Value) -> Self {
        self.contexts.insert(name.to_string(), v);
        self
    }

    pub fn with_status(mut self, status: JobStatus) -> Self {
        self.status = status;
        self
    }
}

impl Context for MapContext {
    fn lookup(&self, name: &str) -> Option<Value> {
        eval::get_ci(&self.contexts, name).cloned()
    }

    fn status(&self) -> JobStatus {
        self.status
    }
}

/// Parse a bare expression (no `${{ }}`).
pub fn parse(src: &str) -> Result<Expr, ExprError> {
    parser::parse(src)
}

pub fn evaluate(expr: &Expr, ctx: &dyn Context) -> Result<Value, ExprError> {
    Ok(eval::eval(expr, ctx)?.into_value())
}

/// Parse+evaluate a bare expression (no ${{ }}).
pub fn eval_str(src: &str, ctx: &dyn Context) -> Result<Value, ExprError> {
    evaluate(&parse(src)?, ctx)
}

/// Locate the first `${{ ... }}` at or after byte `from`. Returns
/// `(start of "${{", start of "}}")`.
fn next_expression(template: &str, from: usize) -> Result<Option<(usize, usize)>, ExprError> {
    let Some(rel) = template[from..].find("${{") else {
        return Ok(None);
    };
    let start = from + rel;
    let close = lexer::find_closing(template, start + 3)?;
    Ok(Some((start, close)))
}

/// Replace every ${{ ... }} in `template` with the string-coerced result.
/// Text outside is kept. `}}` inside string literals does not end the
/// expression.
pub fn interpolate(template: &str, ctx: &dyn Context) -> Result<String, ExprError> {
    let mut out = String::with_capacity(template.len());
    let mut cursor = 0;
    while let Some((start, close)) = next_expression(template, cursor)? {
        out.push_str(&template[cursor..start]);
        let inner = &template[start + 3..close];
        out.push_str(&eval::eval(&parse(inner)?, ctx)?.display());
        cursor = close + 2;
    }
    out.push_str(&template[cursor..]);
    Ok(out)
}

/// If `s` (trimmed) is exactly one `${{ expr }}`, return the inner source.
fn single_expression(s: &str) -> Result<Option<&str>, ExprError> {
    let t = s.trim();
    if !t.starts_with("${{") {
        return Ok(None);
    }
    let close = lexer::find_closing(t, 3)?;
    Ok((close + 2 == t.len()).then(|| &t[3..close]))
}

/// If `template` (trimmed) is exactly one ${{ expr }}, return its raw Value
/// (object/array/number/...); otherwise interpolate() and return
/// Value::String.
pub fn evaluate_template(template: &str, ctx: &dyn Context) -> Result<Value, ExprError> {
    match single_expression(template)? {
        Some(inner) => eval_str(inner, ctx),
        None => interpolate(template, ctx).map(Value::String),
    }
}

/// Recursively apply evaluate_template to every string inside a JSON value
/// (object keys untouched).
pub fn evaluate_value(v: &Value, ctx: &dyn Context) -> Result<Value, ExprError> {
    Ok(match v {
        Value::String(s) if contains_expression(s) => evaluate_template(s, ctx)?,
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|i| evaluate_value(i, ctx))
                .collect::<Result<_, _>>()?,
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| Ok((k.clone(), evaluate_value(v, ctx)?)))
                .collect::<Result<_, ExprError>>()?,
        ),
        other => other.clone(),
    })
}

/// Whether the expression calls one of the status functions anywhere.
fn has_status_call(expr: &Expr) -> bool {
    match expr {
        Expr::Null | Expr::Bool(_) | Expr::Number(_) | Expr::String(_) | Expr::Context(_) => false,
        Expr::Property(b, _) | Expr::Wildcard(b) | Expr::Not(b) => has_status_call(b),
        Expr::Index(a, b) | Expr::And(a, b) | Expr::Or(a, b) | Expr::Compare(_, a, b) => {
            has_status_call(a) || has_status_call(b)
        }
        Expr::Call(f, args) => f.is_status() || args.iter().any(has_status_call),
    }
}

/// `if:` semantics: optional surrounding ${{ }} stripped; empty =>
/// success(); if the expression contains no call to a status function
/// (success/failure/always/cancelled, case-insensitive), it is evaluated as
/// `success() && (expr)`. Result is truthiness.
///
/// A condition that contains `${{ }}` but is not a single wrapped expression
/// (e.g. `${{ a }} && ${{ b }}`) is interpolated into a string, as GitHub
/// does, and that string's truthiness is used (still gated by success()).
pub fn evaluate_condition(src: &str, ctx: &dyn Context) -> Result<bool, ExprError> {
    let success = ctx.status() == JobStatus::Success;
    let inner = match single_expression(src)? {
        Some(inner) => inner,
        None if contains_expression(src) => {
            return Ok(success && !interpolate(src, ctx)?.is_empty());
        }
        None => src,
    };
    if inner.trim().is_empty() {
        return Ok(success);
    }
    let expr = parse(inner)?;
    let expr = if has_status_call(&expr) {
        expr
    } else {
        Expr::And(
            Box::new(Expr::Call(Function::Success, Vec::new())),
            Box::new(expr),
        )
    };
    Ok(eval::eval(&expr, ctx)?.truthy())
}

/// Whether `s` contains `${{`.
pub fn contains_expression(s: &str) -> bool {
    s.contains("${{")
}

/// Truthiness: false, 0, -0, NaN, "", null are falsy; everything else
/// (including empty arrays/objects) is truthy.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// String coercion used by interpolate: null -> "", bools "true"/"false",
/// numbers like JavaScript, strings as-is, arrays -> "Array", objects ->
/// "Object".
pub fn to_display_string(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => eval::js_number_string(n.as_f64().unwrap_or(f64::NAN)),
        Value::String(s) => s.clone(),
        Value::Array(_) => "Array".to_string(),
        Value::Object(_) => "Object".to_string(),
    }
}
