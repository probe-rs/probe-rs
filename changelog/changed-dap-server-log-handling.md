* The `probe-rs dap-server` command now handles the `--log-to-folder` and `--log-file` CLI arguments.
* When neither option is supplied, the default behaviour is that logs are written to the DAP client's "Debug Console" window. 
  * In order to avoid adversely affecting the DAP client performance, we will disallow "trace" level logging when sending logs to the Debug Console.