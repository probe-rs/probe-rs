Fixed a CMSIS-DAP probe becoming unusable after a command failed. The probe still held a reply,
and every later command read that reply instead of its own and rejected it as the wrong command,
until the probe was physically reconnected.
