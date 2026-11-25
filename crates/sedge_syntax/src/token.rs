use crate::source::Span;
use std::fmt;

#[derive(Clone, Debug, PartialEq)]
pub enum TokenKind {
    Ident(String),
    Int(i64),
    Float(f64),
    /// A string literal, already split into literal text and `${...}` holes.
    Str(Vec<StrPart>),

    // Keywords
    Fn,
    Let,
    Var,
    If,
    Else,
    While,
    For,
    In,
    Return,
    Break,
    Continue,
    Match,
    Struct,
    Enum,
    Impl,
    True,
    False,
    SelfKw,
    Native,
    Test,
    Where,

    // Punctuation
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Comma,
    Dot,
    DotDot,
    DotDotEq,
    Colon,
    /// An explicit `;` or one inserted at a line break.
    Semi,
    Arrow,
    FatArrow,
    Question,
    Underscore,

    // Operators
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Amp,
    Pipe,
    Caret,
    Shl,
    Shr,
    Bang,
    AndAnd,
    OrOr,
    Eq,
    EqEq,
    NotEq,
    Lt,
    Le,
    Gt,
    Ge,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    PercentEq,

    Eof,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StrPart {
    Lit(String),
    /// Global span of the expression source inside `${` and `}`.
    Hole(Span),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
    /// True when this `Semi` was inserted at a newline rather than written.
    pub implicit: bool,
}

impl TokenKind {
    pub fn keyword(word: &str) -> Option<TokenKind> {
        Some(match word {
            "fn" => TokenKind::Fn,
            "let" => TokenKind::Let,
            "var" => TokenKind::Var,
            "if" => TokenKind::If,
            "else" => TokenKind::Else,
            "while" => TokenKind::While,
            "for" => TokenKind::For,
            "in" => TokenKind::In,
            "return" => TokenKind::Return,
            "break" => TokenKind::Break,
            "continue" => TokenKind::Continue,
            "match" => TokenKind::Match,
            "struct" => TokenKind::Struct,
            "enum" => TokenKind::Enum,
            "impl" => TokenKind::Impl,
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            "self" => TokenKind::SelfKw,
            "native" => TokenKind::Native,
            "test" => TokenKind::Test,
            "where" => TokenKind::Where,
            "_" => TokenKind::Underscore,
            _ => return None,
        })
    }

    /// Whether a line break after this token ends the statement.
    pub fn ends_statement(&self) -> bool {
        matches!(
            self,
            TokenKind::Ident(_)
                | TokenKind::Int(_)
                | TokenKind::Float(_)
                | TokenKind::Str(_)
                | TokenKind::True
                | TokenKind::False
                | TokenKind::SelfKw
                | TokenKind::Return
                | TokenKind::Break
                | TokenKind::Continue
                | TokenKind::RParen
                | TokenKind::RBracket
                | TokenKind::RBrace
                | TokenKind::Question
                | TokenKind::Underscore
        )
    }
}

impl fmt::Display for TokenKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use TokenKind::*;
        let s = match self {
            Ident(name) => return write!(f, "identifier `{name}`"),
            Int(_) => "integer",
            Float(_) => "float",
            Str(_) => "string",
            Fn => "`fn`",
            Let => "`let`",
            Var => "`var`",
            If => "`if`",
            Else => "`else`",
            While => "`while`",
            For => "`for`",
            In => "`in`",
            Return => "`return`",
            Break => "`break`",
            Continue => "`continue`",
            Match => "`match`",
            Struct => "`struct`",
            Enum => "`enum`",
            Impl => "`impl`",
            True => "`true`",
            False => "`false`",
            SelfKw => "`self`",
            Native => "`native`",
            Test => "`test`",
            Where => "`where`",
            LParen => "`(`",
            RParen => "`)`",
            LBracket => "`[`",
            RBracket => "`]`",
            LBrace => "`{`",
            RBrace => "`}`",
            Comma => "`,`",
            Dot => "`.`",
            DotDot => "`..`",
            DotDotEq => "`..=`",
            Colon => "`:`",
            Semi => "`;`",
            Arrow => "`->`",
            FatArrow => "`=>`",
            Question => "`?`",
            Underscore => "`_`",
            Plus => "`+`",
            Minus => "`-`",
            Star => "`*`",
            Slash => "`/`",
            Percent => "`%`",
            Amp => "`&`",
            Pipe => "`|`",
            Caret => "`^`",
            Shl => "`<<`",
            Shr => "`>>`",
            Bang => "`!`",
            AndAnd => "`&&`",
            OrOr => "`||`",
            Eq => "`=`",
            EqEq => "`==`",
            NotEq => "`!=`",
            Lt => "`<`",
            Le => "`<=`",
            Gt => "`>`",
            Ge => "`>=`",
            PlusEq => "`+=`",
            MinusEq => "`-=`",
            StarEq => "`*=`",
            SlashEq => "`/=`",
            PercentEq => "`%=`",
            Eof => "end of file",
        };
        f.write_str(s)
    }
}
