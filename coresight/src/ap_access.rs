pub trait APAccess<PORT, REGISTER, VALUE> {
    fn read_register(port: PORT, register: REGISTER) -> VALUE;
    fn write_register(port: PORT, register: REGISTER, value: VALUE);
}