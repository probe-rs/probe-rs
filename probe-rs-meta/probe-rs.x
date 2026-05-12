SECTIONS
{
  .probe-rs.version (INFO) :
  {
    KEEP(*(.probe-rs.version));
  }
  .probe-rs.chip (INFO) :
  {
    KEEP(*(.probe-rs.chip));
  }
  .probe-rs.timeout (INFO) :
  {
    KEEP(*(.probe-rs.timeout));
  }
}
