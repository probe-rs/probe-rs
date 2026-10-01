//! A C `printf` implementation for rendering Zephyr log messages on the host.
//!
//! The arguments are pulled from an [`ArgSource`] as the format string is walked, the same way
//! `cbprintf` packaged them on the target.

/// The C type of a `printf` argument, as determined by the conversion specifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgType {
    Int,
    UInt,
    Long,
    ULong,
    LongLong,
    ULongLong,
    Ptr,
    Double,
    LongDouble,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Arg {
    Signed(i64),
    Unsigned(u64),
    Float(f64),
    Str(String),
}

/// Provides the arguments of a format string, in order.
pub trait ArgSource {
    /// Returns the next argument, or `None` if the arguments are exhausted.
    ///
    /// `is_string` is set for `%s`, in which case the source should resolve the pointer to the
    /// string it points to.
    fn next_arg(&mut self, ty: ArgType, is_string: bool) -> Option<Arg>;
}

#[derive(Debug, Default)]
struct Spec {
    left: bool,
    plus: bool,
    space: bool,
    alt: bool,
    zero: bool,
    width: usize,
    precision: Option<usize>,
    length: Length,
    conversion: char,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Length {
    #[default]
    None,
    Char,
    Short,
    Long,
    LongLong,
    IntMax,
    Size,
    PtrDiff,
    LongDouble,
}

impl Length {
    fn signed_type(self) -> ArgType {
        match self {
            Length::None | Length::Char | Length::Short | Length::LongDouble => ArgType::Int,
            Length::Long | Length::Size | Length::PtrDiff => ArgType::Long,
            Length::LongLong | Length::IntMax => ArgType::LongLong,
        }
    }

    fn unsigned_type(self) -> ArgType {
        match self.signed_type() {
            ArgType::Long => ArgType::ULong,
            ArgType::LongLong => ArgType::ULongLong,
            _ => ArgType::UInt,
        }
    }
}

/// Parses a conversion specification following a `%`, returning it and its length in bytes.
///
/// `*` width and precision are resolved by pulling arguments from `args`.
fn parse_spec(s: &str, args: &mut impl ArgSource) -> Option<(Spec, usize)> {
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut spec = Spec::default();

    while let Some(&b) = bytes.get(i) {
        match b {
            b'-' => spec.left = true,
            b'+' => spec.plus = true,
            b' ' => spec.space = true,
            b'#' => spec.alt = true,
            b'0' => spec.zero = true,
            _ => break,
        }
        i += 1;
    }

    let mut read_number = |i: &mut usize| -> Option<usize> {
        if bytes.get(*i) == Some(&b'*') {
            *i += 1;
            return match args.next_arg(ArgType::Int, false)? {
                Arg::Signed(v) => Some(v.max(0) as usize),
                _ => None,
            };
        }
        let start = *i;
        while bytes.get(*i).is_some_and(u8::is_ascii_digit) {
            *i += 1;
        }
        Some(s[start..*i].parse().unwrap_or(0))
    };

    spec.width = read_number(&mut i)?;
    if bytes.get(i) == Some(&b'.') {
        i += 1;
        spec.precision = Some(read_number(&mut i)?);
    }

    spec.length = match (bytes.get(i), bytes.get(i + 1)) {
        (Some(b'h'), Some(b'h')) => Length::Char,
        (Some(b'h'), _) => Length::Short,
        (Some(b'l'), Some(b'l')) => Length::LongLong,
        (Some(b'l'), _) => Length::Long,
        (Some(b'j'), _) => Length::IntMax,
        (Some(b'z'), _) => Length::Size,
        (Some(b't'), _) => Length::PtrDiff,
        (Some(b'L'), _) => Length::LongDouble,
        _ => Length::None,
    };
    i += match spec.length {
        Length::None => 0,
        Length::Char | Length::LongLong => 2,
        _ => 1,
    };

    spec.conversion = char::from(*bytes.get(i)?);

    Some((spec, i + 1))
}

/// Formats `fmt` with arguments from `args`, following C `printf` semantics.
pub fn format(fmt: &str, args: &mut impl ArgSource) -> String {
    let mut out = String::with_capacity(fmt.len());
    let mut rest = fmt;

    while let Some(pos) = rest.find('%') {
        out.push_str(&rest[..pos]);
        rest = &rest[pos + 1..];

        if let Some(tail) = rest.strip_prefix('%') {
            out.push('%');
            rest = tail;
            continue;
        }

        let Some((spec, len)) = parse_spec(rest, args) else {
            // Incomplete specification or missing argument, print it verbatim.
            out.push('%');
            out.push_str(rest);
            return out;
        };
        let spec_text = &rest[..len];
        rest = &rest[len..];

        match format_one(&spec, args) {
            Some(formatted) => out.push_str(&formatted),
            None => {
                out.push('%');
                out.push_str(spec_text);
            }
        }
    }

    out.push_str(rest);
    out
}

fn format_one(spec: &Spec, args: &mut impl ArgSource) -> Option<String> {
    match spec.conversion {
        'd' | 'i' => {
            let value = match args.next_arg(spec.length.signed_type(), false)? {
                Arg::Signed(v) => match spec.length {
                    Length::Char => i64::from(v as i8),
                    Length::Short => i64::from(v as i16),
                    _ => v,
                },
                _ => return None,
            };
            let sign = if value < 0 {
                "-"
            } else if spec.plus {
                "+"
            } else if spec.space {
                " "
            } else {
                ""
            };
            Some(pad_number(
                spec,
                sign,
                &int_digits(value.unsigned_abs().to_string(), value == 0, spec),
            ))
        }
        'u' | 'o' | 'x' | 'X' => {
            let value = match args.next_arg(spec.length.unsigned_type(), false)? {
                Arg::Unsigned(v) => match spec.length {
                    Length::Char => u64::from(v as u8),
                    Length::Short => u64::from(v as u16),
                    _ => v,
                },
                _ => return None,
            };
            let digits = match spec.conversion {
                'u' => value.to_string(),
                'o' => {
                    let digits = format!("{value:o}");
                    if spec.alt && !digits.starts_with('0') {
                        format!("0{digits}")
                    } else {
                        digits
                    }
                }
                'x' => format!("{value:x}"),
                _ => format!("{value:X}"),
            };
            let prefix = match spec.conversion {
                'x' if spec.alt && value != 0 => "0x",
                'X' if spec.alt && value != 0 => "0X",
                _ => "",
            };
            Some(pad_number(
                spec,
                prefix,
                &int_digits(digits, value == 0, spec),
            ))
        }
        'c' => {
            let value = match args.next_arg(ArgType::Int, false)? {
                Arg::Signed(v) => v as u8,
                Arg::Unsigned(v) => v as u8,
                _ => return None,
            };
            Some(pad(spec, &char::from(value).to_string()))
        }
        's' => {
            let Arg::Str(s) = args.next_arg(ArgType::Ptr, true)? else {
                return None;
            };
            let s = match spec.precision {
                Some(precision) => s.chars().take(precision).collect(),
                None => s,
            };
            Some(pad(spec, &s))
        }
        'p' => {
            let Arg::Unsigned(value) = args.next_arg(ArgType::Ptr, false)? else {
                return None;
            };
            Some(pad(spec, &format!("0x{value:x}")))
        }
        'n' => {
            args.next_arg(ArgType::Ptr, false)?;
            Some(String::new())
        }
        'f' | 'F' | 'e' | 'E' | 'g' | 'G' | 'a' | 'A' => {
            let ty = if spec.length == Length::LongDouble {
                ArgType::LongDouble
            } else {
                ArgType::Double
            };
            let Arg::Float(value) = args.next_arg(ty, false)? else {
                return None;
            };
            Some(format_float(spec, value))
        }
        _ => None,
    }
}

/// Applies the precision (minimum number of digits) to the digits of an integer.
fn int_digits(digits: String, is_zero: bool, spec: &Spec) -> String {
    match spec.precision {
        Some(0) if is_zero => String::new(),
        Some(precision) if digits.len() < precision => {
            format!("{}{digits}", "0".repeat(precision - digits.len()))
        }
        _ => digits,
    }
}

/// Pads a number consisting of a sign or prefix and digits to the field width.
fn pad_number(spec: &Spec, prefix: &str, digits: &str) -> String {
    let len = prefix.len() + digits.len();
    if spec.width <= len {
        return format!("{prefix}{digits}");
    }

    let fill = spec.width - len;
    let zero_pad = spec.zero
        && !spec.left
        && (spec.precision.is_none()
            || matches!(
                spec.conversion,
                'f' | 'F' | 'e' | 'E' | 'g' | 'G' | 'a' | 'A'
            ));

    if spec.left {
        format!("{prefix}{digits}{}", " ".repeat(fill))
    } else if zero_pad {
        format!("{prefix}{}{digits}", "0".repeat(fill))
    } else {
        format!("{}{prefix}{digits}", " ".repeat(fill))
    }
}

fn pad(spec: &Spec, s: &str) -> String {
    let len = s.chars().count();
    if spec.width <= len {
        s.to_string()
    } else if spec.left {
        format!("{s}{}", " ".repeat(spec.width - len))
    } else {
        format!("{}{s}", " ".repeat(spec.width - len))
    }
}

fn format_float(spec: &Spec, value: f64) -> String {
    let upper = spec.conversion.is_ascii_uppercase();
    let sign = if value.is_sign_negative() && !value.is_nan() {
        "-"
    } else if spec.plus {
        "+"
    } else if spec.space {
        " "
    } else {
        ""
    };
    let abs = value.abs();

    if !abs.is_finite() {
        let body = if abs.is_nan() { "nan" } else { "inf" };
        let body = if upper {
            body.to_uppercase()
        } else {
            body.to_string()
        };
        // Zero padding does not apply to infinity and NaN.
        let spec = Spec {
            zero: false,
            ..*spec
        };
        return pad_number(&spec, sign, &body);
    }

    let precision = spec.precision.unwrap_or(6);
    let body = match spec.conversion.to_ascii_lowercase() {
        'f' => format_fixed(abs, precision, spec.alt),
        'e' => format_exponent(abs, precision, spec.alt),
        'g' => format_general(abs, precision, spec.alt),
        _ => format_hex_float(abs, spec.precision),
    };
    let body = if upper { body.to_uppercase() } else { body };

    pad_number(spec, sign, &body)
}

fn format_fixed(abs: f64, precision: usize, alt: bool) -> String {
    let mut s = format!("{abs:.precision$}");
    if alt && precision == 0 {
        s.push('.');
    }
    s
}

/// Returns the mantissa and exponent of `abs` in scientific notation with `precision` digits.
fn split_exponent(abs: f64, precision: usize) -> (String, i32) {
    let s = format!("{abs:.precision$e}");
    let (mantissa, exponent) = s.split_once('e').expect("exponent formatting has an 'e'");
    (mantissa.to_string(), exponent.parse().unwrap_or(0))
}

fn format_exponent(abs: f64, precision: usize, alt: bool) -> String {
    let (mut mantissa, exponent) = split_exponent(abs, precision);
    if alt && precision == 0 {
        mantissa.push('.');
    }
    let exp_sign = if exponent < 0 { '-' } else { '+' };
    format!("{mantissa}e{exp_sign}{:02}", exponent.unsigned_abs())
}

fn format_general(abs: f64, precision: usize, alt: bool) -> String {
    let precision = precision.max(1);
    let exponent = if abs == 0.0 {
        0
    } else {
        split_exponent(abs, precision - 1).1
    };

    let mut s = if exponent >= -4 && exponent < precision as i32 {
        format_fixed(abs, (precision as i32 - 1 - exponent) as usize, alt)
    } else {
        format_exponent(abs, precision - 1, alt)
    };

    if !alt {
        let (number, exp) = match s.find('e') {
            Some(pos) => s.split_at(pos),
            None => (s.as_str(), ""),
        };
        if number.contains('.') {
            let number = number.trim_end_matches('0').trim_end_matches('.');
            s = format!("{number}{exp}");
        }
    }
    s
}

fn format_hex_float(abs: f64, precision: Option<usize>) -> String {
    if abs == 0.0 {
        return match precision {
            Some(p) if p > 0 => format!("0x0.{}p+0", "0".repeat(p)),
            _ => "0x0p+0".to_string(),
        };
    }

    let bits = abs.to_bits();
    let biased = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & ((1 << 52) - 1);
    let (lead, exponent) = if biased == 0 {
        (0, -1022)
    } else {
        (1, biased - 1023)
    };

    let mut digits = format!("{fraction:013x}");
    match precision {
        Some(p) => digits.truncate(p),
        None => digits.truncate(digits.trim_end_matches('0').len()),
    }
    if let Some(p) = precision {
        while digits.len() < p {
            digits.push('0');
        }
    }

    let dot = if digits.is_empty() { "" } else { "." };
    format!("0x{lead}{dot}{digits}p{exponent:+}")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Args(std::vec::IntoIter<Arg>);

    impl ArgSource for Args {
        fn next_arg(&mut self, _ty: ArgType, _is_string: bool) -> Option<Arg> {
            self.0.next()
        }
    }

    fn f(fmt: &str, args: Vec<Arg>) -> String {
        format(fmt, &mut Args(args.into_iter()))
    }

    use Arg::{Float, Signed, Str, Unsigned};

    #[test]
    fn integers() {
        assert_eq!(f("%d %i", vec![Signed(-5), Signed(7)]), "-5 7");
        assert_eq!(
            f("%5d|%-5d|%05d", vec![Signed(42), Signed(42), Signed(-42)]),
            "   42|42   |-0042"
        );
        assert_eq!(f("%+d % d", vec![Signed(3), Signed(3)]), "+3  3");
        assert_eq!(f("%.3d|%8.3d", vec![Signed(7), Signed(-7)]), "007|    -007");
        assert_eq!(
            f("%hhd %hd", vec![Signed(0x1ff), Signed(0x18000)]),
            "-1 -32768"
        );
        assert_eq!(
            f(
                "%u %lu %llu",
                vec![Unsigned(1), Unsigned(2), Unsigned(u64::MAX)]
            ),
            "1 2 18446744073709551615"
        );
        assert_eq!(
            f(
                "%x %X %#x %#X %#x",
                vec![
                    Unsigned(255),
                    Unsigned(255),
                    Unsigned(255),
                    Unsigned(255),
                    Unsigned(0)
                ]
            ),
            "ff FF 0xff 0XFF 0"
        );
        assert_eq!(
            f("%08x %#010x", vec![Unsigned(0xbeef), Unsigned(0xbeef)]),
            "0000beef 0x0000beef"
        );
        assert_eq!(f("%o %#o", vec![Unsigned(8), Unsigned(8)]), "10 010");
        assert_eq!(
            f("%zu %jd %td", vec![Unsigned(9), Signed(-9), Signed(4)]),
            "9 -9 4"
        );
        assert_eq!(f("%.0d|", vec![Signed(0)]), "|");
    }

    #[test]
    fn chars_strings_pointers() {
        assert_eq!(f("%c%c", vec![Signed(0x41), Unsigned(0x42)]), "AB");
        assert_eq!(
            f(
                "[%s] [%6s] [%-6s] [%.2s]",
                vec![
                    Str("abc".into()),
                    Str("abc".into()),
                    Str("abc".into()),
                    Str("abc".into())
                ]
            ),
            "[abc] [   abc] [abc   ] [ab]"
        );
        assert_eq!(f("%p", vec![Unsigned(0x2000_1000)]), "0x20001000");
        assert_eq!(f("100%% %n!", vec![Unsigned(0)]), "100% !");
    }

    #[test]
    fn star_width_and_precision() {
        assert_eq!(
            f(
                "%*d|%-*d|%.*s",
                vec![
                    Signed(4),
                    Signed(1),
                    Signed(3),
                    Signed(2),
                    Signed(2),
                    Str("xyz".into())
                ]
            ),
            "   1|2  |xy"
        );
    }

    #[test]
    fn floats() {
        assert_eq!(
            f(
                "%f %.2f %.0f %#.0f",
                vec![Float(1.5), Float(1.23456), Float(2.5), Float(3.0)]
            ),
            "1.500000 1.23 2 3."
        );
        assert_eq!(
            f(
                "%8.3f|%-8.3f|%08.3f|%+.1f",
                vec![Float(-1.5), Float(1.5), Float(-1.5), Float(1.0)]
            ),
            "  -1.500|1.500   |-001.500|+1.0"
        );
        assert_eq!(
            f(
                "%e %.2E %e",
                vec![Float(12345.678), Float(0.000123), Float(0.0)]
            ),
            "1.234568e+04 1.23E-04 0.000000e+00"
        );
        assert_eq!(
            f(
                "%g %g %g %g %g",
                vec![
                    Float(100000.0),
                    Float(1000000.0),
                    Float(0.0001),
                    Float(0.00001),
                    Float(1.5)
                ]
            ),
            "100000 1e+06 0.0001 1e-05 1.5"
        );
        assert_eq!(
            f("%G %#g %g", vec![Float(1e-10), Float(1.0), Float(0.0)]),
            "1E-10 1.00000 0"
        );
        assert_eq!(
            f(
                "%f %F %5f %e",
                vec![
                    Float(f64::INFINITY),
                    Float(f64::NEG_INFINITY),
                    Float(f64::NAN),
                    Float(f64::NAN)
                ]
            ),
            "inf -INF   nan nan"
        );
        assert_eq!(
            f(
                "%a %a %A %.1a",
                vec![Float(1.0), Float(0.5), Float(255.0), Float(1.0)]
            ),
            "0x1p+0 0x1p-1 0X1.FEP+7 0x1.0p+0"
        );
    }

    #[test]
    fn malformed() {
        assert_eq!(f("trailing %", vec![]), "trailing %");
        assert_eq!(f("missing %d arg", vec![]), "missing %d arg");
        assert_eq!(f("unknown %y conv", vec![]), "unknown %y conv");
    }
}
