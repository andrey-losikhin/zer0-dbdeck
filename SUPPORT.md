# Support

Use [GitHub Issues](https://github.com/andrey-losikhin/zer0-dbdeck/issues) for bugs
and feature requests. Use [SECURITY.md](SECURITY.md) for vulnerabilities.

## Supported scope

- Linux, primarily Arch/CachyOS and Wayland.
- PostgreSQL, MySQL and MariaDB through SQLx.
- Current Rust stable and the native library requirements in the README.
- The UI is currently Russian; public contribution documentation is English.

Automated local validation has covered PostgreSQL 17.11, MariaDB 11.4.13 and
MySQL 8.4.11. This is a tested baseline, not an exhaustive compatibility matrix.
Native backup tools must match the server family and support its version.
Windows/macOS, built-in SSH tunneling and packaged binary releases are not supported yet.

Include the application commit, distribution, display backend, engine/server
version, safe reproduction steps and relevant sanitized diagnostics. A small
synthetic schema is more useful than a production screenshot or dump.

This is community-supported software maintained as a personal project. There is
no service-level agreement or guaranteed schedule for features and fixes.
