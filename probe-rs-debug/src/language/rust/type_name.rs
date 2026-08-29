//! Parse a compiler type name when DWARF has no template-parameter DIEs.

use crate::{GenericArg, NamedType, VariableType};

pub(crate) fn parse_variable_type(input: &str) -> Option<VariableType> {
    let mut parser = Parser::new(input.trim());
    let ty = parser.parse_type()?;
    parser.skip_ws();
    parser.done().then_some(ty)
}

#[cfg(test)]
pub(crate) fn parse_named_type(input: &str) -> Option<NamedType> {
    match parse_variable_type(input)? {
        VariableType::Struct(named) | VariableType::Enum(named) => Some(named),
        _ => None,
    }
}

/// `true` if `ident` is a path segment, not a pointer, reference, or `fn` prefix.
pub(crate) fn is_path_ident(ident: &str) -> bool {
    ident
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '{')
}

struct Parser<'a> {
    s: &'a str,
    i: usize,
}

impl<'a> Parser<'a> {
    fn new(s: &'a str) -> Self {
        Self { s, i: 0 }
    }

    fn done(&self) -> bool {
        self.i >= self.s.len()
    }

    fn rest(&self) -> &'a str {
        &self.s[self.i..]
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn advance(&mut self) {
        if let Some(c) = self.peek() {
            self.i += c.len_utf8();
        }
    }

    fn skip_ws(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.advance();
        }
    }

    fn eat(&mut self, prefix: impl AsPrefix) -> bool {
        self.skip_ws();
        prefix.eat(self)
    }

    fn parse_type(&mut self) -> Option<VariableType> {
        self.skip_ws();
        if self.eat("&mut ") || self.eat("&mut") {
            return Some(prefix_type("&mut", self.parse_ref_inner()?));
        }
        if self.eat('&') {
            return Some(prefix_type("&", self.parse_ref_inner()?));
        }
        if self.eat("*const ") {
            return Some(prefix_type("*const", self.parse_type()?));
        }
        if self.eat("*mut ") {
            return Some(prefix_type("*mut", self.parse_type()?));
        }
        if self.eat("*raw ") {
            return Some(prefix_type("*raw", self.parse_type()?));
        }
        if self.peek() == Some('[') {
            return self.parse_array();
        }
        if self.peek() == Some('(') {
            return self.parse_paren();
        }
        if self.eat("dyn ") {
            return self.parse_dyn();
        }
        if let Some(ty) = self.parse_fn() {
            return Some(ty);
        }
        self.parse_path()
    }

    fn parse_ref_inner(&mut self) -> Option<VariableType> {
        self.skip_ws();
        if self.peek() == Some('(') {
            self.parse_paren()
        } else {
            self.parse_type()
        }
    }

    fn parse_paren(&mut self) -> Option<VariableType> {
        if !self.eat('(') {
            return None;
        }
        self.skip_ws();
        if self.eat(')') {
            return Some(VariableType::Base("()".to_string()));
        }
        let mut args = Vec::new();
        loop {
            args.push(GenericArg::Type(self.parse_type()?));
            self.skip_ws();
            if self.eat(')') {
                break;
            }
            if !self.eat(',') {
                return None;
            }
            self.skip_ws();
            if self.eat(')') {
                break;
            }
        }
        if args.len() == 1 {
            match args.pop() {
                Some(GenericArg::Type(ty)) => Some(ty),
                _ => None,
            }
        } else {
            Some(VariableType::Struct(NamedType {
                ident: String::new(),
                namespace: Vec::new(),
                args,
            }))
        }
    }

    fn parse_array(&mut self) -> Option<VariableType> {
        if !self.eat('[') {
            return None;
        }
        let item = self.parse_type()?;
        self.skip_ws();
        if self.eat(';') {
            self.skip_ws();
            let count = self.parse_usize()?;
            self.skip_ws();
            if !self.eat(']') {
                return None;
            }
            Some(VariableType::Array {
                item_type_name: Box::new(item),
                count,
            })
        } else {
            if !self.eat(']') {
                return None;
            }
            Some(prefix_type("[]", item))
        }
    }

    fn parse_dyn(&mut self) -> Option<VariableType> {
        let mut args = Vec::new();
        loop {
            args.push(GenericArg::Type(self.parse_path()?));
            self.skip_ws();
            if !self.eat('+') {
                break;
            }
        }
        Some(VariableType::Struct(NamedType {
            ident: "dyn".to_string(),
            namespace: Vec::new(),
            args,
        }))
    }

    fn parse_fn(&mut self) -> Option<VariableType> {
        let start = self.i;
        let mut ident = String::new();
        if self.eat("unsafe ") {
            ident.push_str("unsafe ");
        }
        if self.eat("extern \"C\" ") {
            ident.push_str("extern \"C\" ");
        }
        if !self.eat("fn") {
            self.i = start;
            return None;
        }
        ident.push_str("fn");
        self.skip_ws();
        if !self.eat('(') {
            self.i = start;
            return None;
        }
        let mut args = Vec::new();
        self.skip_ws();
        if !self.eat(')') {
            loop {
                args.push(GenericArg::Type(self.parse_type()?));
                self.skip_ws();
                if self.eat(')') {
                    break;
                }
                if !self.eat(',') {
                    self.i = start;
                    return None;
                }
            }
        }
        self.skip_ws();
        if self.eat("->") {
            ident.push_str(" ->");
            args.push(GenericArg::Type(self.parse_type()?));
        }
        Some(VariableType::Struct(NamedType {
            ident,
            namespace: Vec::new(),
            args,
        }))
    }

    fn parse_path(&mut self) -> Option<VariableType> {
        self.skip_ws();
        if self.peek() == Some('!') {
            self.advance();
            return Some(VariableType::Base("!".to_string()));
        }
        if self.peek() == Some('\'') {
            return Some(VariableType::Other(self.parse_lifetime()?));
        }

        let mut segments = Vec::new();
        loop {
            let segment = self.parse_segment()?;
            segments.push(segment);
            if !self.eat("::") {
                break;
            }
        }
        if segments.is_empty() {
            return None;
        }

        let mut args = Vec::new();
        if self.eat('<') {
            self.skip_ws();
            if !self.eat('>') {
                loop {
                    args.push(self.parse_generic_arg()?);
                    self.skip_ws();
                    if self.eat('>') {
                        break;
                    }
                    if !self.eat(',') {
                        return None;
                    }
                    self.skip_ws();
                    if self.eat('>') {
                        break;
                    }
                }
            }
        }

        let ident = segments.pop()?;
        if args.is_empty() && segments.is_empty() && is_primitive(&ident) {
            return Some(VariableType::Base(ident));
        }

        Some(VariableType::Struct(NamedType {
            ident,
            namespace: segments,
            args,
        }))
    }

    fn parse_generic_arg(&mut self) -> Option<GenericArg> {
        self.skip_ws();
        if self.peek() == Some('\'') {
            return Some(GenericArg::Const(self.parse_lifetime()?));
        }
        if self.peek().is_some_and(|c| c.is_ascii_digit() || c == '-') {
            return Some(GenericArg::Const(self.parse_const_token()?));
        }
        Some(GenericArg::Type(self.parse_type()?))
    }

    fn parse_segment(&mut self) -> Option<String> {
        self.skip_ws();
        let start = self.i;
        if self.peek() == Some('{') {
            self.skip_balanced('{', '}')?;
            return Some(self.s[start..self.i].to_string());
        }
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == '_' || c == '#' || c == '$' {
                self.advance();
            } else {
                break;
            }
        }
        (self.i > start).then(|| self.s[start..self.i].to_string())
    }

    fn parse_lifetime(&mut self) -> Option<String> {
        let start = self.i;
        if !self.eat('\'') {
            return None;
        }
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            self.advance();
        }
        (self.i > start + 1).then(|| self.s[start..self.i].to_string())
    }

    fn parse_const_token(&mut self) -> Option<String> {
        let start = self.i;
        if self.eat('-') && !self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.i = start;
            return None;
        }
        if self.rest().starts_with("0x") || self.rest().starts_with("0X") {
            self.i += 2;
            while self.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
                self.advance();
            }
        } else {
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.advance();
            }
        }
        (self.i > start).then(|| self.s[start..self.i].to_string())
    }

    fn parse_usize(&mut self) -> Option<usize> {
        let token = self.parse_const_token()?;
        token.parse().ok()
    }

    fn skip_balanced(&mut self, open: char, close: char) -> Option<()> {
        if !self.eat(open) {
            return None;
        }
        let mut depth = 1;
        while let Some(c) = self.peek() {
            self.advance();
            if c == open {
                depth += 1;
            } else if c == close {
                depth -= 1;
                if depth == 0 {
                    return Some(());
                }
            }
        }
        None
    }
}

trait AsPrefix {
    fn eat(self, parser: &mut Parser<'_>) -> bool;
}

impl AsPrefix for &str {
    fn eat(self, parser: &mut Parser<'_>) -> bool {
        if parser.rest().starts_with(self) {
            parser.i += self.len();
            true
        } else {
            false
        }
    }
}

impl AsPrefix for char {
    fn eat(self, parser: &mut Parser<'_>) -> bool {
        if parser.peek() == Some(self) {
            parser.advance();
            true
        } else {
            false
        }
    }
}

fn prefix_type(ident: &str, inner: VariableType) -> VariableType {
    VariableType::Struct(NamedType {
        ident: ident.to_string(),
        namespace: Vec::new(),
        args: vec![GenericArg::Type(inner)],
    })
}

fn is_primitive(name: &str) -> bool {
    matches!(
        name,
        "bool"
            | "char"
            | "str"
            | "f32"
            | "f64"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "!"
            | "()"
    )
}

pub(crate) fn format_named_head(ident: &str, args: &[String]) -> Option<String> {
    match ident {
        "" => Some(format!("({})", args.join(", "))),
        "&" => Some(format_ref("&", args.first()?)),
        "&mut" => Some(format!("&mut {}", args.first()?)),
        "*const" | "*mut" | "*raw" => Some(format!("{ident} {}", args.first()?)),
        "[]" => Some(format!("[{}]", args.first()?)),
        "dyn" => Some(format!("dyn {}", args.join(" + "))),
        ident if ident == "fn" || ident.ends_with(" fn") || ident.ends_with(" ->") => {
            Some(format_fn(ident, args))
        }
        _ => None,
    }
}

fn format_ref(prefix: &str, inner: &str) -> String {
    if inner.starts_with("dyn ") {
        format!("{prefix}({inner})")
    } else {
        format!("{prefix}{inner}")
    }
}

fn format_fn(ident: &str, args: &[String]) -> String {
    if let Some(ident) = ident.strip_suffix(" ->") {
        match args.split_last() {
            Some((ret, params)) => format!("{ident}({}) -> {ret}", params.join(", ")),
            None => format!("{ident}()"),
        }
    } else {
        format!("{ident}({})", args.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TypeNameStyle;

    fn rust() -> crate::language::rust::Rust {
        crate::language::rust::Rust
    }

    fn compact(ty: &VariableType) -> String {
        ty.display_name_with_style(&rust(), TypeNameStyle::Compact)
    }

    fn qualified(ty: &VariableType) -> String {
        ty.display_name_with_style(&rust(), TypeNameStyle::Qualified)
    }

    #[test]
    fn option_with_a_qualified_argument_splits_into_ident_and_args() {
        let named = parse_named_type("Option<esp_hal::soc::implementation::clocks::XtalClkConfig>")
            .unwrap();
        assert_eq!(named.ident, "Option");
        assert_eq!(named.namespace, [] as [String; 0]);
        assert_eq!(
            named.display(&rust(), TypeNameStyle::Compact),
            "Option<XtalClkConfig>"
        );
        assert_eq!(
            named.display(&rust(), TypeNameStyle::Qualified),
            "Option<esp_hal::soc::implementation::clocks::XtalClkConfig>"
        );
    }

    #[test]
    fn nested_generic_arguments_are_shortened_recursively() {
        let ty = parse_variable_type(
            "Option<core::ptr::non_null::NonNull<embassy_executor::raw::TaskHeader>>",
        )
        .unwrap();
        assert_eq!(compact(&ty), "Option<NonNull<TaskHeader>>");
        assert_eq!(
            qualified(&ty),
            "Option<core::ptr::non_null::NonNull<embassy_executor::raw::TaskHeader>>"
        );
    }

    #[test]
    fn arrays_and_maybe_uninit_inside_generic_arguments_are_parsed() {
        let ty =
            parse_variable_type("VecStorageInner<[core::mem::maybe_uninit::MaybeUninit<u32>; 10]>")
                .unwrap();
        assert_eq!(compact(&ty), "VecStorageInner<[MaybeUninit<u32>; 10]>");
        assert_eq!(
            qualified(&ty),
            "VecStorageInner<[core::mem::maybe_uninit::MaybeUninit<u32>; 10]>"
        );
    }

    #[test]
    fn a_mut_slice_pointer_keeps_the_pointer_syntax() {
        let named = parse_named_type("*mut [core::mem::maybe_uninit::MaybeUninit<u32>]").unwrap();
        assert_eq!(named.ident, "*mut");
        assert_eq!(
            named.display(&rust(), TypeNameStyle::Compact),
            "*mut [MaybeUninit<u32>]"
        );
        assert_eq!(
            named.display(&rust(), TypeNameStyle::Qualified),
            "*mut [core::mem::maybe_uninit::MaybeUninit<u32>]"
        );
    }

    #[test]
    fn an_unsafe_fn_argument_shortens_its_parameter_type() {
        let ty = parse_variable_type("Option<unsafe fn(embassy_executor::raw::TaskRef)>").unwrap();
        assert_eq!(compact(&ty), "Option<unsafe fn(TaskRef)>");
        assert_eq!(
            qualified(&ty),
            "Option<unsafe fn(embassy_executor::raw::TaskRef)>"
        );
    }

    #[test]
    fn a_mutable_reference_shortens_the_pointee_path() {
        let ty = parse_variable_type("&mut core::task::wake::Context").unwrap();
        assert_eq!(compact(&ty), "&mut Context");
        assert_eq!(qualified(&ty), "&mut core::task::wake::Context");
    }
}
