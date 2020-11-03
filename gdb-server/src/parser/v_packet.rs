use super::{query::pid, Pid};
use nom::{
    branch::alt, bytes::complete::tag, character::complete::char, combinator::value, IResult,
};

#[derive(Debug, PartialEq, Clone)]
pub enum VPacket {
    Attach(Pid),
    Continue(Action),
    Unknown(Vec<u8>),
    QueryContSupport,
}

#[allow(dead_code)]
#[derive(Debug, PartialEq, Clone)]
pub enum Action {
    Continue,
    ContinueSignal(u8),
    Step,
    StepSignal,
    Stop,
    RangeStep { start: u32, end: u32 },
}

pub fn v_packet(input: &[u8]) -> IResult<&[u8], VPacket> {
    let parse_result = alt((v_attach, v_cont_support, v_cont))(input);

    match parse_result {
        Ok((input, packet)) => Ok((input, packet)),
        Err(nom::Err::Error(nom::error::Error { input, .. })) => {
            // For unknown packets, we have to return a valid packet here.
            // This is requird by the GDB spec.
            Ok((b"", VPacket::Unknown(input.to_owned())))
        }
        Err(other) => Err(other),
    }
}

fn v_attach(input: &[u8]) -> IResult<&[u8], VPacket> {
    let (input, _) = tag("Attach;")(input)?;

    let (input, pid) = pid(input)?;

    Ok((input, VPacket::Attach(pid)))
}

fn v_cont_support(input: &[u8]) -> IResult<&[u8], VPacket> {
    let (input, _) = tag("Cont?")(input)?;

    Ok((input, VPacket::QueryContSupport))
}

fn v_cont(input: &[u8]) -> IResult<&[u8], VPacket> {
    let (input, _) = tag("Cont;")(input)?;

    let (input, action) = v_cont_action(input)?;

    Ok((input, VPacket::Continue(action)))
}

fn v_cont_action(input: &[u8]) -> IResult<&[u8], Action> {
    alt((
        value(Action::Continue, char('c')),
        value(Action::Step, char('s')),
        value(Action::Stop, char('t')),
    ))(input)
}

#[cfg(test)]
mod test {
    use super::*;

    const EMPTY: &[u8] = &[];

    #[test]
    fn parse_v_attach() {
        assert_eq!(v_packet(b"Attach;7").unwrap(), (EMPTY, VPacket::Attach(7)));
    }

    #[test]
    fn parse_v_cont_support() {
        assert_eq!(
            v_packet(b"Cont?").unwrap(),
            (EMPTY, VPacket::QueryContSupport)
        );
    }

    #[test]
    fn parse_v_cont_cont() {
        assert_eq!(
            v_packet(b"Cont;c").unwrap(),
            (EMPTY, VPacket::Continue(Action::Continue))
        );
    }

    #[test]
    fn parse_v_cont_step() {
        assert_eq!(
            v_packet(b"Cont;s").unwrap(),
            (EMPTY, VPacket::Continue(Action::Step))
        );
    }

    #[test]
    fn parse_v_cont_stop() {
        assert_eq!(
            v_packet(b"Cont;t").unwrap(),
            (EMPTY, VPacket::Continue(Action::Stop))
        );
    }
}
