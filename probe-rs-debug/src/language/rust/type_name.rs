//! Parse a compiler type name when DWARF has no template-parameter DIEs.

use crate::{GenericArg, NamedType, TypeNameStyle, VariableType};

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

/// Compact a Rust debug symbol. Keep `Type::method`. Drop crate and module
/// prefixes on types. Compact generic arguments as types.
pub(crate) fn compact_debug_name(name: &str) -> String {
    compact_name(name, NameKind::Symbol)
}

fn compact_type_arg(name: &str) -> String {
    compact_name(name, NameKind::Type)
}

#[derive(Clone, Copy)]
enum NameKind {
    Symbol,
    Type,
}

fn compact_name(name: &str, kind: NameKind) -> String {
    let name = name.trim();
    if name.is_empty() {
        return String::new();
    }
    if let Some(compacted) = compact_as_clause(name) {
        return compacted;
    }
    if let Some(ty) = parse_variable_type(name) {
        return match kind {
            NameKind::Type => {
                ty.display_name_with_style(&crate::language::rust::Rust, TypeNameStyle::Compact)
            }
            NameKind::Symbol => compact_parsed_symbol(&ty),
        };
    }
    compact_method_path(name)
}

fn compact_parsed_symbol(ty: &VariableType) -> String {
    let rust = crate::language::rust::Rust;
    match ty {
        VariableType::Struct(named) | VariableType::Enum(named) => {
            let compact = named.display(&rust, TypeNameStyle::Compact);
            if named.args.is_empty() {
                if looks_like_method(&named.ident)
                    && let Some(parent) = named.namespace.last()
                {
                    format!("{parent}::{compact}")
                } else {
                    compact
                }
            } else if named.namespace.len() == 1 {
                format!("{}::{compact}", named.namespace[0])
            } else {
                compact
            }
        }
        _ => ty.display_name_with_style(&rust, TypeNameStyle::Compact),
    }
}

fn compact_method_path(name: &str) -> String {
    let name = compact_generics(name);
    let segments = split_path_segments(&name);
    if segments.is_empty() {
        return name;
    }
    let start = segments
        .iter()
        .rposition(|segment| segment.contains('<'))
        .unwrap_or_else(|| segments.len().saturating_sub(2));
    segments[start..].join("::")
}

fn split_path_segments(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut depth = 0i32;
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c == '-' && chars.peek().is_some_and(|(_, n)| *n == '>') {
            chars.next();
            continue;
        }
        match c {
            '<' | '(' | '[' | '{' => depth += 1,
            '>' | ')' | ']' | '}' => depth -= 1,
            ':' if depth == 0 && chars.peek().is_some_and(|(_, n)| *n == ':') => {
                parts.push(&s[start..i]);
                chars.next();
                start = i + 2;
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
}

fn looks_like_method(ident: &str) -> bool {
    ident.chars().next().is_some_and(|c| c.is_ascii_lowercase())
}

fn compact_as_clause(name: &str) -> Option<String> {
    if !name.starts_with('<') {
        return None;
    }
    let close = matching_bracket_end(name, 0)?;
    let inner = &name[1..close];
    let as_at = find_as_separator(inner)?;
    let type_name = inner[..as_at].trim();
    let trait_name = inner[as_at + 4..].trim();
    let suffix = name[close + 1..].trim_start();
    Some(format!(
        "<{} as {}>{}",
        compact_type_arg(type_name),
        compact_type_arg(trait_name),
        compact_generics(suffix)
    ))
}

fn find_as_separator(inner: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut chars = inner.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match c {
            '<' | '(' | '[' | '{' => depth += 1,
            '>' | ')' | ']' | '}' => depth -= 1,
            '-' if chars.peek().is_some_and(|(_, n)| *n == '>') => {
                chars.next();
            }
            ' ' if depth == 0 && inner[i..].starts_with(" as ") => return Some(i),
            _ => {}
        }
    }
    None
}

fn compact_generics(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut rest = name;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        let Some(close) = matching_bracket_end(rest, open) else {
            out.push_str(&rest[open..]);
            return out;
        };
        let inner = &rest[open + 1..close];
        out.push('<');
        let args: Vec<String> = split_top_level(inner, ',')
            .into_iter()
            .map(|arg| compact_type_arg(arg.trim()))
            .collect();
        out.push_str(&args.join(", "));
        out.push('>');
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out
}

fn matching_bracket_end(s: &str, open_idx: usize) -> Option<usize> {
    let open = s[open_idx..].chars().next()?;
    let first_close = closer(open)?;
    let mut stack = vec![first_close];
    let mut chars = s[open_idx..].char_indices().peekable();
    chars.next();
    while let Some((i, c)) = chars.next() {
        if c == '-' && chars.peek().is_some_and(|(_, n)| *n == '>') {
            chars.next();
            continue;
        }
        if let Some(close) = closer(c) {
            stack.push(close);
        } else if stack.last() == Some(&c) {
            stack.pop();
            if stack.is_empty() {
                return Some(open_idx + i);
            }
        }
    }
    None
}

fn closer(c: char) -> Option<char> {
    match c {
        '<' => Some('>'),
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        _ => None,
    }
}

fn split_top_level(s: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut depth = 0i32;
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c == '-' && chars.peek().is_some_and(|(_, n)| *n == '>') {
            chars.next();
            continue;
        }
        match c {
            '<' | '(' | '[' | '{' => depth += 1,
            '>' | ')' | ']' | '}' => depth -= 1,
            c if c == sep && depth == 0 => {
                parts.push(&s[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
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

    #[test]
    fn compact_debug_name_shortens_types_and_methods() {
        assert_eq!(compact_debug_name("alloc::string::String"), "String");
        assert_eq!(
            compact_debug_name(
                "embassy_executor::raw::TaskStorage<coredump_c6::____embassy_main_task::{async_fn_env#0}>::poll"
            ),
            "TaskStorage<{async_fn_env#0}>::poll"
        );
        assert_eq!(
            compact_debug_name(
                "RunQueue::dequeue_all<embassy_executor::raw::{impl#9}::poll::{closure_env#0}>"
            ),
            "RunQueue::dequeue_all<{closure_env#0}>"
        );
        assert_eq!(
            compact_debug_name("Executor::run<coredump_c6::__xtensa_lx_rt_main::{closure_env#0}>"),
            "Executor::run<{closure_env#0}>"
        );
        assert_eq!(
            compact_debug_name("core::ptr::drop_in_place<alloc::string::String>"),
            "drop_in_place<String>"
        );
        assert_eq!(
            compact_debug_name(
                "<coredump_c6::____embassy_main_task::{async_fn#0} as core::future::future::Future>::poll"
            ),
            "<{async_fn#0} as Future>::poll"
        );
        assert_eq!(
            compact_debug_name(
                "Option<unsafe fn(embassy_executor::raw::TaskRef)>::unwrap_unchecked"
            ),
            "Option<unsafe fn(TaskRef)>::unwrap_unchecked"
        );
        assert_eq!(
            compact_debug_name("with<!, panic_rtt_target::panic::{closure_env#0}>"),
            "with<!, {closure_env#0}>"
        );
        assert_eq!(
            compact_debug_name("embassy_executor::raw::SyncExecutor::poll"),
            "SyncExecutor::poll"
        );
    }
}
