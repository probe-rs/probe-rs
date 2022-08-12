use std::borrow::Cow;

use colored::Colorize;

#[derive(Debug, Eq, PartialEq)]
pub enum Toolchain<'p> {
    One52(One52<'p>),
    Verbatim(&'p str),
}

impl<'p> Toolchain<'p> {
    pub fn from_str(input: &str) -> Toolchain {
        if let Some(toolchain) = One52::from_str(input) {
            Toolchain::One52(toolchain)
        } else {
            Toolchain::Verbatim(input)
        }
    }

    pub fn format_highlight(&self) -> Cow<str> {
        match self {
            Toolchain::One52(toolchain) => toolchain.format_highlight().into(),
            Toolchain::Verbatim(toolchain) => Cow::Borrowed(toolchain),
        }
    }

    pub fn format_short(&self) -> Cow<str> {
        match self {
            Toolchain::One52(toolchain) => toolchain.format_short(),
            Toolchain::Verbatim(toolchain) => Cow::Borrowed(toolchain),
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct One52<'p> {
    pub channel: Channel<'p>,
    pub host: &'p str,
}

impl<'p> One52<'p> {
    fn from_str(input: &'p str) -> Option<Self> {
        let (maybe_channel, rest) = input.split_once('-')?;

        Some(if maybe_channel == "stable" {
            One52 {
                channel: Channel::Stable,
                host: rest,
            }
        } else if maybe_channel == "beta" {
            One52 {
                channel: Channel::Beta,
                host: rest,
            }
        } else if maybe_channel == "nightly" {
            let maybe_year = rest.split('-').next()?;

            if maybe_year.starts_with("20") {
                let mut count = 0;
                let at_third_dash = |c: char| {
                    c == '-' && {
                        count += 1;
                        count == 3
                    }
                };

                let mut parts = rest.split(at_third_dash);

                let date = parts.next()?;
                let host = parts.next()?;
                One52 {
                    channel: Channel::Nightly { date: Some(date) },
                    host,
                }
            } else {
                One52 {
                    channel: Channel::Nightly { date: None },
                    host: rest,
                }
            }
        } else if maybe_channel.contains('.') {
            One52 {
                channel: Channel::Version(maybe_channel),
                host: rest,
            }
        } else {
            return None;
        })
    }

    fn format_highlight(&self) -> String {
        format!(
            "{}{}{}",
            self.format_short().bold(),
            "-".dimmed(),
            self.host.dimmed()
        )
    }

    fn format_short(&self) -> Cow<str> {
        match self.channel {
            Channel::Beta => "beta".into(),
            Channel::Nightly { date } => {
                if let Some(date) = date {
                    format!("nightly-{}", date).into()
                } else {
                    "nightly".into()
                }
            }
            Channel::Stable => "stable".into(),
            Channel::Version(version) => version.into(),
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum Channel<'p> {
    Beta,
    Nightly { date: Option<&'p str> },
    Stable,
    Version(&'p str),
}

#[cfg(test)]
mod tests {
    use super::*;

    use rstest::rstest;

    #[rstest]
    #[case("stable-x86_64-unknown-linux-gnu", Channel::Stable, "stable")]
    #[case("beta-x86_64-unknown-linux-gnu", Channel::Beta, "beta")]
    #[case("nightly-x86_64-unknown-linux-gnu", Channel::Nightly { date: None }, "nightly")]
    #[case(
        "nightly-2021-05-01-x86_64-unknown-linux-gnu",
        Channel::Nightly { date: Some("2021-05-01") },
        "nightly-2021-05-01",
    )]
    #[case(
        "1.52.1-x86_64-unknown-linux-gnu",
        Channel::Version("1.52.1"),
        "1.52.1"
    )]
    fn end_to_end(
        #[case] input: &str,
        #[case] channel: Channel,
        #[case] expected_short_format: &str,
    ) {
        let toolchain = Toolchain::from_str(input);
        let expected = Toolchain::One52(One52 {
            channel,
            host: "x86_64-unknown-linux-gnu",
        });

        assert_eq!(expected, toolchain);

        let formatted_string = toolchain.format_short();

        assert_eq!(expected_short_format, formatted_string);
    }
}
