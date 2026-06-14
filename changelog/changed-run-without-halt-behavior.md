When executing the "run" command we originally did a step to move past any breakpoints, but if you attempt to continue and the core is already running, this fails.

Add in a check to skipt the step if the core is not halted when "run" is executed.