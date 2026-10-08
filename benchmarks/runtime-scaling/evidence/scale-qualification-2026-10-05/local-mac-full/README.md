# Local Apple M5 Pro full soak

The soak completed 10,807 seconds and 11,402,819 main-domain operations at
500,000 main live rows. The final three-domain audit and `soak storage_slice=passed`
are in the raw `soak.txt`; `profile.json` records process exit 0 and the source,
binary, hardware and toolchain provenance. `audit.txt` extracts the final audit
lines from the raw output; it is not an independent invocation.

The launchd wrapper was configured to relaunch after exit. Its first run
completed successfully, then repeated attempts failed because the fresh-output
directory already existed and overwrote the wrapper status file. `wrapper.log`
records those retries. The captured qualification result is from the completed
manifest (`profile.json` exit 0) and final audit, not from the wrapper's final
retry status. The wrapper is no longer loaded and no benchmark process remains.
