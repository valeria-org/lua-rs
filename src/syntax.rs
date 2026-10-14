//! Lua 5.4 and Luau syntax analysis, without creating a VM or generating bytecode.
//!
//! AST nodes, tokens, trivia, and visitors are provided by
//! [Full Moon](https://github.com/Kampfkarren/full-moon). Trees own their data, preserve the
//! original source spelling, and can outlive the source string. Parsing checks syntax;
//! it does not type-check Luau or validate runtime semantics.
//!
//! ```
//! use luajit::syntax::{Dialect, Spanned, parse_ast};
//!
//! let source = "local answer <const> = 42";
//! let ast = parse_ast(source, Dialect::Lua54)?;
//! let statement = ast.nodes().stmts().next().unwrap();
//! let span = statement.byte_span().unwrap();
//! assert_eq!(&source[span.range()], source);
//! assert_eq!(ast.to_string(), source);
//! # Ok::<(), luajit::syntax::ParseError>(())
//! ```

use std::fmt;
use std::ops::Range;

pub use full_moon::{ast, node::Node, tokenizer, visitors};

/// The grammar used for syntax analysis. Each dialect is selected independently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    /// Standard Lua 5.4, including local attributes and the Lua 5.3 operators.
    Lua54,
    /// Luau, including type syntax, compound assignments, and interpolated strings.
    Luau,
}

impl fmt::Display for Dialect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Lua54 => "Lua 5.4",
            Self::Luau => "Luau",
        })
    }
}

/// A zero-based, half-open byte range (`start..end`) in the original UTF-8 source.
///
/// The end is exclusive. Node spans exclude surrounding comments and whitespace;
/// use token trivia to inspect those separately. An EOF diagnostic can have an empty span.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ByteSpan {
    /// Offset of the first byte.
    pub start: usize,
    /// Offset immediately after the last byte.
    pub end: usize,
}

impl ByteSpan {
    /// Return a range suitable for indexing the original source bytes or string.
    pub fn range(self) -> Range<usize> {
        self.start..self.end
    }

    /// The span of a token, including a comment or whitespace token.
    pub fn of_token(token: &tokenizer::Token) -> Self {
        Self {
            start: token.start_position().bytes(),
            end: token.end_position().bytes(),
        }
    }
}

/// Byte spans for AST nodes and token references.
pub trait Spanned {
    /// Return the node's span in the input source, or `None` for a tokenless node
    /// such as an empty block. Positions on manually constructed or edited ASTs
    /// need to be updated before they can describe the rewritten source.
    fn byte_span(&self) -> Option<ByteSpan>;
}

impl<T: Node + ?Sized> Spanned for T {
    fn byte_span(&self) -> Option<ByteSpan> {
        let (start, end) = self.range()?;
        Some(ByteSpan {
            start: start.bytes(),
            end: end.bytes(),
        })
    }
}

/// A lexical or syntactic diagnostic, independent of VM-managed values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyntaxDiagnostic {
    /// Location in the original source, measured in bytes.
    pub span: ByteSpan,
    /// Human-readable explanation of the syntax error.
    pub message: String,
}

/// All diagnostics from an unsuccessful parse. No partial AST is returned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    /// The requested grammar.
    pub dialect: Dialect,
    /// Diagnostics in the order reported by the parser.
    pub diagnostics: Vec<SyntaxDiagnostic>,
}

impl ParseError {
    fn from_errors(dialect: Dialect, errors: Vec<full_moon::Error>) -> Self {
        Self {
            dialect,
            diagnostics: errors
                .into_iter()
                .map(|error| {
                    let (start, end) = error.range();
                    SyntaxDiagnostic {
                        span: ByteSpan {
                            start: start.bytes(),
                            end: end.bytes(),
                        },
                        message: error.error_message().into_owned(),
                    }
                })
                .collect(),
        }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} syntax error", self.dialect)?;
        if let Some(diagnostic) = self.diagnostics.first() {
            write!(
                f,
                " at bytes {}..{}: {}",
                diagnostic.span.start, diagnostic.span.end, diagnostic.message
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for ParseError {}

/// Parse UTF-8 source into an owned, lossless AST using exactly the selected dialect.
///
/// The AST exposes statements, expressions, types, tokens, and trivia through [`ast`].
/// Use [`Spanned`] for node ranges and [`ByteSpan::of_token`] for trivia ranges.
/// Accepted input prints back exactly with `ast.to_string()`.
///
/// This API does not compile or execute the source. The existing
/// [`crate::parse::parse`] and [`crate::state::Vm::load`] APIs use the LuaJIT grammar.
pub fn parse_ast(source: &str, dialect: Dialect) -> Result<ast::Ast, ParseError> {
    if dialect == Dialect::Lua54 && source.contains('`') {
        reject_luau_strings(source)?;
    }
    let version = match dialect {
        Dialect::Lua54 => full_moon::LuaVersion::lua54(),
        Dialect::Luau => full_moon::LuaVersion::luau(),
    };
    full_moon::parse_fallible(source, version)
        .into_result()
        .map_err(|errors| ParseError::from_errors(dialect, errors))
}

fn reject_luau_strings(source: &str) -> Result<(), ParseError> {
    // Full Moon 3.0 panics when its Lua 5.4 lexer encounters a backtick. Lex with
    // both dialects to identify actual interpolated strings, without rejecting
    // backticks inside ordinary quoted strings, long strings, or comments.
    let tokens = match tokenizer::Lexer::new(source, full_moon::LuaVersion::new()).collect() {
        tokenizer::LexerResult::Ok(tokens) => tokens,
        tokenizer::LexerResult::Recovered(_, errors) | tokenizer::LexerResult::Fatal(errors) => {
            return Err(ParseError::from_errors(
                Dialect::Lua54,
                errors
                    .into_iter()
                    .map(full_moon::Error::TokenizerError)
                    .collect(),
            ));
        }
    };
    if let Some(token) = tokens.iter().find(|token| {
        matches!(
            token.token_type(),
            tokenizer::TokenType::InterpolatedString { .. }
        )
    }) {
        return Err(ParseError {
            dialect: Dialect::Lua54,
            diagnostics: vec![SyntaxDiagnostic {
                span: ByteSpan::of_token(token),
                message: "interpolated strings require the Luau dialect".into(),
            }],
        });
    }
    Ok(())
}
