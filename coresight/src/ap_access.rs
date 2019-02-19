pub trait APAccess<PORT, REGISTER, VALUE> {
    type Error;
    fn read_register_ap(&mut self, port: PORT, register: REGISTER) -> Result<VALUE, Self::Error>;
    fn write_register_ap(&mut self, port: PORT, register: REGISTER, value: VALUE) -> Result<(), Self::Error>;
}