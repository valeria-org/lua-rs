use luajit::syntax::{ByteSpan, Dialect, Node, Spanned, ast, parse_ast, visitors::Visitor};

fn check_source(source: &str, dialect: Dialect) -> ast::Ast {
    let ast = parse_ast(source, dialect).unwrap_or_else(|e| panic!("{e}\n{source}"));
    assert_eq!(ast.to_string(), source, "lossless {dialect} parsing");
    for token in ast.tokens().chain(std::iter::once(ast.eof())) {
        let span = token.byte_span().unwrap();
        assert_eq!(&source[span.range()], token.token().to_string());
        for trivia in token.leading_trivia().chain(token.trailing_trivia()) {
            assert_eq!(
                &source[ByteSpan::of_token(trivia).range()],
                trivia.to_string()
            );
        }
    }
    ast
}

#[test]
fn lua54_grammar_and_typed_nodes() {
    let source = include_str!("fixtures/syntax/lua54.lua");
    let ast = check_source(source, Dialect::Lua54);
    let mut statements = ast.nodes().stmts();
    let ast::Stmt::LocalAssignment(local) = statements.next().unwrap() else {
        panic!("expected a local assignment");
    };
    assert_eq!(local.attributes().flatten().count(), 2);
    assert!(
        ast.nodes()
            .stmts()
            .any(|s| matches!(s, ast::Stmt::Label(_)))
    );
    assert!(ast.nodes().stmts().any(|s| matches!(s, ast::Stmt::Goto(_))));
}

#[test]
fn luau_grammar_and_typed_nodes() {
    let source = include_str!("fixtures/syntax/luau.luau");
    let ast = check_source(source, Dialect::Luau);
    let statements: Vec<_> = ast.nodes().stmts().collect();
    assert!(matches!(
        statements[0],
        ast::Stmt::ExportedTypeDeclaration(_)
    ));
    assert!(
        statements
            .iter()
            .any(|s| matches!(s, ast::Stmt::TypeFunction(_)))
    );
    assert!(
        statements
            .iter()
            .any(|s| matches!(s, ast::Stmt::ConstAssignment(_)))
    );
    assert!(
        statements
            .iter()
            .any(|s| matches!(s, ast::Stmt::CompoundAssignment(_)))
    );
}

#[test]
fn dialects_do_not_accept_each_others_extensions() {
    for source in [
        "local value: number = 1",
        "type Pair<T> = {first: T, second: T}",
        "local value = if ready then 1 else 0",
        "value += 1",
        "local text = `hello {name}`",
        "while true do continue end",
        "const value = 1",
    ] {
        assert!(parse_ast(source, Dialect::Lua54).is_err(), "{source}");
        check_source(source, Dialect::Luau);
    }
    for source in [
        "local value <const> = 1",
        "local value <close> = acquire()",
        "local value = (1 << 3) & ~2 | (5 ~ 1) >> 1",
        "::label:: goto label",
    ] {
        assert!(parse_ast(source, Dialect::Luau).is_err(), "{source}");
        check_source(source, Dialect::Lua54);
    }
}

#[test]
fn byte_offsets_include_unicode_and_crlf() {
    let source = "-- café 🦀\r\nlocal greeting = 'héllo 🌍' -- τέλος\r\nreturn greeting\r\n";
    for dialect in [Dialect::Lua54, Dialect::Luau] {
        let ast = check_source(source, dialect);
        let local = ast.nodes().stmts().next().unwrap();
        let start = source.find("local").unwrap();
        let end = source.find(" -- τέλος").unwrap();
        assert_eq!(local.byte_span(), Some(ByteSpan { start, end }));
        let ast::Stmt::LocalAssignment(local) = local else {
            unreachable!()
        };
        let expression = local.expressions().iter().next().unwrap();
        assert_eq!(
            &source[expression.byte_span().unwrap().range()],
            "'héllo 🌍'"
        );
    }
}

#[test]
fn visitor_exposes_nested_expression_spans() {
    struct Expressions<'a> {
        source: &'a str,
        spans: Vec<ByteSpan>,
    }
    impl Visitor for Expressions<'_> {
        fn visit_expression(&mut self, expression: &ast::Expression) {
            let span = expression.byte_span().unwrap();
            assert!(self.source.get(span.range()).is_some());
            self.spans.push(span);
        }
    }
    let source = "local result = if ok then `value {1 + 2}` else `empty`";
    let ast = check_source(source, Dialect::Luau);
    let mut visitor = Expressions {
        source,
        spans: Vec::new(),
    };
    visitor.visit_ast(&ast);
    assert!(
        visitor
            .spans
            .iter()
            .any(|span| &source[span.range()] == "1 + 2")
    );
    assert!(
        visitor
            .spans
            .iter()
            .any(|span| &source[span.range()] == "`value {1 + 2}`")
    );
}

#[test]
fn empty_chunks_and_shebangs_are_lossless() {
    for dialect in [Dialect::Lua54, Dialect::Luau] {
        for source in [
            "",
            "-- only a comment\r\n",
            "#!/usr/bin/env lua\nlocal x = 1\n",
        ] {
            let ast = check_source(source, dialect);
            if !source.contains("local") {
                assert_eq!(ast.nodes().byte_span(), None);
            }
            assert_eq!(
                ast.eof().byte_span(),
                Some(ByteSpan {
                    start: source.len(),
                    end: source.len()
                })
            );
        }
    }
}

#[test]
fn lexical_and_syntax_diagnostics_have_byte_spans() {
    for dialect in [Dialect::Lua54, Dialect::Luau] {
        for source in [
            "-- café 🦀\nlocal x = )",
            "local x = 'unterminated",
            "local x =",
            "local x = @",
            "local x = 🦀",
        ] {
            let error = parse_ast(source, dialect).unwrap_err();
            assert_eq!(error.dialect, dialect);
            assert!(!error.diagnostics.is_empty());
            assert!(error.to_string().contains(&dialect.to_string()));
            for diagnostic in &error.diagnostics {
                assert!(diagnostic.span.start <= diagnostic.span.end);
                assert!(diagnostic.span.end <= source.len());
                assert!(source.get(diagnostic.span.range()).is_some());
                assert!(!diagnostic.message.is_empty());
            }
            if source.ends_with(')') {
                assert!(
                    error
                        .diagnostics
                        .iter()
                        .any(|d| d.span.start == source.find(')').unwrap())
                );
            }
            if source == "local x =" {
                assert!(error.diagnostics.iter().any(|d| d.span.end == source.len()));
            }
        }
    }
}

#[test]
fn lua54_backticks_are_rejected_only_as_syntax() {
    for source in [
        "local text = 'a `backtick`' -- `comment`\n",
        "local text = [=[a `backtick`]=] --[=[`comment`]=]\n",
        "-- `comment`\nlocal text = \"`backtick`\"",
    ] {
        check_source(source, Dialect::Lua54);
    }
    for source in [
        "local text = `hello`",
        "local text = `hello {name}`",
        "local text = `unclosed",
        "local text = `hello {`",
        "local x = ; local text = `hello`",
    ] {
        assert!(parse_ast(source, Dialect::Lua54).is_err(), "{source}");
    }
}

#[test]
fn trees_own_their_data_and_parse_alias_is_public() {
    let ast = {
        let source = String::from("local x <const> = 42");
        luajit::parse::parse_ast(&source, luajit::parse::Dialect::Lua54).unwrap()
    };
    assert_eq!(ast.to_string(), "local x <const> = 42");
}
