# RPM packaging

GitHub Actions stages the binary, documentation, man page and completions, then creates architecture-
specific RPM packages with `fpm`. The generated package is installed and smoke-tested on a native
runner before it can become a release asset.
