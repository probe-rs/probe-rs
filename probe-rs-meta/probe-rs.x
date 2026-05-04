SECTIONS
{
  .probe-rs.chip (INFO) :
  {
    KEEP(*(.probe-rs.chip));
  }
  .probe-rs.timeout (INFO) :
  {
    KEEP(*(.probe-rs.timeout));
  }
}
