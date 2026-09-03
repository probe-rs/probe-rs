Fixed a CMSIS-DAP probe being reported as not supporting SWD after an earlier session left it with
replies outstanding. Every `DAP_Info` sub-command shares one command ID and the reply does not say
which it answers, so a stale packet count was read as the capability mask.
