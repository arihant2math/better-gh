//! Tree-walking evaluator and built-in functions.

use std::cmp::Ordering;

use serde_json::{Map, Value};

use super::{CompareOp, Context, Expr, ExprError, Function, JobStatus};

/// Internal evaluation value. Keeps numbers as `f64` (so NaN/Infinity survive
/// until the end) and distinguishes the result of a `*` filter.
#[derive(Debug, Clone)]
pub(crate) enum V {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Value>),
    Obj(Map<String, Value>),
    /// Result of an object filter (`.*` / `[*]`); behaves like an array but
    /// further `.prop` / `[i]` / `*` map over its elements.
    Filtered(Vec<Value>),
}

impl V {
    pub(crate) fn from_value(v: Value) -> V {
        match v {
            Value::Null => V::Null,
            Value::Bool(b) => V::Bool(b),
            Value::Number(n) => V::Num(n.as_f64().unwrap_or(f64::NAN)),
            Value::String(s) => V::Str(s),
            Value::Array(a) => V::Arr(a),
            Value::Object(o) => V::Obj(o),
        }
    }

    pub(crate) fn into_value(self) -> Value {
        match self {
            V::Null => Value::Null,
            V::Bool(b) => Value::Bool(b),
            V::Num(n) => number_value(n),
            V::Str(s) => Value::String(s),
            V::Arr(a) | V::Filtered(a) => Value::Array(a),
            V::Obj(o) => Value::Object(o),
        }
    }

    pub(crate) fn truthy(&self) -> bool {
        match self {
            V::Null => false,
            V::Bool(b) => *b,
            V::Num(n) => !(*n == 0.0 || n.is_nan()),
            V::Str(s) => !s.is_empty(),
            V::Arr(_) | V::Obj(_) | V::Filtered(_) => true,
        }
    }

    pub(crate) fn display(&self) -> String {
        match self {
            V::Null => String::new(),
            V::Bool(b) => b.to_string(),
            V::Num(n) => js_number_string(*n),
            V::Str(s) => s.clone(),
            V::Arr(_) | V::Filtered(_) => "Array".to_string(),
            V::Obj(_) => "Object".to_string(),
        }
    }

    fn to_number(&self) -> f64 {
        match self {
            V::Null => 0.0,
            V::Bool(b) => f64::from(u8::from(*b)),
            V::Num(n) => *n,
            V::Str(s) => string_to_number(s),
            V::Arr(_) | V::Obj(_) | V::Filtered(_) => f64::NAN,
        }
    }

    fn is_container(&self) -> bool {
        matches!(self, V::Arr(_) | V::Obj(_) | V::Filtered(_))
    }
}

/// Convert an `f64` to JSON: integral values become integers (so `1` is not
/// rendered `1.0`); NaN / ±Infinity have no JSON form and become `null`.
pub(crate) fn number_value(n: f64) -> Value {
    if !n.is_finite() {
        return Value::Null;
    }
    if n.fract() == 0.0 && n.abs() < 9_007_199_254_740_992.0 {
        return Value::from(n as i64);
    }
    serde_json::Number::from_f64(n).map_or(Value::Null, Value::Number)
}

/// Format a number the way JavaScript's `Number.prototype.toString` does.
pub(crate) fn js_number_string(n: f64) -> String {
    if n.is_nan() {
        return "NaN".into();
    }
    if n.is_infinite() {
        return if n > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    if n == 0.0 {
        return "0".into();
    }
    let sign = if n < 0.0 { "-" } else { "" };
    // `{:e}` gives the shortest round-tripping digits: "d.ddde<exp>".
    let sci = format!("{:e}", n.abs());
    let (mantissa, exp) = sci.split_once('e').expect("exponent format");
    let exp: i32 = exp.parse().expect("exponent");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let point = exp + 1; // position of the decimal point relative to digits
    let body = if k <= point && point <= 21 {
        format!("{digits}{}", "0".repeat((point - k) as usize))
    } else if 0 < point && point <= 21 {
        let (a, b) = digits.split_at(point as usize);
        format!("{a}.{b}")
    } else if -6 < point && point <= 0 {
        format!("0.{}{digits}", "0".repeat((-point) as usize))
    } else {
        let e_sign = if point - 1 < 0 { '-' } else { '+' };
        let (first, rest) = digits.split_at(1);
        let frac = if rest.is_empty() {
            String::new()
        } else {
            format!(".{rest}")
        };
        format!("{first}{frac}e{e_sign}{}", (point - 1).abs())
    };
    format!("{sign}{body}")
}

/// String to number coercion: trimmed, empty → 0, decimal / hex /
/// `Infinity`; anything else NaN.
pub(crate) fn string_to_number(s: &str) -> f64 {
    let t = s.trim();
    if t.is_empty() {
        return 0.0;
    }
    let (neg, body) = match t.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let mag = if body == "Infinity" {
        f64::INFINITY
    } else if let Some(hex) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        match u64::from_str_radix(hex, 16) {
            Ok(v) if !hex.starts_with('+') => v as f64,
            _ => f64::NAN,
        }
    } else if body.starts_with(['+', '-']) {
        f64::NAN
    } else {
        super::lexer::parse_decimal(body).unwrap_or(f64::NAN)
    };
    if neg { -mag } else { mag }
}

/// Property lookup: exact key first, then case-insensitive.
pub(crate) fn get_ci<'a>(map: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    map.get(key).or_else(|| {
        map.iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key) || fold(k) == fold(key))
            .map(|(_, v)| v)
    })
}

/// Ordinal ignore-case folding: each char is replaced by its simple
/// (single-char) uppercase mapping, like .NET `OrdinalIgnoreCase`.
pub(crate) fn fold(s: &str) -> String {
    s.chars()
        .map(|c| {
            let mut up = c.to_uppercase();
            match (up.next(), up.next()) {
                (Some(u), None) => u,
                _ => c,
            }
        })
        .collect()
}

fn str_eq_ci(a: &str, b: &str) -> bool {
    a == b || fold(a) == fold(b)
}

/// Loose equality (`==`).
///
/// Arrays and objects are compared by reference in GitHub Actions. Values here
/// are owned copies with no identity, so two arrays/objects are never equal.
fn loose_eq(a: &V, b: &V) -> bool {
    if a.is_container() || b.is_container() {
        return false;
    }
    match (a, b) {
        (V::Null, V::Null) => true,
        (V::Bool(x), V::Bool(y)) => x == y,
        (V::Num(x), V::Num(y)) => x == y,
        (V::Str(x), V::Str(y)) => str_eq_ci(x, y),
        _ => a.to_number() == b.to_number(),
    }
}

fn loose_cmp(a: &V, b: &V) -> Option<Ordering> {
    if a.is_container() || b.is_container() {
        return None;
    }
    if let (V::Str(x), V::Str(y)) = (a, b) {
        return Some(fold(x).cmp(&fold(y)));
    }
    a.to_number().partial_cmp(&b.to_number())
}

fn index_value<'a>(base: &'a Value, index: &V) -> Option<&'a Value> {
    match base {
        Value::Object(map) => {
            let key = match index {
                V::Str(s) => s.clone(),
                other => other.display(),
            };
            get_ci(map, &key)
        }
        Value::Array(items) => {
            let n = index.to_number();
            if n.is_finite() && n >= 0.0 {
                items.get(n.trunc() as usize)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn wildcard_items(v: V) -> Vec<Value> {
    match v {
        V::Arr(a) => a,
        V::Obj(o) => o.into_iter().map(|(_, v)| v).collect(),
        V::Filtered(items) => items
            .into_iter()
            .flat_map(|item| match item {
                Value::Array(a) => a,
                Value::Object(o) => o.into_iter().map(|(_, v)| v).collect(),
                _ => Vec::new(),
            })
            .collect(),
        _ => Vec::new(),
    }
}

pub(crate) fn eval(expr: &Expr, ctx: &dyn Context) -> Result<V, ExprError> {
    Ok(match expr {
        Expr::Null => V::Null,
        Expr::Bool(b) => V::Bool(*b),
        Expr::Number(n) => V::Num(*n),
        Expr::String(s) => V::Str(s.clone()),
        Expr::Context(name) => ctx.lookup(name).map_or(V::Null, V::from_value),
        Expr::Property(base, name) => match eval(base, ctx)? {
            V::Obj(map) => get_ci(&map, name).cloned().map_or(V::Null, V::from_value),
            V::Filtered(items) => V::Filtered(
                items
                    .iter()
                    .filter_map(|item| item.as_object().and_then(|m| get_ci(m, name)))
                    .cloned()
                    .collect(),
            ),
            _ => V::Null,
        },
        Expr::Index(base, index) => {
            let base = eval(base, ctx)?;
            let index = eval(index, ctx)?;
            match base {
                V::Filtered(items) => V::Filtered(
                    items
                        .iter()
                        .filter_map(|item| index_value(item, &index))
                        .cloned()
                        .collect(),
                ),
                V::Arr(a) => index_value(&Value::Array(a), &index)
                    .cloned()
                    .map_or(V::Null, V::from_value),
                V::Obj(o) => index_value(&Value::Object(o), &index)
                    .cloned()
                    .map_or(V::Null, V::from_value),
                _ => V::Null,
            }
        }
        Expr::Wildcard(base) => V::Filtered(wildcard_items(eval(base, ctx)?)),
        Expr::Not(inner) => V::Bool(!eval(inner, ctx)?.truthy()),
        Expr::And(a, b) => {
            let left = eval(a, ctx)?;
            if left.truthy() { eval(b, ctx)? } else { left }
        }
        Expr::Or(a, b) => {
            let left = eval(a, ctx)?;
            if left.truthy() { left } else { eval(b, ctx)? }
        }
        Expr::Compare(op, a, b) => {
            let l = eval(a, ctx)?;
            let r = eval(b, ctx)?;
            V::Bool(match op {
                CompareOp::Eq => loose_eq(&l, &r),
                CompareOp::Ne => !loose_eq(&l, &r),
                CompareOp::Lt => loose_cmp(&l, &r) == Some(Ordering::Less),
                CompareOp::Le => {
                    matches!(loose_cmp(&l, &r), Some(Ordering::Less | Ordering::Equal))
                }
                CompareOp::Gt => loose_cmp(&l, &r) == Some(Ordering::Greater),
                CompareOp::Ge => {
                    matches!(loose_cmp(&l, &r), Some(Ordering::Greater | Ordering::Equal))
                }
            })
        }
        Expr::Call(func, args) => call(*func, args, ctx)?,
    })
}

fn call(func: Function, args: &[Expr], ctx: &dyn Context) -> Result<V, ExprError> {
    let arg = |i: usize| eval(&args[i], ctx);
    Ok(match func {
        Function::Success => V::Bool(ctx.status() == JobStatus::Success),
        Function::Failure => V::Bool(ctx.status() == JobStatus::Failure),
        Function::Cancelled => V::Bool(ctx.status() == JobStatus::Cancelled),
        Function::Always => V::Bool(true),
        Function::Contains => {
            let search = arg(0)?;
            let item = arg(1)?;
            V::Bool(match search {
                V::Arr(items) | V::Filtered(items) => items
                    .into_iter()
                    .any(|el| loose_eq(&V::from_value(el), &item)),
                other => fold(&other.display()).contains(&fold(&item.display())),
            })
        }
        Function::StartsWith => {
            let s = fold(&arg(0)?.display());
            V::Bool(s.starts_with(&fold(&arg(1)?.display())))
        }
        Function::EndsWith => {
            let s = fold(&arg(0)?.display());
            V::Bool(s.ends_with(&fold(&arg(1)?.display())))
        }
        Function::Format => {
            let fmt = arg(0)?.display();
            let rest = args[1..]
                .iter()
                .map(|a| eval(a, ctx).map(|v| v.display()))
                .collect::<Result<Vec<_>, _>>()?;
            V::Str(format_string(&fmt, &rest)?)
        }
        Function::Join => {
            let sep = if args.len() > 1 {
                arg(1)?.display()
            } else {
                ",".to_string()
            };
            match arg(0)? {
                V::Arr(items) | V::Filtered(items) => V::Str(
                    items
                        .into_iter()
                        .map(|v| V::from_value(v).display())
                        .collect::<Vec<_>>()
                        .join(&sep),
                ),
                other => V::Str(other.display()),
            }
        }
        Function::ToJson => {
            let v = arg(0)?.into_value();
            V::Str(serde_json::to_string_pretty(&v).expect("serializing a Value cannot fail"))
        }
        Function::FromJson => {
            let input = arg(0)?.display();
            let parsed: Value =
                serde_json::from_str(&input).map_err(|e| ExprError::InvalidJson {
                    input: truncate(&input, 100),
                    message: e.to_string(),
                })?;
            V::from_value(parsed)
        }
        Function::HashFiles => {
            let patterns = args
                .iter()
                .map(|a| eval(a, ctx).map(|v| v.display()))
                .collect::<Result<Vec<_>, _>>()?;
            V::Str(ctx.hash_files(&patterns)?)
        }
    })
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}...", s.chars().take(max).collect::<String>())
    }
}

/// `format()` implementation: `{N}` placeholders, `{{` / `}}` escapes.
pub(crate) fn format_string(fmt: &str, args: &[String]) -> Result<String, ExprError> {
    let invalid = |reason: String| ExprError::InvalidFormat {
        format: fmt.to_string(),
        reason,
    };
    let chars: Vec<char> = fmt.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '{' if chars.get(i + 1) == Some(&'{') => {
                out.push('{');
                i += 2;
            }
            '{' => {
                let mut j = i + 1;
                while chars.get(j).is_some_and(|c| c.is_ascii_digit()) {
                    j += 1;
                }
                if j == i + 1 || chars.get(j) != Some(&'}') {
                    return Err(invalid(format!("invalid placeholder at position {i}")));
                }
                let digits: String = chars[i + 1..j].iter().collect();
                let index: usize = digits
                    .parse()
                    .map_err(|_| invalid(format!("invalid placeholder index '{digits}'")))?;
                let value = args.get(index).ok_or_else(|| ExprError::FormatArgument {
                    format: fmt.to_string(),
                    index,
                    count: args.len(),
                })?;
                out.push_str(value);
                i = j + 1;
            }
            '}' if chars.get(i + 1) == Some(&'}') => {
                out.push('}');
                i += 2;
            }
            '}' => return Err(invalid(format!("unescaped '}}' at position {i}"))),
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    Ok(out)
}
