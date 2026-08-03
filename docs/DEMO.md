# Demo captures

Run:

```sh
scripts/capture-demo.sh
```

This builds `lens-top`, uses the deterministic `--demo` source and writes plain and JSON captures under
`dist/demo/`. For an interactive recording, run `lens-top --demo` inside your preferred terminal
recorder at 120x32. Demo mode never reads host `/proc`, so documentation remains reproducible.
