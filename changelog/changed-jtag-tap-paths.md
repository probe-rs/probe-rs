JTAG now uses one fixed path between two TAP states. A transfer now ends in Run-Test/Idle instead of Update-DR, which adds one clock when the target asks for no idle cycles.
