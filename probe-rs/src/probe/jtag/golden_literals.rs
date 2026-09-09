// Golden wire sequences recorded from today's helpers.

const MOVE_TEST_LOGIC_RESET_TO_TEST_LOGIC_RESET: (&str, &str, &str) = ("", "", "");
const MOVE_TEST_LOGIC_RESET_TO_RUN_TEST_IDLE: (&str, &str, &str) = ("0", "0", "0");
const MOVE_TEST_LOGIC_RESET_TO_SHIFT_IR: (&str, &str, &str) = ("01100", "00000", "00000");
const MOVE_TEST_LOGIC_RESET_TO_SHIFT_DR: (&str, &str, &str) = ("0100", "0000", "0000");
const MOVE_TEST_LOGIC_RESET_TO_PAUSE_IR: (&str, &str, &str) = ("011010", "000000", "000000");
const MOVE_TEST_LOGIC_RESET_TO_PAUSE_DR: (&str, &str, &str) = ("01010", "00000", "00000");
const MOVE_RUN_TEST_IDLE_TO_TEST_LOGIC_RESET: (&str, &str, &str) = ("111", "000", "000");
const MOVE_RUN_TEST_IDLE_TO_RUN_TEST_IDLE: (&str, &str, &str) = ("", "", "");
const MOVE_RUN_TEST_IDLE_TO_SHIFT_IR: (&str, &str, &str) = ("1100", "0000", "0000");
const MOVE_RUN_TEST_IDLE_TO_SHIFT_DR: (&str, &str, &str) = ("100", "000", "000");
const MOVE_RUN_TEST_IDLE_TO_PAUSE_IR: (&str, &str, &str) = ("11010", "00000", "00000");
const MOVE_RUN_TEST_IDLE_TO_PAUSE_DR: (&str, &str, &str) = ("1010", "0000", "0000");
const MOVE_SHIFT_IR_TO_TEST_LOGIC_RESET: (&str, &str, &str) = ("110111", "000000", "000000");
const MOVE_SHIFT_IR_TO_RUN_TEST_IDLE: (&str, &str, &str) = ("110", "000", "000");
const MOVE_SHIFT_IR_TO_SHIFT_IR: (&str, &str, &str) = ("", "", "");
const MOVE_SHIFT_IR_TO_SHIFT_DR: (&str, &str, &str) = ("11100", "00000", "00000");
const MOVE_SHIFT_IR_TO_PAUSE_IR: (&str, &str, &str) = ("10", "00", "00");
const MOVE_SHIFT_IR_TO_PAUSE_DR: (&str, &str, &str) = ("111010", "000000", "000000");
const MOVE_SHIFT_DR_TO_TEST_LOGIC_RESET: (&str, &str, &str) = ("110111", "000000", "000000");
const MOVE_SHIFT_DR_TO_RUN_TEST_IDLE: (&str, &str, &str) = ("110", "000", "000");
const MOVE_SHIFT_DR_TO_SHIFT_IR: (&str, &str, &str) = ("111100", "000000", "000000");
const MOVE_SHIFT_DR_TO_SHIFT_DR: (&str, &str, &str) = ("", "", "");
const MOVE_SHIFT_DR_TO_PAUSE_IR: (&str, &str, &str) = ("1111010", "0000000", "0000000");
const MOVE_SHIFT_DR_TO_PAUSE_DR: (&str, &str, &str) = ("10", "00", "00");
const MOVE_PAUSE_IR_TO_TEST_LOGIC_RESET: (&str, &str, &str) = ("110111", "000000", "000000");
const MOVE_PAUSE_IR_TO_RUN_TEST_IDLE: (&str, &str, &str) = ("110", "000", "000");
const MOVE_PAUSE_IR_TO_SHIFT_IR: (&str, &str, &str) = ("10", "00", "00");
const MOVE_PAUSE_IR_TO_SHIFT_DR: (&str, &str, &str) = ("11100", "00000", "00000");
const MOVE_PAUSE_IR_TO_PAUSE_IR: (&str, &str, &str) = ("", "", "");
const MOVE_PAUSE_IR_TO_PAUSE_DR: (&str, &str, &str) = ("111010", "000000", "000000");
const MOVE_PAUSE_DR_TO_TEST_LOGIC_RESET: (&str, &str, &str) = ("110111", "000000", "000000");
const MOVE_PAUSE_DR_TO_RUN_TEST_IDLE: (&str, &str, &str) = ("110", "000", "000");
const MOVE_PAUSE_DR_TO_SHIFT_IR: (&str, &str, &str) = ("111100", "000000", "000000");
const MOVE_PAUSE_DR_TO_SHIFT_DR: (&str, &str, &str) = ("10", "00", "00");
const MOVE_PAUSE_DR_TO_PAUSE_IR: (&str, &str, &str) = ("1111010", "0000000", "0000000");
const MOVE_PAUSE_DR_TO_PAUSE_DR: (&str, &str, &str) = ("", "", "");

const SHIFT_IR_ONE_TAP: (&str, &str, &str) = ("011000000110", "000000110100", "000000000000");
const SHIFT_IR_THREE_TAP: (&str, &str, &str) = (
    "011000000000000000110",
    "000001111011011111100",
    "000000000000000000000",
);

const SHIFT_DR_ONE_TAP_ONE: (&str, &str, &str) = ("0100110", "0000100", "0000000");
const SHIFT_DR_THREE_TAP_ONE: (&str, &str, &str) = ("010000110", "000001000", "000000000");
const SHIFT_DR_ONE_TAP_THIRTY_TWO: (&str, &str, &str) = (
    "01000000000000000000000000000000000110",
    "00000001111001101010001011000100100000",
    "00000000000000000000000000000000000000",
);
const SHIFT_DR_THREE_TAP_THIRTY_TWO: (&str, &str, &str) = (
    "0100000000000000000000000000000000000110",
    "0000000011110011010100010110001001000000",
    "0000000000000000000000000000000000000000",
);
const SHIFT_DR_ONE_TAP_FORTY_ONE: (&str, &str, &str) = (
    "01000000000000000000000000000000000000000000110",
    "00001000000001000000110000000010000010100000000",
    "00000000000000000000000000000000000000000000000",
);
const SHIFT_DR_THREE_TAP_FORTY_ONE: (&str, &str, &str) = (
    "0100000000000000000000000000000000000000000000110",
    "0000010000000010000001100000000100000101000000000",
    "0000000000000000000000000000000000000000000000000",
);
const SHIFT_DR_ONE_TAP_SIXTY_FOUR: (&str, &str, &str) = (
    "0100000000000000000000000000000000000000000000000000000000000000000110",
    "0000000100011110111001100110101010100010001011001100010001001000100000",
    "0000000000000000000000000000000000000000000000000000000000000000000000",
);
const SHIFT_DR_THREE_TAP_SIXTY_FOUR: (&str, &str, &str) = (
    "010000000000000000000000000000000000000000000000000000000000000000000110",
    "000000001000111101110011001101010101000100010110011000100010010001000000",
    "000000000000000000000000000000000000000000000000000000000000000000000000",
);

const RESET: (&str, &str, &str) = ("111110", "111111", "000000");
const RESET_TLR_BITS: (&str, &str, &str) = ("11111", "11111", "00000");
const REGISTER_WRITE_EIGHT_IDLE: (&str, &str, &str) = (
    "01100000011100000000000000000000000000000000000000000011000000000",
    "00000011010000100000000100000011000000001000001010000000000000000",
    "00000000000000000000000000000000000000000000000000000000000000000",
);

fn move_literal(from: TapState, to: TapState) -> (&'static str, &'static str, &'static str) {
    match (from, to) {
        (TapState::TestLogicReset, TapState::TestLogicReset) => {
            MOVE_TEST_LOGIC_RESET_TO_TEST_LOGIC_RESET
        }
        (TapState::TestLogicReset, TapState::RunTestIdle) => {
            MOVE_TEST_LOGIC_RESET_TO_RUN_TEST_IDLE
        }
        (TapState::TestLogicReset, TapState::ShiftIr) => MOVE_TEST_LOGIC_RESET_TO_SHIFT_IR,
        (TapState::TestLogicReset, TapState::ShiftDr) => MOVE_TEST_LOGIC_RESET_TO_SHIFT_DR,
        (TapState::TestLogicReset, TapState::PauseIr) => MOVE_TEST_LOGIC_RESET_TO_PAUSE_IR,
        (TapState::TestLogicReset, TapState::PauseDr) => MOVE_TEST_LOGIC_RESET_TO_PAUSE_DR,
        (TapState::RunTestIdle, TapState::TestLogicReset) => MOVE_RUN_TEST_IDLE_TO_TEST_LOGIC_RESET,
        (TapState::RunTestIdle, TapState::RunTestIdle) => MOVE_RUN_TEST_IDLE_TO_RUN_TEST_IDLE,
        (TapState::RunTestIdle, TapState::ShiftIr) => MOVE_RUN_TEST_IDLE_TO_SHIFT_IR,
        (TapState::RunTestIdle, TapState::ShiftDr) => MOVE_RUN_TEST_IDLE_TO_SHIFT_DR,
        (TapState::RunTestIdle, TapState::PauseIr) => MOVE_RUN_TEST_IDLE_TO_PAUSE_IR,
        (TapState::RunTestIdle, TapState::PauseDr) => MOVE_RUN_TEST_IDLE_TO_PAUSE_DR,
        (TapState::ShiftIr, TapState::TestLogicReset) => MOVE_SHIFT_IR_TO_TEST_LOGIC_RESET,
        (TapState::ShiftIr, TapState::RunTestIdle) => MOVE_SHIFT_IR_TO_RUN_TEST_IDLE,
        (TapState::ShiftIr, TapState::ShiftIr) => MOVE_SHIFT_IR_TO_SHIFT_IR,
        (TapState::ShiftIr, TapState::ShiftDr) => MOVE_SHIFT_IR_TO_SHIFT_DR,
        (TapState::ShiftIr, TapState::PauseIr) => MOVE_SHIFT_IR_TO_PAUSE_IR,
        (TapState::ShiftIr, TapState::PauseDr) => MOVE_SHIFT_IR_TO_PAUSE_DR,
        (TapState::ShiftDr, TapState::TestLogicReset) => MOVE_SHIFT_DR_TO_TEST_LOGIC_RESET,
        (TapState::ShiftDr, TapState::RunTestIdle) => MOVE_SHIFT_DR_TO_RUN_TEST_IDLE,
        (TapState::ShiftDr, TapState::ShiftIr) => MOVE_SHIFT_DR_TO_SHIFT_IR,
        (TapState::ShiftDr, TapState::ShiftDr) => MOVE_SHIFT_DR_TO_SHIFT_DR,
        (TapState::ShiftDr, TapState::PauseIr) => MOVE_SHIFT_DR_TO_PAUSE_IR,
        (TapState::ShiftDr, TapState::PauseDr) => MOVE_SHIFT_DR_TO_PAUSE_DR,
        (TapState::PauseIr, TapState::TestLogicReset) => MOVE_PAUSE_IR_TO_TEST_LOGIC_RESET,
        (TapState::PauseIr, TapState::RunTestIdle) => MOVE_PAUSE_IR_TO_RUN_TEST_IDLE,
        (TapState::PauseIr, TapState::ShiftIr) => MOVE_PAUSE_IR_TO_SHIFT_IR,
        (TapState::PauseIr, TapState::ShiftDr) => MOVE_PAUSE_IR_TO_SHIFT_DR,
        (TapState::PauseIr, TapState::PauseIr) => MOVE_PAUSE_IR_TO_PAUSE_IR,
        (TapState::PauseIr, TapState::PauseDr) => MOVE_PAUSE_IR_TO_PAUSE_DR,
        (TapState::PauseDr, TapState::TestLogicReset) => MOVE_PAUSE_DR_TO_TEST_LOGIC_RESET,
        (TapState::PauseDr, TapState::RunTestIdle) => MOVE_PAUSE_DR_TO_RUN_TEST_IDLE,
        (TapState::PauseDr, TapState::ShiftIr) => MOVE_PAUSE_DR_TO_SHIFT_IR,
        (TapState::PauseDr, TapState::ShiftDr) => MOVE_PAUSE_DR_TO_SHIFT_DR,
        (TapState::PauseDr, TapState::PauseIr) => MOVE_PAUSE_DR_TO_PAUSE_IR,
        (TapState::PauseDr, TapState::PauseDr) => MOVE_PAUSE_DR_TO_PAUSE_DR,
    }
}
