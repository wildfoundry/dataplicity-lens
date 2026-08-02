# Linux fixtures

These files are deliberately small, deterministic fragments of Linux kernel interfaces. Parser tests
exercise normal process names, kernel-thread style empty command lines, zombies, large counters,
cgroups, container-like cgroups, malformed input and Unicode command lines without depending on the
GitHub runner's live `/proc` state.
