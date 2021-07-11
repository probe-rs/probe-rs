#[macro_export]
macro_rules! print_dut_status {
    ($tracker:expr, $color:ident,$($arg:tt)*) => ({
        let prefix = format!("[{}/{}]({})", $tracker.current_dut(), $tracker.num_duts(), $tracker.current_dut_name()).$color();
        print!("{} - ", prefix);
        print!($($arg)*);
    })
}

#[macro_export]
macro_rules! print_test_status {
    ($tracker:expr, $color:ident,$($arg:tt)*) => ({
        let prefix = format!("[{}/{}]({})", $tracker.current_dut(), $tracker.num_duts(), $tracker.current_dut_name()).$color();
        print!("{} - ", prefix);
        print!($($arg)*);
    })
}

#[macro_export]
macro_rules! println_status {
    ($tracker:expr, $color:ident,$($arg:tt)*) => ({
        let prefix = format!("[{}]", "DONE".$color());
        print!("{} - ", prefix);
        println!($($arg)*);
    })
}

#[macro_export]
macro_rules! println_dut_status {
    ($tracker:expr, $color:ident,$($arg:tt)*) => ({
        let prefix = format!("[{}/{}]({})", $tracker.current_dut(), $tracker.num_duts(), $tracker.current_dut_name()).$color();
        print!("{} - ", prefix);
        println!($($arg)*);
    })
}

#[macro_export]
macro_rules! println_test_status {
    ($tracker:expr, $color:ident,$($arg:tt)*) => ({
        let prefix = format!("[{}/{}]({}) - Test [{}/{}]", $tracker.current_dut(), $tracker.num_duts(), $tracker.current_dut_name(), $tracker.current_test(), $tracker.num_tests()).$color();
        print!("{} - ", prefix);
        println!($($arg)*);
    })
}
