use serde_json::json;

use super::*;

fn ctx() -> MapContext {
    MapContext::new()
        .with(
            "github",
            json!({
                "ref": "refs/heads/main",
                "event_name": "push",
                "repository": "octo/hello",
                "run_number": 42,
                "event": {
                    "issues": [
                        {"number": 1, "labels": [{"name": "bug"}, {"name": "ui"}]},
                        {"number": 2, "labels": [{"name": "docs"}]},
                        {"number": 3}
                    ],
                    "Head_Commit": {"Message": "fix it"},
                    "payload": {"a": {"x": 1}, "b": {"x": 2}, "c": {"y": 3}}
                }
            }),
        )
        .with(
            "steps",
            json!({"my-step": {"outputs": {"result-value": "ok"}, "outcome": "success"}}),
        )
        .with(
            "needs",
            json!({"build-1": {"result": "success", "outputs": {"n": "7"}}}),
        )
        .with("env", json!({"NAME": "World", "EMPTY": "", "ZERO": "0"}))
        .with(
            "matrix",
            json!({"os": "ubuntu-latest", "node": [18, 20], "version": 1.5}),
        )
}

fn ev(src: &str) -> Value {
    eval_str(src, &ctx()).unwrap_or_else(|e| panic!("{src}: {e}"))
}

fn ev_err(src: &str) -> ExprError {
    eval_str(src, &ctx()).expect_err(src)
}

fn status_ctx(status: JobStatus) -> MapContext {
    let mut c = ctx();
    c.status = status;
    c
}

// ---------------------------------------------------------------- literals

#[test]
fn literal_null_bool() {
    assert_eq!(ev("null"), Value::Null);
    assert_eq!(ev("true"), json!(true));
    assert_eq!(ev("false"), json!(false));
}

#[test]
fn literal_integers_and_floats() {
    assert_eq!(ev("42"), json!(42));
    assert_eq!(ev("1.5"), json!(1.5));
    assert_eq!(ev("-7"), json!(-7));
    assert_eq!(ev("-2.5"), json!(-2.5));
    assert_eq!(ev(".5"), json!(0.5));
    assert_eq!(ev("1."), json!(1));
}

#[test]
fn literal_hex_and_exponent() {
    assert_eq!(ev("0xff"), json!(255));
    assert_eq!(ev("0XFF"), json!(255));
    assert_eq!(ev("-0x10"), json!(-16));
    assert_eq!(ev("2.99e-2"), json!(0.0299));
    assert_eq!(ev("1e3"), json!(1000));
    assert_eq!(ev("1E+2"), json!(100));
}

#[test]
fn literal_nan_infinity() {
    assert_eq!(interpolate("${{ NaN }}", &ctx()).unwrap(), "NaN");
    assert_eq!(interpolate("${{ Infinity }}", &ctx()).unwrap(), "Infinity");
    assert_eq!(
        interpolate("${{ -Infinity }}", &ctx()).unwrap(),
        "-Infinity"
    );
    // No JSON form: raw value is null.
    assert_eq!(ev("NaN"), Value::Null);
}

#[test]
fn literal_strings_and_escapes() {
    assert_eq!(ev("'hello'"), json!("hello"));
    assert_eq!(ev("''"), json!(""));
    assert_eq!(ev("'it''s'"), json!("it's"));
    assert_eq!(ev("''''"), json!("'"));
    assert_eq!(ev("'a \"b\" }} {{ c'"), json!("a \"b\" }} {{ c"));
    assert_eq!(ev("'héllo ☃'"), json!("héllo ☃"));
}

#[test]
fn whitespace_is_ignored() {
    assert_eq!(ev("  \t1\n==\r\n 1  "), json!(true));
}

// ---------------------------------------------------------------- lexer errors

#[test]
fn lexer_unterminated_string() {
    let e = ev_err("'abc");
    assert!(matches!(e, ExprError::UnterminatedString { pos: 0, .. }));
    assert!(e.to_string().contains("'abc"));
}

#[test]
fn lexer_unexpected_char() {
    assert!(matches!(
        ev_err("1 = 1"),
        ExprError::UnexpectedChar {
            ch: '=',
            pos: 2,
            ..
        }
    ));
    assert!(matches!(
        ev_err("a & b"),
        ExprError::UnexpectedChar { ch: '&', .. }
    ));
    assert!(matches!(
        ev_err("a | b"),
        ExprError::UnexpectedChar { ch: '|', .. }
    ));
    assert!(matches!(
        ev_err("\"x\""),
        ExprError::UnexpectedChar { ch: '"', .. }
    ));
    assert!(matches!(
        ev_err("a + b"),
        ExprError::UnexpectedChar { ch: '+', .. }
    ));
}

#[test]
fn lexer_invalid_numbers() {
    for src in ["1.2.3", "12abc", "0x", "0xfg", "1e", "1-2"] {
        match ev_err(src) {
            ExprError::InvalidNumber { token, .. } => assert_eq!(token, src),
            other => panic!("{src}: {other:?}"),
        }
    }
}

#[test]
fn lexer_error_message_includes_expression() {
    let msg = ev_err("github.ref == 'x' ~").to_string();
    assert!(msg.contains("position 18"), "{msg}");
    assert!(msg.contains("github.ref == 'x' ~"), "{msg}");
}

#[test]
fn keywords_are_case_sensitive() {
    // `True` is not a keyword, so it is a (missing) context -> null.
    assert_eq!(ev("True"), Value::Null);
    assert_eq!(ev("NULL"), Value::Null);
}

// ---------------------------------------------------------------- parser errors

#[test]
fn parse_errors() {
    assert!(matches!(ev_err("1 =="), ExprError::UnexpectedEnd { .. }));
    assert!(matches!(ev_err("(1"), ExprError::UnexpectedEnd { .. }));
    assert!(matches!(
        ev_err("1 2"),
        ExprError::UnexpectedToken { pos: 2, .. }
    ));
    assert!(matches!(ev_err(")"), ExprError::UnexpectedToken { .. }));
    assert!(matches!(ev_err("a."), ExprError::UnexpectedEnd { .. }));
    assert!(matches!(ev_err("a.'b'"), ExprError::UnexpectedToken { .. }));
    assert!(matches!(ev_err("a[1"), ExprError::UnexpectedEnd { .. }));
    assert!(matches!(
        ev_err("format('a',)"),
        ExprError::UnexpectedToken { .. }
    ));
    assert!(matches!(ev_err(""), ExprError::EmptyExpression { .. }));
    assert!(matches!(ev_err("   "), ExprError::EmptyExpression { .. }));
}

#[test]
fn unknown_function() {
    let e = ev_err("frobnicate(1)");
    assert!(matches!(&e, ExprError::UnknownFunction { name, pos: 0, .. } if name == "frobnicate"));
    assert!(e.to_string().contains("frobnicate"));
}

#[test]
fn wrong_arity() {
    for src in [
        "contains('a')",
        "contains('a', 'b', 'c')",
        "startsWith('a')",
        "endsWith()",
        "format()",
        "join()",
        "join('a', 'b', 'c')",
        "toJSON()",
        "fromJSON('1', '2')",
        "hashFiles()",
        "success(1)",
        "always(true)",
    ] {
        assert!(
            matches!(ev_err(src), ExprError::ArgumentCount { .. }),
            "{src}"
        );
    }
    let msg = ev_err("contains('a')").to_string();
    assert!(
        msg.contains("contains") && msg.contains("2") && msg.contains("got 1"),
        "{msg}"
    );
}

#[test]
fn nesting_limit() {
    let deep = format!("{}1{}", "(".repeat(60), ")".repeat(60));
    assert!(matches!(ev_err(&deep), ExprError::TooDeep { .. }));
    let ok = format!("{}1{}", "(".repeat(20), ")".repeat(20));
    assert_eq!(ev(&ok), json!(1));
    let chain = vec!["true"; 1000].join(" && ");
    assert!(matches!(ev_err(&chain), ExprError::TooDeep { .. }));
    let chain = vec!["true"; 100].join(" && ");
    assert_eq!(ev(&chain), json!(true));
    let long = "a".repeat(MAX_EXPRESSION_LENGTH + 1);
    assert!(matches!(ev_err(&long), ExprError::TooLong { .. }));
}

#[test]
fn ast_shapes() {
    assert_eq!(
        parse("a.b[0]").unwrap(),
        Expr::Index(
            Box::new(Expr::Property(
                Box::new(Expr::Context("a".into())),
                "b".into()
            )),
            Box::new(Expr::Number(0.0))
        )
    );
    assert_eq!(
        parse("!a || b && c").unwrap(),
        Expr::Or(
            Box::new(Expr::Not(Box::new(Expr::Context("a".into())))),
            Box::new(Expr::And(
                Box::new(Expr::Context("b".into())),
                Box::new(Expr::Context("c".into()))
            ))
        )
    );
    assert_eq!(parse("a.*").unwrap(), parse("a[*]").unwrap());
    assert_eq!(
        parse("SUCCESS()").unwrap(),
        Expr::Call(Function::Success, vec![])
    );
}

// ---------------------------------------------------------------- precedence

#[test]
fn precedence_and_binds_tighter_than_or() {
    assert_eq!(ev("true || false && false"), json!(true));
    assert_eq!(ev("(true || false) && false"), json!(false));
}

#[test]
fn precedence_equality_vs_and() {
    assert_eq!(ev("1 == 1 && 2 == 2"), json!(true));
    assert_eq!(ev("1 == 2 || 'a' == 'A'"), json!(true));
}

#[test]
fn precedence_comparison_vs_equality() {
    // (1 < 2) == true
    assert_eq!(ev("1 < 2 == true"), json!(true));
    assert_eq!(ev("2 > 1 != false"), json!(true));
}

#[test]
fn precedence_not() {
    assert_eq!(ev("!false && true"), json!(true));
    assert_eq!(ev("!(1 == 1)"), json!(false));
    assert_eq!(ev("!!'x'"), json!(true));
    assert_eq!(ev("!''"), json!(true));
    // `!` binds tighter than `==`: (!1) == false
    assert_eq!(ev("!1 == false"), json!(true));
}

#[test]
fn precedence_postfix_over_not() {
    assert_eq!(ev("!github.missing"), json!(true));
    assert_eq!(ev("!github.ref"), json!(false));
}

// ---------------------------------------------------------------- short-circuit

#[test]
fn or_returns_operand_value() {
    assert_eq!(ev("'' || 'default'"), json!("default"));
    assert_eq!(ev("'value' || 'default'"), json!("value"));
    assert_eq!(ev("env.MISSING || env.NAME"), json!("World"));
    assert_eq!(ev("0 || null"), Value::Null);
    assert_eq!(ev("false || 0"), json!(0));
}

#[test]
fn and_returns_operand_value() {
    assert_eq!(ev("null && x"), Value::Null);
    assert_eq!(ev("'a' && 'b'"), json!("b"));
    assert_eq!(ev("0 && 'b'"), json!(0));
    assert_eq!(ev("'' && 1"), json!(""));
    assert_eq!(ev("1 && matrix.node"), json!([18, 20]));
}

#[test]
fn ternary_idiom() {
    assert_eq!(
        ev("github.ref == 'refs/heads/main' && 'prod' || 'dev'"),
        json!("prod")
    );
    assert_eq!(
        ev("github.ref == 'refs/heads/x' && 'prod' || 'dev'"),
        json!("dev")
    );
}

#[test]
fn short_circuit_skips_errors() {
    // The right-hand side would error (hashFiles unavailable) if evaluated.
    assert_eq!(ev("false && hashFiles('x')"), json!(false));
    assert_eq!(ev("true || hashFiles('x')"), json!(true));
    assert!(matches!(
        ev_err("true && hashFiles('x')"),
        ExprError::Function { .. }
    ));
}

// ---------------------------------------------------------------- contexts / access

#[test]
fn context_lookup() {
    assert_eq!(ev("github.ref"), json!("refs/heads/main"));
    assert_eq!(ev("github.run_number"), json!(42));
    assert_eq!(
        ev("env"),
        json!({"NAME": "World", "EMPTY": "", "ZERO": "0"})
    );
}

#[test]
fn context_names_case_insensitive() {
    assert_eq!(ev("GITHUB.REF"), json!("refs/heads/main"));
    assert_eq!(ev("Env.name"), json!("World"));
}

#[test]
fn property_names_case_insensitive() {
    assert_eq!(ev("github.event.head_commit.message"), json!("fix it"));
    assert_eq!(ev("github.EVENT.Head_Commit.Message"), json!("fix it"));
}

#[test]
fn property_exact_match_preferred() {
    let c = MapContext::new().with("o", json!({"Key": 1, "key": 2}));
    assert_eq!(eval_str("o.key", &c).unwrap(), json!(2));
    assert_eq!(eval_str("o.Key", &c).unwrap(), json!(1));
    assert_eq!(eval_str("o.KEY", &c).unwrap(), json!(1));
}

#[test]
fn missing_values_are_null() {
    assert_eq!(ev("nosuch"), Value::Null);
    assert_eq!(ev("nosuch.a.b.c"), Value::Null);
    assert_eq!(ev("github.ref.length"), Value::Null);
    assert_eq!(ev("github.missing[0]"), Value::Null);
    assert_eq!(ev("matrix.node[5]"), Value::Null);
    assert_eq!(ev("matrix.node[-1]"), Value::Null);
    assert_eq!(ev("'str'[0]"), Value::Null);
}

#[test]
fn hyphenated_identifiers() {
    assert_eq!(ev("steps.my-step.outputs.result-value"), json!("ok"));
    assert_eq!(ev("needs.build-1.result"), json!("success"));
    assert_eq!(ev("needs.build-1.result == 'success'"), json!(true));
    assert_eq!(ev("steps.my-step.outcome"), json!("success"));
}

#[test]
fn keyword_as_property_name() {
    let c = MapContext::new().with("o", json!({"true": 1, "null": 2}));
    assert_eq!(eval_str("o.true", &c).unwrap(), json!(1));
    assert_eq!(eval_str("o.null", &c).unwrap(), json!(2));
}

#[test]
fn index_access() {
    assert_eq!(ev("matrix.node[0]"), json!(18));
    assert_eq!(ev("matrix.node[1.7]"), json!(20));
    assert_eq!(ev("matrix.node['1']"), json!(20));
    assert_eq!(ev("github['ref']"), json!("refs/heads/main"));
    assert_eq!(ev("github['REF']"), json!("refs/heads/main"));
    assert_eq!(ev("steps['my-step'].outputs['result-value']"), json!("ok"));
    assert_eq!(ev("github.event.issues[0].number"), json!(1));
}

#[test]
fn index_with_expression() {
    let c = ctx().with("k", json!("event_name"));
    assert_eq!(eval_str("github[k]", &c).unwrap(), json!("push"));
}

#[test]
fn postfix_on_call_and_parens() {
    assert_eq!(ev("fromJSON('{\"a\":{\"b\":[1,2]}}').a.b[1]"), json!(2));
    assert_eq!(ev("(github).ref"), json!("refs/heads/main"));
}

// ---------------------------------------------------------------- object filters

#[test]
fn filter_array_property() {
    assert_eq!(ev("github.event.issues.*.number"), json!([1, 2, 3]));
}

#[test]
fn filter_nested_flattening() {
    assert_eq!(
        ev("github.event.issues.*.labels.*.name"),
        json!(["bug", "ui", "docs"])
    );
}

#[test]
fn filter_bracket_star() {
    assert_eq!(ev("github.event.issues[*].number"), json!([1, 2, 3]));
}

#[test]
fn filter_missing_properties_skipped() {
    // issue 3 has no labels; not included as null.
    assert_eq!(
        ev("github.event.issues.*.labels"),
        json!([[{"name": "bug"}, {"name": "ui"}], [{"name": "docs"}]])
    );
}

#[test]
fn filter_object_values() {
    assert_eq!(ev("github.event.payload.*.x"), json!([1, 2]));
}

#[test]
fn filter_on_scalar_or_null_is_empty() {
    assert_eq!(ev("github.ref.*"), json!([]));
    assert_eq!(ev("nosuch.*.x"), json!([]));
}

#[test]
fn filter_with_index_and_functions() {
    assert_eq!(
        ev("github.event.issues.*.labels[0].name"),
        json!(["bug", "docs"])
    );
    assert_eq!(
        ev("contains(github.event.issues.*.labels.*.name, 'DOCS')"),
        json!(true)
    );
    assert_eq!(
        ev("join(github.event.issues.*.number, '+')"),
        json!("1+2+3")
    );
}

#[test]
fn filter_is_truthy_even_if_empty() {
    assert_eq!(ev("!nosuch.*"), json!(false));
}

// ---------------------------------------------------------------- coercion / comparison

#[test]
fn equality_coercions() {
    assert_eq!(ev("'1' == 1"), json!(true));
    assert_eq!(ev("null == 0"), json!(true));
    assert_eq!(ev("true == 1"), json!(true));
    assert_eq!(ev("false == 0"), json!(true));
    assert_eq!(ev("false == ''"), json!(true));
    assert_eq!(ev("null == ''"), json!(true));
    assert_eq!(ev("null == false"), json!(true));
    assert_eq!(ev("'  ' == 0"), json!(true));
    assert_eq!(ev("'0x10' == 16"), json!(true));
    assert_eq!(ev("' 2.5 ' == 2.5"), json!(true));
    assert_eq!(ev("'true' == true"), json!(false));
    assert_eq!(ev("null == null"), json!(true));
}

#[test]
fn string_equality_case_insensitive() {
    assert_eq!(ev("'abc' == 'ABC'"), json!(true));
    assert_eq!(ev("'abc' != 'ABC'"), json!(false));
    assert_eq!(ev("github.event_name == 'PUSH'"), json!(true));
    assert_eq!(ev("'Größe' == 'GRÖßE'"), json!(true));
    // Simple (1:1) case mapping only, like .NET OrdinalIgnoreCase.
    assert_eq!(ev("'straße' == 'STRASSE'"), json!(false));
}

#[test]
fn nan_comparisons() {
    assert_eq!(ev("NaN == NaN"), json!(false));
    assert_eq!(ev("NaN != NaN"), json!(true));
    assert_eq!(ev("'abc' == 0"), json!(false));
    assert_eq!(ev("'abc' != 0"), json!(true));
    assert_eq!(ev("'abc' < 1"), json!(false));
    assert_eq!(ev("'abc' >= 1"), json!(false));
    assert_eq!(ev("NaN < 1 || NaN > 1 || NaN == 1"), json!(false));
}

#[test]
fn container_equality_never_equal() {
    assert_eq!(ev("matrix.node == matrix.node"), json!(false));
    assert_eq!(ev("matrix.node != matrix.node"), json!(true));
    assert_eq!(ev("env == 'Object'"), json!(false));
    assert_eq!(ev("matrix.node == 0"), json!(false));
    assert_eq!(ev("matrix.node < 1"), json!(false));
}

#[test]
fn numeric_ordering() {
    assert_eq!(ev("1 < 2"), json!(true));
    assert_eq!(ev("2 <= 2"), json!(true));
    assert_eq!(ev("3 > 2.5"), json!(true));
    assert_eq!(ev("-1 >= 0"), json!(false));
    assert_eq!(ev("'10' > 9"), json!(true));
    assert_eq!(ev("true > false"), json!(true));
    assert_eq!(ev("null < 1"), json!(true));
    assert_eq!(ev("github.run_number >= 40"), json!(true));
}

#[test]
fn string_ordering_case_insensitive() {
    assert_eq!(ev("'a' < 'B'"), json!(true));
    assert_eq!(ev("'B' > 'a'"), json!(true));
    assert_eq!(ev("'abc' <= 'ABC'"), json!(true));
    assert_eq!(ev("'10' < '9'"), json!(true)); // string comparison, not numeric
}

// ---------------------------------------------------------------- truthiness / display

#[test]
fn truthiness() {
    for v in [json!(false), json!(0), json!(-0.0), json!(""), Value::Null] {
        assert!(!truthy(&v), "{v}");
    }
    for v in [
        json!(true),
        json!(1),
        json!(-1),
        json!("0"),
        json!("false"),
        json!([]),
        json!({}),
    ] {
        assert!(truthy(&v), "{v}");
    }
}

#[test]
fn display_strings() {
    assert_eq!(to_display_string(&Value::Null), "");
    assert_eq!(to_display_string(&json!(true)), "true");
    assert_eq!(to_display_string(&json!(false)), "false");
    assert_eq!(to_display_string(&json!("s")), "s");
    assert_eq!(to_display_string(&json!([1])), "Array");
    assert_eq!(to_display_string(&json!({"a": 1})), "Object");
}

#[test]
fn display_numbers_like_js() {
    let cases: &[(f64, &str)] = &[
        (3.0, "3"),
        (1.5, "1.5"),
        (-1.5, "-1.5"),
        (0.0, "0"),
        (-0.0, "0"),
        (0.1, "0.1"),
        (0.0299, "0.0299"),
        (0.000001, "0.000001"),
        (0.0000001, "1e-7"),
        (123456789.0, "123456789"),
        (1e20, "100000000000000000000"),
        (1e21, "1e+21"),
        (1.5e300, "1.5e+300"),
        (2.5e-10, "2.5e-10"),
    ];
    for (n, s) in cases {
        assert_eq!(to_display_string(&json!(n)), *s, "{n}");
    }
    assert_eq!(to_display_string(&json!(u64::MAX)), "18446744073709552000");
}

// ---------------------------------------------------------------- functions

#[test]
fn contains_string() {
    assert_eq!(ev("contains('Hello world', 'llo')"), json!(true));
    assert_eq!(ev("contains('Hello world', 'LLO W')"), json!(true));
    assert_eq!(ev("contains('Hello', 'xyz')"), json!(false));
    assert_eq!(ev("contains('abc', '')"), json!(true));
    assert_eq!(ev("contains(123, 2)"), json!(true));
    assert_eq!(ev("contains('true story', true)"), json!(true));
    assert_eq!(ev("contains(null, 'a')"), json!(false));
}

#[test]
fn contains_array() {
    assert_eq!(ev("contains(matrix.node, 18)"), json!(true));
    assert_eq!(ev("contains(matrix.node, '20')"), json!(true));
    assert_eq!(ev("contains(matrix.node, 19)"), json!(false));
    assert_eq!(
        ev("contains(fromJSON('[\"push\", \"pull_request\"]'), github.event_name)"),
        json!(true)
    );
    assert_eq!(ev("contains(fromJSON('[\"A\"]'), 'a')"), json!(true));
    // array search is element equality, not substring
    assert_eq!(ev("contains(fromJSON('[\"abc\"]'), 'b')"), json!(false));
}

#[test]
fn starts_ends_with() {
    assert_eq!(ev("startsWith('Hello world', 'he')"), json!(true));
    assert_eq!(ev("startsWith(github.ref, 'refs/heads/')"), json!(true));
    assert_eq!(ev("startsWith('abc', 'b')"), json!(false));
    assert_eq!(ev("endsWith('Hello world', 'LD')"), json!(true));
    assert_eq!(ev("endsWith('abc', 'b')"), json!(false));
    assert_eq!(ev("startsWith(12345, 12)"), json!(true));
    assert_eq!(ev("endsWith(null, '')"), json!(true));
    assert_eq!(ev("STARTSWITH('a', 'A')"), json!(true));
}

#[test]
fn format_basic() {
    assert_eq!(
        ev("format('Hello {0} {1} {2}', 'Mona', 'the', 'Octocat')"),
        json!("Hello Mona the Octocat")
    );
    assert_eq!(ev("format('{0}{0}', 'ab')"), json!("abab"));
    assert_eq!(ev("format('{1}-{0}', 1, 2.5)"), json!("2.5-1"));
    assert_eq!(
        ev("format('{0}|{1}|{2}', null, true, matrix.node)"),
        json!("|true|Array")
    );
    assert_eq!(ev("format('no placeholders')"), json!("no placeholders"));
}

#[test]
fn format_escapes() {
    assert_eq!(
        ev("format('{{Hello {0} {1} {2}!}}', 'Mona', 'the', 'Octocat')"),
        json!("{Hello Mona the Octocat!}")
    );
    assert_eq!(ev("format('{{0}}', 'x')"), json!("{0}"));
    assert_eq!(ev("format('{{{0}}}', 'x')"), json!("{x}"));
}

#[test]
fn format_errors() {
    assert!(matches!(
        ev_err("format('{0}')"),
        ExprError::FormatArgument {
            index: 0,
            count: 0,
            ..
        }
    ));
    assert!(matches!(
        ev_err("format('{1}', 'a')"),
        ExprError::FormatArgument {
            index: 1,
            count: 1,
            ..
        }
    ));
    assert!(matches!(
        ev_err("format('{', 'a')"),
        ExprError::InvalidFormat { .. }
    ));
    assert!(matches!(
        ev_err("format('}', 'a')"),
        ExprError::InvalidFormat { .. }
    ));
    assert!(matches!(
        ev_err("format('{a}', 'a')"),
        ExprError::InvalidFormat { .. }
    ));
    assert!(matches!(
        ev_err("format('{0', 'a')"),
        ExprError::InvalidFormat { .. }
    ));
    assert!(matches!(
        ev_err("format('{}', 'a')"),
        ExprError::InvalidFormat { .. }
    ));
    let msg = ev_err("format('{3}', 'a')").to_string();
    assert!(msg.contains("{3}") && msg.contains("1 argument"), "{msg}");
}

#[test]
fn join_function() {
    assert_eq!(ev("join(matrix.node)"), json!("18,20"));
    assert_eq!(ev("join(matrix.node, ', ')"), json!("18, 20"));
    assert_eq!(ev("join('abc', '-')"), json!("abc"));
    assert_eq!(ev("join(fromJSON('[]'))"), json!(""));
    assert_eq!(
        ev("join(fromJSON('[null, true, 1.5, \"x\", [1]]'), '|')"),
        json!("|true|1.5|x|Array")
    );
    assert_eq!(ev("join(null)"), json!(""));
}

#[test]
fn to_json_pretty() {
    assert_eq!(ev("toJSON(matrix.node)"), json!("[\n  18,\n  20\n]"));
    assert_eq!(
        ev("toJSON(env)"),
        json!("{\n  \"NAME\": \"World\",\n  \"EMPTY\": \"\",\n  \"ZERO\": \"0\"\n}")
    );
    assert_eq!(ev("toJSON('a\"b')"), json!("\"a\\\"b\""));
    assert_eq!(ev("toJSON(1)"), json!("1"));
    assert_eq!(ev("toJSON(1.5)"), json!("1.5"));
    assert_eq!(ev("toJSON(null)"), json!("null"));
    assert_eq!(ev("toJSON(true)"), json!("true"));
    assert_eq!(
        ev("toJSON(github.event.issues.*.number)"),
        json!("[\n  1,\n  2,\n  3\n]")
    );
}

#[test]
fn from_json() {
    assert_eq!(ev("fromJSON('{\"a\": [1, 2]}')"), json!({"a": [1, 2]}));
    assert_eq!(ev("fromJSON('true')"), json!(true));
    assert_eq!(ev("fromJSON('3')"), json!(3));
    assert_eq!(ev("fromJSON(needs.build-1.outputs.n) > 5"), json!(true));
    assert_eq!(ev("fromJSON(toJSON(matrix)).os"), json!("ubuntu-latest"));
}

#[test]
fn from_json_errors() {
    let e = ev_err("fromJSON('{bad')");
    assert!(matches!(&e, ExprError::InvalidJson { input, .. } if input == "{bad"));
    assert!(e.to_string().contains("{bad"));
    assert!(matches!(
        ev_err("fromJSON('')"),
        ExprError::InvalidJson { .. }
    ));
}

#[test]
fn function_names_case_insensitive() {
    assert_eq!(ev("CONTAINS('abc', 'B')"), json!(true));
    assert_eq!(ev("tojson(1)"), json!("1"));
    assert_eq!(ev("FromJson('[1]')"), json!([1]));
    assert_eq!(ev("Always()"), json!(true));
}

#[test]
fn hash_files_default_errors() {
    let e = ev_err("hashFiles('**/package-lock.json')");
    assert_eq!(e.to_string(), "hashFiles is not available in this context");
}

struct HashCtx;
impl Context for HashCtx {
    fn lookup(&self, _name: &str) -> Option<Value> {
        None
    }
    fn hash_files(&self, patterns: &[String]) -> Result<String, ExprError> {
        Ok(format!("hash:{}", patterns.join(";")))
    }
}

#[test]
fn hash_files_custom() {
    assert_eq!(
        eval_str("hashFiles('a', '**/*.rs', 3)", &HashCtx).unwrap(),
        json!("hash:a;**/*.rs;3")
    );
    // Default trait status is Success.
    assert_eq!(eval_str("success()", &HashCtx).unwrap(), json!(true));
}

#[test]
fn status_functions() {
    let check = |status, expected: [bool; 4]| {
        let c = status_ctx(status);
        let got: Vec<bool> = ["success()", "failure()", "cancelled()", "always()"]
            .iter()
            .map(|s| eval_str(s, &c).unwrap() == json!(true))
            .collect();
        assert_eq!(got, expected, "{status:?}");
    };
    check(JobStatus::Success, [true, false, false, true]);
    check(JobStatus::Failure, [false, true, false, true]);
    check(JobStatus::Cancelled, [false, false, true, true]);
}

// ---------------------------------------------------------------- interpolate

#[test]
fn interpolate_basic() {
    assert_eq!(
        interpolate("Hello ${{ env.NAME }}!", &ctx()).unwrap(),
        "Hello World!"
    );
    assert_eq!(
        interpolate("no expressions", &ctx()).unwrap(),
        "no expressions"
    );
    assert_eq!(interpolate("", &ctx()).unwrap(), "");
}

#[test]
fn interpolate_multiple() {
    assert_eq!(
        interpolate(
            "${{ github.repository }}#${{ github.run_number }} on ${{matrix.os}}",
            &ctx()
        )
        .unwrap(),
        "octo/hello#42 on ubuntu-latest"
    );
    assert_eq!(interpolate("${{ 1 }}${{ 2 }}", &ctx()).unwrap(), "12");
}

#[test]
fn interpolate_coercions() {
    assert_eq!(interpolate("[${{ null }}]", &ctx()).unwrap(), "[]");
    assert_eq!(
        interpolate(
            "${{ true }} ${{ 1.50 }} ${{ matrix.node }} ${{ env }}",
            &ctx()
        )
        .unwrap(),
        "true 1.5 Array Object"
    );
    assert_eq!(
        interpolate("v${{ matrix.version }}", &ctx()).unwrap(),
        "v1.5"
    );
}

#[test]
fn interpolate_braces_inside_strings() {
    assert_eq!(
        interpolate("${{ format('{{0}}') }}", &ctx()).unwrap(),
        "{0}"
    );
    assert_eq!(interpolate("a ${{ '}}' }} b", &ctx()).unwrap(), "a }} b");
    assert_eq!(interpolate("${{ 'it''s }}' }}", &ctx()).unwrap(), "it's }}");
    assert_eq!(
        interpolate("${{ format('{{{0}}}', 'x') }} and ${{ '${{' }}", &ctx()).unwrap(),
        "{x} and ${{"
    );
}

#[test]
fn interpolate_keeps_lone_braces() {
    assert_eq!(
        interpolate("{ } }} {{ $ ${ x", &ctx()).unwrap(),
        "{ } }} {{ $ ${ x"
    );
    assert_eq!(
        interpolate("json: {\"a\": ${{ 1 }}}", &ctx()).unwrap(),
        "json: {\"a\": 1}"
    );
}

#[test]
fn interpolate_errors() {
    assert!(matches!(
        interpolate("x ${{ 1 ", &ctx()),
        Err(ExprError::UnclosedExpression { pos: 2, .. })
    ));
    assert!(matches!(
        interpolate("${{ '}} ", &ctx()),
        Err(ExprError::UnclosedExpression { .. })
    ));
    assert!(matches!(
        interpolate("${{ }}", &ctx()),
        Err(ExprError::EmptyExpression { .. })
    ));
    assert!(matches!(
        interpolate("${{ a b }}", &ctx()),
        Err(ExprError::UnexpectedToken { .. })
    ));
    let msg = interpolate("x ${{ 1 ", &ctx()).unwrap_err().to_string();
    assert!(msg.contains("'}}'"), "{msg}");
}

#[test]
fn interpolate_multiline() {
    let t = "echo ${{ env.NAME }}\necho ${{\n  github.ref\n}}";
    assert_eq!(
        interpolate(t, &ctx()).unwrap(),
        "echo World\necho refs/heads/main"
    );
}

// ---------------------------------------------------------------- evaluate_template / value

#[test]
fn template_raw_values() {
    assert_eq!(
        evaluate_template("${{ matrix.node }}", &ctx()).unwrap(),
        json!([18, 20])
    );
    assert_eq!(
        evaluate_template("  ${{ env }}  ", &ctx()).unwrap(),
        json!({"NAME": "World", "EMPTY": "", "ZERO": "0"})
    );
    assert_eq!(
        evaluate_template("${{ github.run_number }}", &ctx()).unwrap(),
        json!(42)
    );
    assert_eq!(
        evaluate_template("${{ fromJSON('[\"a\",{\"b\":1}]') }}", &ctx()).unwrap(),
        json!(["a", {"b": 1}])
    );
    assert_eq!(
        evaluate_template("${{ null }}", &ctx()).unwrap(),
        Value::Null
    );
    assert_eq!(
        evaluate_template("${{ true }}", &ctx()).unwrap(),
        json!(true)
    );
}

#[test]
fn template_mixed_is_string() {
    assert_eq!(
        evaluate_template("n=${{ github.run_number }}", &ctx()).unwrap(),
        json!("n=42")
    );
    assert_eq!(
        evaluate_template("${{ 1 }}${{ 2 }}", &ctx()).unwrap(),
        json!("12")
    );
    assert_eq!(
        evaluate_template("${{ 1 }} ${{ 2 }}", &ctx()).unwrap(),
        json!("1 2")
    );
    assert_eq!(evaluate_template("plain", &ctx()).unwrap(), json!("plain"));
}

#[test]
fn template_string_with_closing_braces() {
    assert_eq!(
        evaluate_template("${{ '}}' }}", &ctx()).unwrap(),
        json!("}}")
    );
}

#[test]
fn evaluate_value_recursive() {
    let input = json!({
        "${{ env.NAME }}": "${{ env.NAME }}",
        "list": ["${{ matrix.node }}", "x-${{ matrix.os }}", 5, null, true],
        "nested": {"n": "${{ github.run_number }}", "plain": "keep ${ this }"}
    });
    assert_eq!(
        evaluate_value(&input, &ctx()).unwrap(),
        json!({
            "${{ env.NAME }}": "World",
            "list": [[18, 20], "x-ubuntu-latest", 5, null, true],
            "nested": {"n": 42, "plain": "keep ${ this }"}
        })
    );
}

#[test]
fn evaluate_value_propagates_errors() {
    assert!(evaluate_value(&json!({"a": ["${{ nope( }}"]}), &ctx()).is_err());
}

// ---------------------------------------------------------------- evaluate_condition

#[test]
fn condition_plain_and_wrapped() {
    let c = ctx();
    assert!(evaluate_condition("github.ref == 'refs/heads/main'", &c).unwrap());
    assert!(evaluate_condition("${{ github.ref == 'refs/heads/main' }}", &c).unwrap());
    assert!(evaluate_condition("  ${{ github.event_name == 'push' }}  ", &c).unwrap());
    assert!(!evaluate_condition("github.ref == 'refs/heads/dev'", &c).unwrap());
    assert!(!evaluate_condition("${{ false }}", &c).unwrap());
}

#[test]
fn condition_empty_is_success() {
    assert!(evaluate_condition("", &status_ctx(JobStatus::Success)).unwrap());
    assert!(!evaluate_condition("", &status_ctx(JobStatus::Failure)).unwrap());
    assert!(!evaluate_condition("${{ }}", &status_ctx(JobStatus::Cancelled)).unwrap());
    assert!(evaluate_condition("   ", &status_ctx(JobStatus::Success)).unwrap());
}

#[test]
fn condition_implicit_success_wrapping() {
    let failed = status_ctx(JobStatus::Failure);
    assert!(!evaluate_condition("github.ref == 'refs/heads/main'", &failed).unwrap());
    assert!(!evaluate_condition("true", &failed).unwrap());
    assert!(evaluate_condition("always()", &failed).unwrap());
    assert!(evaluate_condition("${{ always() }}", &failed).unwrap());
    assert!(evaluate_condition("failure()", &failed).unwrap());
    assert!(evaluate_condition("failure() && github.ref == 'refs/heads/main'", &failed).unwrap());
    assert!(!evaluate_condition("failure() && github.ref == 'refs/heads/x'", &failed).unwrap());
    assert!(!evaluate_condition("success()", &failed).unwrap());
    assert!(evaluate_condition("!success()", &failed).unwrap());
    assert!(evaluate_condition("!cancelled()", &failed).unwrap());
}

#[test]
fn condition_status_function_case_insensitive_and_nested() {
    let failed = status_ctx(JobStatus::Failure);
    assert!(evaluate_condition("ALWAYS()", &failed).unwrap());
    assert!(evaluate_condition("Failure()", &failed).unwrap());
    // Status call nested inside another expression still disables wrapping.
    assert!(evaluate_condition("contains(format('{0}', failure()), 'true')", &failed).unwrap());
    assert!(evaluate_condition("github.ref == 'refs/heads/main' || always()", &failed).unwrap());
}

#[test]
fn condition_cancelled() {
    let c = status_ctx(JobStatus::Cancelled);
    assert!(!evaluate_condition("true", &c).unwrap());
    assert!(evaluate_condition("cancelled()", &c).unwrap());
    assert!(!evaluate_condition("failure()", &c).unwrap());
    assert!(evaluate_condition("always()", &c).unwrap());
}

#[test]
fn condition_truthiness_of_values() {
    let c = ctx();
    assert!(evaluate_condition("env.NAME", &c).unwrap());
    assert!(!evaluate_condition("env.EMPTY", &c).unwrap());
    assert!(evaluate_condition("env.ZERO", &c).unwrap()); // "0" is a non-empty string
    assert!(!evaluate_condition("env.MISSING", &c).unwrap());
    assert!(evaluate_condition("matrix.node", &c).unwrap());
    assert!(!evaluate_condition("0", &c).unwrap());
}

#[test]
fn condition_with_multiple_expressions_is_string() {
    // GitHub treats this as a non-empty string -> truthy.
    assert!(evaluate_condition("${{ false }} && ${{ false }}", &ctx()).unwrap());
    assert!(
        !evaluate_condition(
            "${{ false }} && ${{ false }}",
            &status_ctx(JobStatus::Failure)
        )
        .unwrap()
    );
}

#[test]
fn condition_errors() {
    assert!(evaluate_condition("github.ref ==", &ctx()).is_err());
    assert!(evaluate_condition("${{ 1 ", &ctx()).is_err());
    assert!(evaluate_condition("nope()", &ctx()).is_err());
}

// ---------------------------------------------------------------- misc

#[test]
fn contains_expression_detection() {
    assert!(contains_expression("a ${{ b }}"));
    assert!(contains_expression("${{"));
    assert!(!contains_expression("${ b }"));
    assert!(!contains_expression("{{ b }}"));
}

#[test]
fn map_context_builder() {
    let c = MapContext::new()
        .with("a", json!(1))
        .with_status(JobStatus::Failure);
    assert_eq!(c.lookup("A"), Some(json!(1)));
    assert_eq!(c.lookup("b"), None);
    assert_eq!(c.status(), JobStatus::Failure);
    assert_eq!(MapContext::default().status, JobStatus::Success);
}

#[test]
fn evaluate_reuses_parsed_ast() {
    let expr = parse("github.run_number > threshold").unwrap();
    let low = ctx().with("threshold", json!(10));
    let high = ctx().with("threshold", json!(100));
    assert_eq!(evaluate(&expr, &low).unwrap(), json!(true));
    assert_eq!(evaluate(&expr, &high).unwrap(), json!(false));
}

#[test]
fn not_returns_bool() {
    assert_eq!(ev("!'x'"), json!(false));
    assert_eq!(ev("!null"), json!(true));
    assert_eq!(ev("!matrix.node"), json!(false));
    assert_eq!(ev("!NaN"), json!(true));
}

#[test]
fn large_and_float_context_numbers() {
    let c = MapContext::new().with(
        "n",
        json!({"big": 9007199254740993u64, "f": 2.0, "neg": -3}),
    );
    assert_eq!(eval_str("n.f", &c).unwrap(), json!(2));
    assert_eq!(eval_str("n.neg < 0", &c).unwrap(), json!(true));
    assert_eq!(interpolate("${{ n.big }}", &c).unwrap(), "9007199254740992");
}

#[test]
fn error_display_messages() {
    let e = parse("a ==").unwrap_err();
    assert_eq!(e.to_string(), "unexpected end of expression: a ==");
    let e = parse("a b").unwrap_err();
    assert_eq!(
        e.to_string(),
        "unexpected token 'b' at position 2 in expression: a b"
    );
    let e = interpolate("${{ x", &ctx()).unwrap_err();
    assert_eq!(
        e.to_string(),
        "unclosed expression starting at position 0 (missing '}}') in: ${{ x"
    );
}
